//! Query-local relationship cursor. Incoming postings address single use rows.
use super::{
    binary::{Decoder, bad},
    graph::{Use, decode_use, ids},
    offsets::{read_bytes, row, table, word},
    symbol_lookup::file_bytes,
};
use crate::domain::content_lookup::ExtentRead;
use std::{io, ops::Range};
pub struct Relations<R> {
    reader: R,
    uses: Range<u64>,
    incoming: Range<u64>,
    end: u64,
    files: usize,
    decls: usize,
}
impl<R: ExtentRead> Relations<R> {
    pub fn open(mut reader: R, files: usize, decls: usize) -> io::Result<Self> {
        let footer = reader.length().checked_sub(8).ok_or_else(bad)?;
        let end = word(&mut reader, footer)?;
        let mut at = end;
        let uses = table(&mut reader, &mut at)?;
        let incoming = table(&mut reader, &mut at)?;
        if at != footer || (incoming.end - incoming.start) / 8 - 1 != decls as u64 {
            return Err(bad());
        }
        Ok(Self {
            reader,
            uses,
            incoming,
            end,
            files,
            decls,
        })
    }
    pub fn visit_file(&mut self, file: usize, mut visit: impl FnMut(Use)) -> io::Result<()> {
        let (_, bytes) = file_bytes(&mut self.reader, &self.uses, self.end, file)?;
        let mut d = Decoder(&bytes);
        while !d.0.is_empty() {
            let u = decode_use(&mut d, self.files, self.decls)?;
            if u.file != file {
                return Err(bad());
            }
            visit(u);
        }
        Ok(())
    }
    pub fn incoming(&mut self, id: usize) -> io::Result<Vec<usize>> {
        let r = row(&mut self.reader, &self.incoming, id as u64, self.end)?;
        let bytes = read_bytes(&mut self.reader, r)?;
        let mut d = Decoder(&bytes);
        let result = ids(&mut d, ((self.uses.end - self.uses.start) / 8 - 1) as usize)?;
        d.end()?;
        Ok(result)
    }
    pub fn site(&mut self, id: usize) -> io::Result<Use> {
        let r = row(&mut self.reader, &self.uses, id as u64, self.end)?;
        let bytes = read_bytes(&mut self.reader, r)?;
        let mut d = Decoder(&bytes);
        let u = decode_use(&mut d, self.files, self.decls)?;
        d.end()?;
        Ok(u)
    }
}
