//! Locate a document or a reverse posting list in the pinned Markdown extent.
use super::{
    binary::bad,
    offsets::{read_bytes, row, table, word},
};
use crate::domain::{content_lookup::ExtentRead, markdown::{Link, Parsed, Section, read_headings, read_selected_links}};
use std::{io, ops::Range};
mod postings;
pub(crate) use postings::BACKLINK_LIMIT;

pub(super) fn document_body(mut data: &[u8]) -> io::Result<Parsed> {
    let parsed = Parsed::read_from(&mut data)?;
    if !data.is_empty() {
        return Err(bad());
    }
    Ok(parsed)
}
pub(super) fn address(
    r: &mut impl ExtentRead,
    record: &mut Range<u64>,
) -> io::Result<(String, String)> {
    let path = super::offsets::text(r, &mut record.start, record.end)?;
    let anchor = super::offsets::text(r, &mut record.start, record.end)?;
    Ok((path, anchor))
}

pub struct Lookup {
    docs: Range<u64>,
    destinations: Range<u64>,
    end: u64,
}
impl Lookup {
    pub fn open(mut r: impl ExtentRead) -> io::Result<Self> {
        Self::read_directory(&mut r)
    }
    pub(crate) fn read_directory(r: &mut impl ExtentRead) -> io::Result<Self> {
        let footer = r.length().checked_sub(8).ok_or_else(bad)?;
        let end = word(r, footer)?;
        let mut at = end;
        let docs = table(r, &mut at)?;
        let destinations = table(r, &mut at)?;
        if at != footer {
            return Err(bad());
        }
        Ok(Self {
            docs,
            destinations,
            end,
        })
    }
    fn record<R: ExtentRead>(&self, mut reader: R, id: usize) -> io::Result<Record<R>> {
        let range = self.document_range(&mut reader, id)?
            .ok_or_else(|| io::Error::other("not a Markdown document"))?;
        Ok(Record { reader, remaining: range.start + 8..range.end })
    }

    pub(crate) fn headings(&self, reader: impl ExtentRead, id: usize) -> io::Result<Vec<Section>> {
        let (headings, _) = read_headings(self.record(reader, id)?)?;
        Ok(headings)
    }

    pub(crate) fn links(&self, reader: impl ExtentRead, id: usize, ordinals: impl Iterator<Item = usize>)
        -> io::Result<(Vec<Section>, Vec<Link>)> {
        read_selected_links(self.record(reader, id)?, ordinals)
    }

    pub fn document(&self, mut r: impl ExtentRead, id: usize) -> io::Result<Option<Parsed>> {
        let Some(record) = self.document_range(&mut r, id)? else { return Ok(None) };
        let bytes = read_bytes(&mut r, record.start + 8..record.end)?;
        document_body(&bytes).map(Some)
    }
    pub(crate) fn document_range(&self, r: &mut impl ExtentRead, id: usize) -> io::Result<Option<Range<u64>>> {
        let (mut lo, mut hi) = (0, (self.docs.end - self.docs.start) / 8 - 1);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let record = row(r, &self.docs, mid, self.end)?;
            match document_id(r, &record)?.cmp(&(id as u64)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(Some(record)),
            }
        }
        Ok(None)
    }

}

fn document_id(r: &mut impl ExtentRead, record: &Range<u64>) -> io::Result<u64> {
    if record.end - record.start < 8 {
        return Err(bad());
    }
    word(r, record.start)
}

/// Sequential reads from one record share the extent cursor's existing read-ahead.
struct Record<R> {
    reader: R,
    remaining: Range<u64>,
}

impl<R: ExtentRead> io::Read for Record<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (self.remaining.end - self.remaining.start).min(buf.len() as u64) as usize;
        self.reader.read(self.remaining.start, &mut buf[..n])?;
        self.remaining.start += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_and_address_decoders_respect_the_record_boundary() {
        let doc = Parsed::parse("doc.md", "# Title\n[Next](other.md)\n");
        let mut wire = Vec::new();
        doc.write_to(&mut wire).unwrap();
        assert!(document_body(&wire).is_ok());
        assert!(document_body(&wire[..wire.len() - 1]).is_err());
        wire.push(0);
        assert!(document_body(&wire).is_err());
        let mut bytes = Vec::new();
        super::super::binary::string(&mut bytes, "other.md").unwrap();
        super::super::binary::string(&mut bytes, "anchor").unwrap();
        let end = bytes.len() as u64;
        let mut reader = io::Cursor::new(bytes);
        let mut range = 0..end;
        assert_eq!(
            address(&mut reader, &mut range).unwrap(),
            ("other.md".into(), "anchor".into())
        );
        assert_eq!(range.start, end);
        assert!(address(&mut reader, &mut (0..end - 1)).is_err());
    }
}
