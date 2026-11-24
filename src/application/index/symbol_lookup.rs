//! The two offset tables address individual declaration and name rows.
//! Name lookup probes spellings and reads only the returned declaration rows.
use super::offsets::{read_bytes, row, table, text, word};
use super::{binary::bad, graph::Decl};
use crate::domain::content_lookup::ExtentRead;
use std::{io, ops::Range};

pub struct SymbolLookup<R> {
    reader: R,
    declarations: Range<u64>,
    names: Range<u64>,
    row_end: u64,
    file_count: usize,
    labels: Range<u64>,
}
impl<R: ExtentRead> SymbolLookup<R> {
    pub fn open(mut reader: R, file_count: usize) -> io::Result<Self> {
        let footer = reader.length().checked_sub(8).ok_or_else(bad)?;
        let row_end = word(&mut reader, footer)?;
        let labels = super::public_labels::directory(&mut reader, row_end)?;
        let mut at = row_end;
        let declarations = table(&mut reader, &mut at)?;
        let names = table(&mut reader, &mut at)?;
        if at != footer {
            return Err(bad());
        }
        Ok(Self {
            reader,
            labels,
            declarations,
            names,
            row_end,
            file_count,
        })
    }
    pub fn count(&self) -> usize {
        ((self.declarations.end - self.declarations.start) / 8 - 1) as usize
    }
    /// Fixed-width end coordinate of the last declaration owned by this file.
    pub(crate) fn source_end_word(&mut self, file: usize, end: usize) -> io::Result<Option<u64>> {
        let (mut lo, mut hi) = (0, self.count() as u64);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let pos = word(&mut self.reader, self.declarations.start + mid * 8)?;
            if pos >= self.row_end { return Err(bad()); }
            if word(&mut self.reader, pos)? <= file as u64 { lo = mid + 1; } else { hi = mid; }
        }
        let Some(id) = lo.checked_sub(1) else { return Ok(None) };
        let range = row(&mut self.reader, &self.declarations, id, self.row_end)?;
        let decl = self.decode_declaration(range.clone())?;
        Ok((decl.file == file && decl.kind == "source_file" && decl.span == (0..end))
            .then_some(range.start + 16))
    }
    pub fn declaration(&mut self, id: u64) -> io::Result<Decl> {
        let r = row(&mut self.reader, &self.declarations, id, self.row_end)?;
        self.decode_declaration(r)
    }
    fn decode_declaration(&mut self, r: Range<u64>) -> io::Result<Decl> {
        let bytes = read_bytes(&mut self.reader, r)?;
        let mut d = super::binary::Decoder(&bytes);
        let item = super::graph::decode_decl(&mut d, self.file_count)?;
        d.end()?;
        Ok(item)
    }
    pub fn semantic_declaration(&mut self, id: u64) -> io::Result<Decl> {
        let mut d = self.declaration(id)?;
        d.label = super::public_labels::lookup(&mut self.reader, &self.labels, id)?;
        Ok(d)
    }
    pub fn visit_file(
        &mut self,
        file: usize,
        mut visit: impl FnMut(usize, Decl),
    ) -> io::Result<()> {
        let (first, bytes) = file_bytes(&mut self.reader, &self.declarations, self.row_end, file)?;
        let mut d = super::binary::Decoder(&bytes);
        let mut id = first;
        while !d.0.is_empty() {
            let item = super::graph::decode_decl(&mut d, self.file_count)?;
            if item.file != file {
                return Err(bad());
            }
            visit(id, item);
            id += 1;
        }
        Ok(())
    }
    fn name_row(&mut self, name: &str) -> io::Result<Range<u64>> {
        let mut low = 0;
        let mut high = (self.names.end - self.names.start) / 8 - 1;
        while low < high {
            let mid = low + (high - low) / 2;
            let r = row(&mut self.reader, &self.names, mid, self.row_end)?;
            let mut at = r.start;
            match text(&mut self.reader, &mut at, r.end)?.as_str().cmp(name) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => {
                    let n = word(&mut self.reader, at)?;
                    if at
                        .checked_add(8)
                        .and_then(|x| n.checked_mul(8)?.checked_add(x))
                        != Some(r.end)
                    {
                        return Err(bad());
                    }
                    return Ok(at + 8..r.end);
                }
            }
        }
        Ok(0..0)
    }

    pub fn get(&mut self, name: &str, limit: usize) -> io::Result<(Vec<Decl>, bool)> {
        let row = self.name_row(name)?;
        let count = (row.end - row.start) / 8;
        let mut ids = vec![0; count.min(limit as u64) as usize * 8];
        if !ids.is_empty() {
            self.reader.read(row.start, &mut ids)?;
        }
        let mut results = Vec::with_capacity(ids.len() / 8);
        for b in ids.as_chunks::<8>().0 {
            results.push(self.declaration(u64::from_le_bytes(*b))?);
        }
        Ok((results, count > limit as u64))
    }

    /// Stop at the second match. An ambiguous name requires a source position.
    pub fn named_target(&mut self, input: &str) -> io::Result<(usize, Decl)> {
        let (owner, name) = match input.rsplit_once("::") {
            Some((owner, method)) => (Some(format!("impl#[{owner}]{method}().")), method),
            None => (None, input),
        };
        let row = self.name_row(name)?;
        let mut found = None;
        let mut at = row.start;
        let mut buf = [0; 256];
        while at < row.end {
            let len = (row.end - at).min(buf.len() as u64) as usize;
            self.reader.read(at, &mut buf[..len])?;
            at += len as u64;
            for bytes in buf[..len].as_chunks::<8>().0 {
                let id = u64::from_le_bytes(*bytes);
                let decl = self.semantic_declaration(id)?;
                if decl.kind == "source_file"
                    || owner.as_ref().is_some_and(|suffix| {
                        !decl
                            .label
                            .as_deref()
                            .and_then(|label| label.rsplit(['/', ' ']).next())
                            .is_some_and(|last| last == suffix)
                    })
                {
                    continue;
                }
                if found.is_some() {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("ambiguous declaration {input}")));
                }
                found = Some((id as usize, decl));
            }
        }
        found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no matching indexed declaration for {input}; use cx --symbol {name} or supply FILE LINE COL")))
    }
}

pub(super) fn file_bytes(
    r: &mut impl ExtentRead,
    index: &Range<u64>,
    end: u64,
    file: usize,
) -> io::Result<(usize, Vec<u8>)> {
    let count = (index.end - index.start) / 8 - 1;
    let mut lower = |file: usize| -> io::Result<u64> {
        let (mut lo, mut hi) = (0, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let pos = word(r, index.start + mid * 8)?;
            if pos >= end {
                return Err(bad());
            }
            if word(r, pos)? < file as u64 {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    };
    let a = lower(file)?;
    let b = lower(file + 1)?;
    let from = word(r, index.start + a * 8)?;
    let to = word(r, index.start + b * 8)?;
    if from > to || to > end {
        return Err(bad());
    }
    Ok((a as usize, read_bytes(r, from..to)?))
}
