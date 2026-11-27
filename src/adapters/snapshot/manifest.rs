//! Versioned bootstrap manifest for the single-segment native layout.
use super::{pieces::Map, registry};
use std::{io, ops::Range, os::unix::ffi::OsStrExt, path::Path};

pub use crate::application::index::layout::*;
// Product-specific layout 2 stores contiguous per-family streams. Layout 1
// remains reserved for the reference partition-directory codec.
use registry::{PRODUCT_TAGS as PRODUCTS,CODEC_TAGS as CODECS};

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid native manifest")
}

fn num(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

fn int(out: &mut Vec<u8>, n: u64) {
    out.extend(n.to_le_bytes());
}
fn tag(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_le_bytes());
}

/// Each descriptor precedes its payload. A writer records the byte range
/// after emitting the table, including for variable-length checkpoints.
fn descriptor(out: &mut [u8], table: usize, start: usize, length: usize) {
    let row = &mut out[24 * table..24 * (table + 1)];
    row[..4].copy_from_slice(&(table as u32 + 1).to_le_bytes());
    row[4..8].copy_from_slice(&1u32.to_le_bytes());
    row[8..16].copy_from_slice(&(start as u64).to_le_bytes());
    row[16..24].copy_from_slice(&(length as u64).to_le_bytes());
}

fn table<'a>(buf: &'a [u8], i: usize, d: &[u8; 24]) -> io::Result<&'a [u8]> {
    if d[..4] != (i as u32 + 1).to_le_bytes() || d[4..8] != 1u32.to_le_bytes() {
        return Err(bad());
    }
    let start = usize::try_from(num(&d[8..16])).map_err(|_| bad())?;
    let len = usize::try_from(num(&d[16..])).map_err(|_| bad())?;
    buf
        .get(start..start.checked_add(len).ok_or_else(bad)?)
        .ok_or_else(bad)
}

pub(super) struct Selected<'a> {
    pub directory: Directory,
    pub migration: registry::Migration,
    pub commit: Option<String>,
    pub generation: u64,
    pub map: Map,
    pub checkpoint: Option<&'a [u8]>,
}

/// Borrow exactly the published descriptor payloads. Semantic decoding and
/// canonical prefix verification both consume this same sequence.
struct Tables<'a> {
    rows: Vec<&'a [u8]>,
}
impl<'a> Tables<'a> {
    fn read(buf: &'a [u8], header_len: usize) -> io::Result<Self> {
        let descriptors = buf.get(..header_len).ok_or_else(bad)?.as_chunks::<24>().0;
        if !matches!(descriptors.len(), 10 | 11 | 13) {
            return Err(bad());
        }
        let rows = descriptors
            .iter()
            .enumerate()
            .map(|(i, d)| table(buf, i, d))
            .collect::<io::Result<_>>()?;
        Ok(Self { rows })
    }
    fn extras(&self) -> &[&'a [u8]] {
        &self.rows[10..]
    }
    fn hex(&self, slot: usize, widths: &[usize]) -> io::Result<&'a [u8]> {
        let data = self.rows[slot];
        if !data.is_empty()
            && !(widths.contains(&data.len()) && data.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(bad());
        }
        Ok(data)
    }
    fn check_prefix(&self, buf: &[u8], mut prefix: Vec<u8>) -> io::Result<()> {
        let mut end = prefix.len();
        for (i, extra) in self.extras().iter().enumerate() {
            descriptor(&mut prefix, 10 + i, end, extra.len());
            end = end.checked_add(extra.len()).ok_or_else(bad)?;
        }
        if !buf.starts_with(&prefix) || end != buf.len() {
            return Err(bad());
        }
        Ok(())
    }
}
pub(super) struct Identity<'a> {
    pub root: &'a Path,
    pub previous: u64,
    pub commit: Option<&'a str>,
}

struct Prefix<'a> {
    root: &'a Path,
    segment: u64,
    previous: u64,
    rules: &'a [u8],
    commit: &'a [u8],
    extras: usize,
}

impl Directory {
    pub(super) fn manifest(
        &self, identity: Identity<'_>, segment: u64,
        map: &Map, checkpoint: impl FnOnce(&mut Vec<u8>) -> io::Result<()>,
    ) -> io::Result<Vec<u8>> {
        let mut out = self.prefix(Prefix {
            root: identity.root,
            segment,
            previous: identity.previous,
            rules: env!("CODEINDEX_RULES").as_bytes(),
            commit: identity.commit.unwrap_or("").as_bytes(),
            extras: 3,
        });
        let evidence = out.len();
        checkpoint(&mut out)?;
        let spans = out.len();
        map.encode_into(&mut out);
        let contracts = out.len();
        registry::append_contracts(&mut out);
        let end = out.len();
        descriptor(&mut out, 10, evidence, spans - evidence);
        descriptor(&mut out, 11, spans, contracts - spans);
        descriptor(&mut out, 12, contracts, end - contracts);
        Ok(out)
    }

