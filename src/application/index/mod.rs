//! A refresh captures eligible inputs and publishes all six query products together.
pub(crate) mod binary;
pub(crate) mod build;
pub(crate) mod graph;
pub mod layout;
pub(crate) mod markdown_lookup;
mod offsets;
mod public_labels;
pub(crate) mod relationship_lookup;
pub(crate) mod symbol_lookup;

use crate::domain::ObservedPath;
use binary::{Decoder, bad, put, string};
use std::{
    io::{self, Write},
    ops::Range,
};

#[derive(PartialEq, Eq, Debug, Clone)]
struct Observation {
    path: String,
    stamp: [u64; 7],
}
pub(crate) fn same_observation(a: &[ObservedPath], b: &[ObservedPath]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.path == b.path && a.stamp == b.stamp)
}
pub(super) const CATALOG_MAGIC: &[u8; 8] = b"CIXFILE3";
pub struct Entry {
    pub(crate) blob: Option<crate::domain::BlobId>,
    catalog_row: Range<u64>,
    observation: Observation,
    original_stamp: Option<Box<[u64; 7]>>,
    pub bytes: Option<Range<u64>>,
    pub line_ends: Vec<usize>,
    pub(crate) evidence: Range<u64>,
}
impl Entry {
    pub(crate) fn update_stamp(&mut self, stamp: [u64; 7]) {
        if self.observation.stamp != stamp {
            self.original_stamp
                .get_or_insert_with(|| Box::new(self.observation.stamp));
            self.observation.stamp = stamp;
        }
    }
    pub(crate) fn catalog_stamp(&self) -> [u64; 7] {
        self.original_stamp
            .as_deref()
            .copied()
            .unwrap_or(self.observation.stamp)
    }
    pub(crate) fn stamp(&self) -> [u64; 7] {
        self.observation.stamp
    }
    pub fn path(&self) -> &str {
        &self.observation.path
    }
    pub fn line_column(&self, byte: usize) -> (usize, usize) {
        let line = self.line_ends.partition_point(|&end| end <= byte);
        let previous = if line == 0 {
            0
        } else {
            self.line_ends[line - 1]
        };
        (line + 1, byte - previous + 1)
    }
    pub fn line_start(&self, line: usize) -> Option<usize> {
        match line {
            0 => None,
            1 => Some(0),
            n => self.line_ends.get(n - 2).copied(),
        }
    }
    fn write(&self, w: &mut impl Write) -> io::Result<()> {
        string(w, self.path())?;
        for s in self.observation.stamp {
            w.write_all(&s.to_le_bytes())?;
        }
        w.write_all(&[u8::from(self.bytes.is_some())])?;
        if let Some(b) = &self.bytes {
            w.write_all(&(b.end - b.start).to_le_bytes())?;
            let blob = self
                .blob
                .as_ref()
                .filter(|b| matches!(b.len, 20 | 32))
                .ok_or_else(bad)?;
            w.write_all(&[blob.len])?;
            w.write_all(blob.as_bytes())?;
        }
        put(w, self.line_ends.len())?;
        for &end in &self.line_ends {
            put(w, end)?;
        }
        Ok(())
    }
}
pub(crate) fn files(bytes: &[u8], source_len: u64) -> io::Result<Vec<Entry>> {
    let mut d = Decoder(bytes);
    let catalog_size = bytes.len();
    let hashes = bytes.starts_with(CATALOG_MAGIC);
    let packed = hashes || bytes.starts_with(b"CIXFILE2");
    if packed {
        d.take(8)?;
    }
    let mut source_end = 0u64;
    let count = d.num()?;
    let mut files = Vec::new();
    for _ in 0..count {
        let row_start = (bytes.len() - d.0.len()) as u64;
        let path = d.string()?.to_owned();
        let mut stamp = [0; 7];
        for s in &mut stamp {
            *s = u64::from_le_bytes(d.take(8)?.try_into().unwrap());
        }
        let bytes = match d.take(1)? {
            [0] => None,
            [1] => {
                let start = if packed { source_end } else { d.num()? as u64 };
                let n = d.num()? as u64;
                let end = if packed {
                    start.checked_add(n).ok_or_else(bad)?
                } else {
                    n
                };
                source_end = end;
                if start > end || end > source_len {
                    return Err(bad());
                }
                Some(start..end)
            }
            _ => return Err(bad()),
        };
        let blob = if hashes && bytes.is_some() {
            let len = d.take(1)?[0];
            if !matches!(len, 20 | 32) {
                return Err(bad());
            }
            let mut data = [0u8; 32];
            data[..len as usize].copy_from_slice(d.take(len as usize)?);
            Some(crate::domain::BlobId { bytes: data, len })
        } else {
            None
        };
        let count = d.num()?;
        let mut line_ends = Vec::new();
        let mut previous = 0;
        for _ in 0..count {
            let end = d.num()?;
            if end <= previous || bytes.as_ref().is_none_or(|b| end as u64 > b.end - b.start) {
                return Err(bad());
            }
            line_ends.push(end);
            previous = end;
        }
        if files
            .last()
            .is_some_and(|f: &Entry| f.path() >= path.as_str())
        {
            return Err(bad());
        }
        files.push(Entry {
            blob,
            catalog_row: if hashes {
                row_start..(catalog_size - d.0.len()) as u64
            } else {
                0..0
            },
            original_stamp: None,
            observation: Observation { path, stamp },
            bytes,
            line_ends,
            evidence: 0..0,
        });
    }
    // Older generations have no evidence directory and are rebuilt on rules mismatch.
    if packed && source_end != source_len {
        return Err(bad());
    }
    if packed || !d.0.is_empty() {
        let mut from = d.num()? as u64;
        for f in &mut files {
            let word = d.num()? as u64;
            let end = if packed {
                from.checked_add(word).ok_or_else(bad)?
            } else {
                word
            };
            if end < from {
                return Err(bad());
            }
            f.evidence = from..end;
            from = end;
        }
    }
    d.end()?;
    Ok(files)
}
