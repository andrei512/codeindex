//! Fresh builds consume a whole file at a time. Its spans and term-local
//! postings can immediately be encoded into their final wire representation.
use super::{ChunkRange, File, RankedIndex};
use std::io::{self, Write};

#[derive(Default)]
pub(crate) struct BulkRank {
    rank: RankedIndex,
    first_file: usize,
    pub(super) terms: Vec<PackedTerm>,
    pub(super) file_bytes: Vec<u8>,
    pub(super) file_ranges: Vec<usize>,
    touched: Vec<usize>,
    short: Vec<(u64, usize)>,
}
#[derive(Default)]
pub(super) struct PackedTerm {
    pub(super) count: usize,
    pub(super) groups: Vec<u8>,
}
#[inline]
pub(super) fn varint(out: &mut Vec<u8>, mut n: usize) {
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
}
pub(super) fn encode_file(f: &File, out: &mut Vec<u8>) {
    varint(out, f.path.len());
    out.extend_from_slice(f.path.as_bytes());
    varint(out, f.spans.len());
    let mut prev = 0;
    for s in &f.spans {
        varint(out, s.start - prev);
        varint(out, s.len());
        prev = s.end;
    }
}
impl BulkRank {
    pub(crate) fn at(first_file: usize) -> Self {
        Self {
            first_file,
            ..Self::default()
        }
    }
    pub(crate) fn add_file(&mut self, path: &str, body: &str, chunks: &[ChunkRange]) {
        if self.short.is_empty() && (body.len() >= 64 * 1024 || self.rank.tokens >= 2048) {
            // The high byte of a packed key is its length (0..=7). MAX is unused.
            self.short.resize(4096, (u64::MAX, 0));
        }
        let first_position = self.rank.tokens;
        let file = self
            .rank
            .add_file_with(path, body, chunks, &mut self.short, |id| self.touched.push(id));
        self.terms
            .resize_with(self.rank.terms.len(), PackedTerm::default);
        for id in self.touched.drain(..) {
            let positions = &mut self.rank.terms[id].positions;
            let term = &mut self.terms[id];
            term.count += positions.len();
            varint(&mut term.groups, self.first_file + file);
            varint(&mut term.groups, positions.len());
            let mut prev = first_position;
            for p in positions.drain(..) {
                varint(&mut term.groups, p - prev);
                prev = p;
            }
        }
        if self.file_ranges.is_empty() {
            self.file_ranges.push(0);
        }
        let f = &mut self.rank.files[file];
        encode_file(f, &mut self.file_bytes);
        self.file_ranges.push(self.file_bytes.len());
        f.spans = Vec::new();
        f.path = String::new();
    }
    pub(crate) fn write_to(&self, out: impl Write) -> io::Result<()> {
        self.rank.write_with(out, Some(self))
    }
}

impl BulkRank {
    pub(crate) fn into_shard(mut self) -> super::shards::Shard {
        use super::shards::{Shard, TermRecord};
        let mut terms: [Vec<TermRecord>; super::shards::BUCKETS] = Default::default();
        for (key, id) in self.rank.dictionary {
            let mut prefix = [0; 8];
            let n = key.len().min(8);
            prefix[..n].copy_from_slice(&key[..n]);
            let bucket = super::shards::bucket(prefix[0]);
            terms[bucket].push(TermRecord {
                prefix: u64::from_be_bytes(prefix),
                key,
                df: self.rank.terms[id].document_frequency,
                postings: std::mem::take(&mut self.terms[id]),
            });
        }
        let mut rows = Vec::new();
        let mut previous = 0;
        for row in &self.rank.rows {
            for n in [
                self.first_file + row.file,
                row.chunk.first_line,
                row.chunk.last_line - row.chunk.first_line,
                row.chunk.bytes.start,
                row.chunk.bytes.len(),
                row.span_start,
                row.body_start - previous,
                row.parts_start - row.body_start,
                row.end - row.parts_start,
            ] {
                varint(&mut rows, n);
            }
            previous = row.end;
        }
        Shard {
            file_bytes: self.file_bytes,
            file_ranges: self.file_ranges,
            row_count: self.rank.rows.len(),
            rows,
            tokens: self.rank.tokens,
            terms,
        }
    }
}