    fn prefix(&self, prefix: Prefix<'_>) -> Vec<u8> {
        let Prefix { root, segment, previous, rules, commit, extras } = prefix;
        let path = root.as_os_str().as_bytes();
        let lengths = [path.len(), 8, 8, 8, 56 * N, 0, 6 * 24, rules.len(), commit.len(), 16];
        let header = 24 * (lengths.len() + extras);
        let mut out = Vec::with_capacity(header + lengths.iter().sum::<usize>());
        out.resize(header, 0);
        let mut offset = header;
        for (table, length) in lengths.into_iter().enumerate() {
            descriptor(&mut out, table, offset, length);
            offset += length;
        }
        out.extend_from_slice(path);
        int(&mut out, previous);
        int(&mut out, FILES as u64);
        int(&mut out, FILES as u64);
        for (i, r) in self.0.iter().enumerate() {
            int(&mut out, i as u64 + 1);
            tag(&mut out, PRODUCTS[i]);
            tag(&mut out, CODECS[i]);
            int(&mut out, segment);
            int(&mut out, r.start);
            int(&mut out, r.end - r.start);
            int(&mut out, 0);
            int(&mut out, 0);
        }
        for i in 0..6 {
            tag(&mut out, i + 1);
            tag(&mut out, 2);
            int(&mut out, 3 + i as u64);
            int(&mut out, FILES as u64);
        }
        out.extend_from_slice(rules);
        out.extend_from_slice(commit);
        int(&mut out, segment);
        int(&mut out, self.0.iter().map(|r| r.end).max().unwrap_or(0));
        debug_assert_eq!(out.len(), offset);
        out
    }
    fn decode_extents(ext: &[u8], segment_length: u64) -> io::Result<Self> {
        let mut dir = Self::default();
        if ext.len() != 56 * N {
            return Err(bad());
        }
        let mut ordered = Vec::new();
        for (i, row) in ext.as_chunks::<56>().0.iter().enumerate() {
            let start = num(&row[24..32]);
            let end = start.checked_add(num(&row[32..40])).ok_or_else(bad)?;
            dir.0[i] = checked_extent(start..end, segment_length)?;
            ordered.extend((start != end).then_some(start..end));
        }
        ordered.sort_unstable_by_key(|r| r.start);
        if ordered.windows(2).any(|r| r[0].end > r[1].start) {
            return Err(bad());
        }
        Ok(dir)
    }

    pub(super) fn decode<'a>(
        buf: &'a [u8],
        header_len: usize,
        root: &Path,
        id: u64,
    ) -> io::Result<Selected<'a>> {
        let tables = Tables::read(buf, header_len)?;
        if tables.rows[0] != root.as_os_str().as_bytes() {
            return Err(bad());
        }
        let prev = u64::from_le_bytes(tables.rows[1].try_into().map_err(|_| bad())?);
        let segment_info: &[u8; 16] = tables.rows[9].try_into().map_err(|_| bad())?;
        let (segment, segment_length) = (num(&segment_info[..8]), num(&segment_info[8..]));
        if prev >= id || !(1..=id).contains(&segment) {
            return Err(bad());
        }
        let rules = tables.hex(7, &[16])?;
        let commit = tables.hex(8, &[40, 64])?;
        let dir = Self::decode_extents(tables.rows[4], segment_length)?;
        let extras = tables.extras();
        let prefix = dir.prefix(Prefix { root, segment, previous: prev, rules, commit, extras: extras.len() });
        tables.check_prefix(buf, prefix)?;
        let (map, migration) = match extras {
            [] | [_] => (
                Map::legacy(segment, segment_length)?,
                registry::legacy_migration(rules),
            ),
            [_, spans, contracts] => {
                let migration = registry::check_contracts(contracts)?;
                (
                    Map::decode(spans, id, segment_length)?,
                    migration,
                )
            }
            _ => return Err(bad()),
        };
        Ok(Selected {
            directory: dir,
            migration,
            commit: std::str::from_utf8(commit)
                .ok()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            generation: segment,
            map,
            checkpoint: extras.first().copied(),
        })
    }
}

fn checked_extent(extent: Range<u64>, segment_length: u64) -> io::Result<Range<u64>> {
    if extent.end > segment_length { return Err(bad()); }
    Ok(extent)
}
