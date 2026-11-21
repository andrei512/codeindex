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
    pub fn declaration(&mut self, id: u64) -> io::Result<Decl> {
        let r = row(&mut self.reader, &self.declarations, id, self.row_end)?;
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
    pub fn get(&mut self, name: &str, limit: usize) -> io::Result<(Vec<Decl>, bool)> {
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
                    let count = n.min(limit as u64) as usize;
                    let mut ids = vec![0; count * 8];
                    self.reader.read(at + 8, &mut ids)?;
                    let mut results = Vec::with_capacity(count);
                    for b in ids.as_chunks::<8>().0 {
                        results.push(self.declaration(u64::from_le_bytes(*b))?);
                    }
                    return Ok((results, n > count as u64));
                }
            }
        }
        Ok((Vec::new(), false))
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
