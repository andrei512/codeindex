//! A refresh captures eligible inputs and publishes all six query products together.
pub(crate) mod binary;
pub(crate) mod build;
mod catalog;
pub(crate) use catalog::{files, scope_range};
pub(crate) mod graph;
pub mod layout;
pub(crate) mod markdown_lookup;
mod offsets;
mod public_labels;
pub(crate) mod relationship_lookup;
pub(crate) mod symbol_lookup;

use crate::domain::ObservedPath;
use binary::{bad, put, string};
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
/// Both catalogs are path-sorted. Inserts and deletions shift ordinals, so a
/// full pass can match rows with a forward cursor instead of a lookup per file.
pub(crate) fn prior_rows<'a>(old: &'a [Entry], current: &'a [ObservedPath]) -> impl Iterator<Item = Option<usize>> + 'a {
    let mut prior = old.iter().enumerate().peekable();
    current.iter().map(move |row| loop {
        let &(id, entry) = prior.peek()?;
        match entry.path().cmp(&row.path) {
            std::cmp::Ordering::Less => { prior.next(); }
            std::cmp::Ordering::Equal => { prior.next(); return Some(id); }
            std::cmp::Ordering::Greater => return None,
        }
    })
}
pub(super) const CATALOG_MAGIC: &[u8; 8] = b"CIXFILE6";
pub struct Entry {
    pub(crate) score_marker: bool,
    pub(crate) test_score: crate::domain::code::score::Saved,
    pub(crate) score: crate::domain::code::score::Saved,
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
    pub(crate) fn matches_observation(&self, observed: &ObservedPath) -> bool {
        self.path() == observed.path && self.stamp() == observed.stamp
    }

    pub(crate) fn has_markdown(&self) -> bool {
        self.bytes.is_some() && crate::domain::markdown::is_markdown(self.path())
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
    fn write_with_lines(&self, w: &mut impl Write, ends: &[usize]) -> io::Result<()> {
        string(w, self.path())?;
        offsets::write_words(w, &self.observation.stamp)?;
        w.write_all(&[u8::from(self.bytes.is_some()) | (u8::from(self.score_marker) << 1)])?;
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
        put(w, ends.len())?;
        offsets::write_positions(w, ends)?;
        Self::write_score(w, &self.score)?;
        Self::write_score(w, &self.test_score)?;
        Ok(())
    }
    fn write_score(
        w: &mut impl Write,
        saved: &crate::domain::code::score::Saved,
    ) -> io::Result<()> {
        w.write_all(&[u8::from(saved.is_some())])?;
        if let Some(s) = saved {
            put(w, s.lines)?;
            put(w, s.findings.len())?;
            for f in &s.findings {
                w.write_all(&[f.rule as u8])?;
                put(w, f.line)?;
                put(w, f.value)?;
                string(w, &f.unit)?;
            }
        }
        Ok(())
    }
}
