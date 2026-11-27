//! Shards encode file records and rows independently. Merge the dictionaries
//! by key range, retaining file-shard order within every posting list.
use super::{
    bulk::{BulkRank, PackedTerm, varint},
};
use std::io::{self, Write};

pub(crate) enum FreshRank {
    Single(BulkRank),
    Parallel(ShardSet),
}
impl FreshRank {
    pub(crate) fn write_to(&self, out: impl Write) -> io::Result<()> {
        match self {
            Self::Single(v) => v.write_to(out),
            Self::Parallel(v) => v.write_to(out),
        }
    }
}
pub(super) struct TermRecord {
    pub(super) prefix: u64,
    pub(super) key: Vec<u8>,
    pub(super) df: usize,
    pub(super) postings: PackedTerm,
}
pub(crate) struct Shard {
    pub(super) file_bytes: Vec<u8>,
    pub(super) file_ranges: Vec<usize>,
    pub(super) rows: Vec<u8>,
    pub(super) row_count: usize,
    pub(super) tokens: usize,
    pub(super) terms: [Vec<TermRecord>; BUCKETS],
}
pub(super) const BUCKETS: usize = 12;

pub(super) fn bucket(initial: u8) -> usize {
    // Frequent initial letters get smaller ranges. Every input byte has a home.
    [b'a', b'c', b'e', b'g', b'i', b'm', b'o', b'r', b's', b't', b'w']
        .partition_point(|&lower| lower <= initial)
}

struct Terms {
    bytes: Vec<u8>,
    ends: Vec<u64>,
}
impl Terms {
    fn encode(slices: Vec<&[TermRecord]>) -> Self {
        // Stable sorting retains each shard's position for keys it shares
        // with later shards. Prefixes compare without dereferencing heap keys.
        let mut terms: Vec<_> = slices.into_iter().flatten().collect();
        terms.sort_by(|a, b| a.prefix.cmp(&b.prefix).then_with(|| a.key.cmp(&b.key)));
        let capacity = terms
            .iter()
            .map(|t| t.postings.groups.len() + t.key.len() + 4)
            .sum();
        let mut bytes = Vec::with_capacity(capacity);
        let mut ends = Vec::new();
        for group in terms.chunk_by(|a, b| a.key == b.key) {
            let key = &group[0].key;
            varint(&mut bytes, key.len());
            bytes.extend_from_slice(key);
            varint(&mut bytes, group.iter().map(|t| t.df).sum());
            varint(&mut bytes, group.iter().map(|t| t.postings.count).sum());
            for term in group {
                bytes.extend_from_slice(&term.postings.groups);
            }
            ends.push(bytes.len() as u64);
        }
        Self { bytes, ends }
    }
}

pub(crate) struct ShardSet {
    shards: Vec<Shard>,
    terms: Vec<Terms>,
}
impl ShardSet {
    pub(crate) fn new(mut shards: Vec<Shard>) -> io::Result<Self> {
        let terms = std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            for bucket in 0..BUCKETS {
                let slices = shards.iter().map(|s| s.terms[bucket].as_slice()).collect();
                jobs.push(scope.spawn(move || Terms::encode(slices)));
            }
            jobs.into_iter()
                .map(|j| {
                    j.join()
                        .map_err(|_| io::Error::other("rank dictionary encoder failed"))
                })
                .collect::<io::Result<_>>()
        })?;
        std::thread::scope(|scope| {
            for shard in &mut shards {
                let records = std::mem::take(&mut shard.terms);
                scope.spawn(move || drop(records));
            }
        });
        Ok(Self { shards, terms })
    }

    pub(crate) fn write_to(&self, mut out: impl Write) -> io::Result<()> {
        let file_count = self
            .shards
            .iter()
            .map(|s| s.file_ranges.len().saturating_sub(1))
            .sum();
        let row_count = self.shards.iter().map(|s| s.row_count).sum();
        let tokens: usize = self.shards.iter().map(|s| s.tokens).sum();
        let term_count = self.terms.iter().map(|p| p.ends.len()).sum();
        let mut header = b"CIRANK04".to_vec();
        varint(&mut header, file_count);
        out.write_all(&header)?;
        let mut pos = header.len() as u64;
        let mut files = Vec::with_capacity(file_count + 1);
        files.push(pos);
        for s in &self.shards {
            files.extend(s.file_ranges.iter().skip(1).map(|&end| pos + end as u64));
            out.write_all(&s.file_bytes)?;
            pos += s.file_bytes.len() as u64;
        }
        header.clear();
        varint(&mut header, row_count);
        out.write_all(&header)?;
        pos += header.len() as u64;
        for s in &self.shards {
            out.write_all(&s.rows)?;
            pos += s.rows.len() as u64;
        }
        let rows_end = pos;
        header.clear();
        varint(&mut header, term_count);
        out.write_all(&header)?;
        pos += header.len() as u64;
        let mut terms = Vec::with_capacity(term_count + 1);
        terms.push(pos);
        for part in &self.terms {
            terms.extend(part.ends.iter().map(|end| pos + end));
            out.write_all(&part.bytes)?;
            pos += part.bytes.len() as u64;
        }
        let mut footer = Vec::new();
        super::cursor::Directory {
            format: super::storage::Format::Tree,
            files: file_count,
            terms: term_count,
            offset: pos,
            rows_end,
            tokens,
            rows: row_count,
        }
        .write_tree(&mut footer, &files, &terms)?;
        out.write_all(&footer)?;
        out.flush()
    }
}
