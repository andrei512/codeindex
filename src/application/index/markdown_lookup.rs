//! Locate a document or a reverse posting list in the pinned Markdown extent.
use super::{
    binary::bad,
    offsets::{read_bytes, row, table, text, word},
};
use crate::domain::{content_lookup::ExtentRead, markdown::Parsed};
use std::{io, ops::Range};
pub struct Lookup {
    docs: Range<u64>,
    destinations: Range<u64>,
    end: u64,
}
impl Lookup {
    pub fn open(mut r: impl ExtentRead) -> io::Result<Self> {
        let footer = r.length().checked_sub(8).ok_or_else(bad)?;
        let end = word(&mut r, footer)?;
        let mut at = end;
        let docs = table(&mut r, &mut at)?;
        let destinations = table(&mut r, &mut at)?;
        if at != footer {
            return Err(bad());
        }
        Ok(Self {
            docs,
            destinations,
            end,
        })
    }
    pub fn document(&self, mut r: impl ExtentRead, id: usize) -> io::Result<Option<Parsed>> {
        let Some(record) = self.document_range(&mut r, id)? else { return Ok(None) };
        let bytes = read_bytes(&mut r, record.start + 8..record.end)?;
        let mut data = bytes.as_slice();
        let doc = Parsed::read_from(&mut data)?;
        if !data.is_empty() {
            return Err(bad());
        }
        Ok(Some(doc))
    }
    pub(crate) fn document_range(&self, r: &mut impl ExtentRead, id: usize) -> io::Result<Option<Range<u64>>> {
        let (mut lo, mut hi) = (0, (self.docs.end - self.docs.start) / 8 - 1);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let record = row(r, &self.docs, mid, self.end)?;
            if record.end - record.start < 8 {
                return Err(bad());
            }
            match word(r, record.start)?.cmp(&(id as u64)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(Some(record)),
            }
        }
        Ok(None)
    }
    fn key(&self, r: &mut impl ExtentRead, id: u64) -> io::Result<(String, String, Range<u64>)> {
        let mut record = row(r, &self.destinations, id, self.end)?;
        let path = text(r, &mut record.start, record.end)?;
        let anchor = text(r, &mut record.start, record.end)?;
        let count = word(r, record.start)?;
        record.start += 8;
        if count
            .checked_mul(16)
            .and_then(|n| n.checked_add(record.start))
            != Some(record.end)
        {
            return Err(bad());
        }
        Ok((path, anchor, record))
    }
    pub fn backlinks(
        &self,
        mut r: impl ExtentRead,
        path: &str,
        anchor: Option<&str>,
    ) -> io::Result<(Vec<(usize, usize)>, bool)> {
        let count = (self.destinations.end - self.destinations.start) / 8 - 1;
        let (mut lo, mut hi) = (0, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (p, a, _) = self.key(&mut r, mid)?;
            if (p.as_str(), a.as_str()) < (path, anchor.unwrap_or("")) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut heap = std::collections::BinaryHeap::new();
        let enqueue = |heap: &mut std::collections::BinaryHeap<_>,
                       range: Range<u64>,
                       r: &mut _|
         -> io::Result<()> {
            if !range.is_empty() {
                let f = word(r, range.start)? as usize;
                let l = word(r, range.start + 8)? as usize;
                heap.push(std::cmp::Reverse((f, l, range.start + 16, range.end)));
            }
            Ok(())
        };
        for id in lo..count {
            let (p, a, range) = self.key(&mut r, id)?;
            if p != path || anchor.is_some_and(|v| v != a) {
                break;
            }
            enqueue(&mut heap, range, &mut r)?;
            if anchor.is_some() {
                break;
            }
        }
        let mut hits = Vec::new();
        while hits.len() < 50 {
            let Some(std::cmp::Reverse((f, l, start, end))) = heap.pop() else {
                break;
            };
            hits.push((f, l));
            enqueue(&mut heap, start..end, &mut r)?;
        }
        Ok((hits, !heap.is_empty()))
    }
}
