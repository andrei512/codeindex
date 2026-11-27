//! Query-local relationship cursor. Incoming postings address single use rows.
use super::{
    binary::{Decoder, bad},
    graph::{UseLocation, decode_location, visit_ids},
    offsets::{adjacent, read_bytes, row, table, word},
    symbol_lookup::file_bytes,
};
use crate::domain::content_lookup::ExtentRead;
use std::{io, ops::Range};
pub struct Relations<R> {
    reader: R,
    uses: Range<u64>,
    keys: Option<Range<u64>>,
    contexts: usize,
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
        let keys = (at < footer)
            .then(|| table(&mut reader, &mut at))
            .transpose()?;
        let contexts = keys
            .as_ref()
            .map_or(0, |r| ((r.end - r.start) / 8 - 1) as usize);
        if at != footer || (incoming.end - incoming.start) / 8 - 1 != decls as u64 {
            return Err(bad());
        }
        Ok(Self {
            reader,
            uses,
            keys,
            contexts,
            incoming,
            end,
            files,
            decls,
        })
    }
    pub fn visit_position(
        &mut self,
        file: usize,
        at: usize,
        mut visit: impl FnMut(UseLocation, &[usize]),
    ) -> io::Result<()> {
        let (_, bytes) = file_bytes(&mut self.reader, &self.uses, self.end, file)?;
        let mut d = Decoder(&bytes);
        let mut targets = Vec::new();
        while !d.0.is_empty() {
            let site = self.location_in(&mut d, file)?;
            if !site.span.contains(&at) {
                visit_ids(&mut d, self.decls, |_| {})?;
                continue;
            }
            targets.clear();
            visit_ids(&mut d, self.decls, |id| targets.push(id))?;
            visit(site, &targets);
        }
        Ok(())
    }
    fn location_in(&self, decoder: &mut Decoder<'_>, file: usize) -> io::Result<UseLocation> {
        let site = decode_location(decoder, self.files, self.decls, self.contexts)?;
        if site.file != file { return Err(bad()); }
        Ok(site)
    }

    pub(crate) fn contextual(&self) -> bool {
        self.keys.is_some()
    }
    pub(crate) fn context_key(&mut self, id: usize) -> io::Result<String> {
        let range = row(
            &mut self.reader,
            self.keys.as_ref().ok_or_else(bad)?,
            id as u64,
            self.end,
        )?;
        let bytes = read_bytes(&mut self.reader, range)?;
        let mut d = Decoder(&bytes);
        let key = d.string()?.to_owned();
        d.end()?;
        Ok(key)
    }
    pub(crate) fn definition_contexts(&mut self, id: usize) -> io::Result<Vec<usize>> {
        let range = row(&mut self.reader, &self.incoming, id as u64, self.end)?;
        // Skip the posting list without reading it, including for high fan-in definitions.
        let n = word(&mut self.reader, range.start)?;
        let start = n
            .checked_mul(8)
            .and_then(|len| range.start.checked_add(8)?.checked_add(len))
            .filter(|&p| p <= range.end)
            .ok_or_else(bad)?;
        if start == range.end {
            return Ok(Vec::new());
        }
        let bytes = read_bytes(&mut self.reader, start..range.end)?;
        let mut d = Decoder(&bytes);
        let mut contexts = Vec::new();
        visit_ids(&mut d, self.contexts, |id| contexts.push(id))?;
        d.end()?;
        Ok(contexts)
    }
    /// Reverse postings are in use-row order. Read adjacent records together,
    /// retaining each encoded boundary so corrupt offsets cannot shift a hit.
    pub(crate) fn visit_incoming(
        &mut self,
        id: usize,
        mut visit: impl FnMut(UseLocation) -> io::Result<()>,
    ) -> io::Result<()> {
        let range = row(&mut self.reader, &self.incoming, id as u64, self.end)?;
        let postings = read_bytes(&mut self.reader, range)?;
        let mut d = Decoder(&postings);
        let ids = d.words()?;
        if !d.0.is_empty() {
            d.words()?;
        }
        d.end()?;
        let rows = adjacent::Rows {
            table: &self.uses,
            data_end: self.end,
        };
        rows.visit(&mut self.reader, ids, |bytes| {
            let mut d = Decoder(bytes);
            let site = decode_location(&mut d, self.files, self.decls, self.contexts)?;
            visit_ids(&mut d, self.decls, |_| {})?;
            d.end()?;
            visit(site)
        })
    }
}
