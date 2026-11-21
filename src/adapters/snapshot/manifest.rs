//! Versioned bootstrap manifest for the single-segment native layout.
use super::{Envelope, pieces::Map, registry};
use std::{io, os::unix::ffi::OsStrExt, path::Path};

pub use crate::application::index::layout::*;
// Product-specific layout 2 stores contiguous per-family streams. Layout 1
// remains reserved for the reference partition-directory codec.
use registry::{PRODUCT_TAGS as PRODUCTS,CODEC_TAGS as CODECS};

fn int(out: &mut Vec<u8>, n: u64) {
    out.extend(n.to_le_bytes());
}
fn tag(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_le_bytes());
}

impl Directory {
    pub(super) fn manifest(
        &self,
        root: &Path,
        generation: u64,
        previous: u64,
        commit: Option<&str>,
        checkpoint: &[u8],
        map: &Map,
    ) -> Vec<u8> {
        let spans = map.encode();
        let contracts = registry::write_contracts();
        let mut out = self.encode(
            root,
            generation,
            previous,
            env!("CODEINDEX_RULES").as_bytes(),
            commit.unwrap_or("").as_bytes(),
            &[checkpoint.len(), spans.len(), contracts.len()],
        );
        out.extend_from_slice(checkpoint);
        out.extend(spans);
        out.extend(contracts);
        out
    }
    fn encode(
        &self,
        root: &Path,
        generation: u64,
        previous: u64,
        rules: &[u8],
        commit: &[u8],
        extras: &[usize],
    ) -> Vec<u8> {
        let mut tables: [Vec<u8>; 10] = Default::default();
        tables[0].extend(root.as_os_str().as_bytes());
        int(&mut tables[1], previous);
        int(&mut tables[2], FILES as u64);
        int(&mut tables[3], FILES as u64);
        for (i, r) in self.0.iter().enumerate() {
            let row = &mut tables[4];
            int(row, i as u64 + 1);
            tag(row, PRODUCTS[i]);
            tag(row, CODECS[i]);
            int(row, generation);
            int(row, r.start);
            int(row, r.end - r.start);
            int(row, 0);
            int(row, 0);
        }
        // Each family has its own extent; coverage and path observations
        // are in the file catalog for this layout.
        for i in 0..6 {
            let t = &mut tables[6];
            tag(t, i + 1);
            tag(t, 2);
            int(t, 3 + i as u64);
            int(t, FILES as u64);
        }
        tables[7].extend_from_slice(rules);
        tables[8].extend_from_slice(commit);
        let t = &mut tables[9];
        int(t, generation);
        int(t, self.0.iter().map(|r| r.end).max().unwrap_or(0));
        let mut out = Vec::new();
        let mut offset = 240 + 24 * extras.len() as u64;
        for (i, t) in tables.iter().enumerate() {
            tag(&mut out, i as u32 + 1);
            tag(&mut out, 1);
            int(&mut out, offset);
            int(&mut out, t.len() as u64);
            offset += t.len() as u64;
        }
        for (i, &length) in extras.iter().enumerate() {
            tag(&mut out, 11 + i as u32);
            tag(&mut out, 1);
            int(&mut out, offset);
            int(&mut out, length as u64);
            offset += length as u64;
        }
        for t in tables {
            out.extend(t);
        }
        out
    }
    pub(super) fn decode(
        m: &Envelope,
        root: &Path,
        id: u64,
    ) -> io::Result<(Self, registry::Migration, Option<String>, u64, Map, Vec<u8>)> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "invalid native manifest");
        if m.root() != (0..240) && m.root() != (0..264) && m.root() != (0..312) {
            return Err(bad());
        }
        let mut buf = Vec::new();
        m.visit(0..m.payload_len(), |b| {
            buf.extend(b);
            Ok(())
        })?;
        let num = |x: &[u8]| u64::from_le_bytes(x.try_into().unwrap());
        let mut ext = None;
        let mut prev = None;
        let mut commit = &[][..];
        let mut segment = None;
        let mut extras = Vec::new();
        let mut rules = &[][..];
        for (i, d) in buf
            .get(..m.root().end as usize)
            .ok_or_else(bad)?
            .as_chunks::<24>()
            .0
            .iter()
            .enumerate()
        {
            if d[..4] != (i as u32 + 1).to_le_bytes() || d[4..8] != 1u32.to_le_bytes() {
                return Err(bad());
            }
            let start = usize::try_from(num(&d[8..16])).map_err(|_| bad())?;
            let len = usize::try_from(num(&d[16..])).map_err(|_| bad())?;
            let t = buf
                .get(start..start.checked_add(len).ok_or_else(bad)?)
                .ok_or_else(bad)?;
            match i {
                0 if t != root.as_os_str().as_bytes() => return Err(bad()),
                1 if t.len() == 8 => prev = Some(num(t)),
                4 => ext = Some(t),
                7 if t.is_empty() || (t.len() == 16 && t.iter().all(u8::is_ascii_hexdigit)) => {
                    rules = t
                }
                8 if t.is_empty()
                    || (matches!(t.len(), 40 | 64) && t.iter().all(u8::is_ascii_hexdigit)) =>
                {
                    commit = t
                }
                9 if t.len() == 16 => segment = Some((num(&t[..8]), num(&t[8..]))),
                10..=12 => extras.push(t),
                _ => {}
            }
        }
        let (segment, segment_length) = segment.ok_or_else(bad)?;
        if segment == 0 || segment > id {
            return Err(bad());
        }
        let mut dir = Self::default();
        let ext = ext.ok_or_else(bad)?;
        if ext.len() != 56 * N {
            return Err(bad());
        }
        let mut ordered = Vec::new();
        for (i, row) in ext.as_chunks::<56>().0.iter().enumerate() {
            let start = num(&row[24..32]);
            let end = start.checked_add(num(&row[32..40])).ok_or_else(bad)?;
            if end > segment_length {
                return Err(bad());
            }
            dir.0[i] = start..end;
            if start != end {
                ordered.push(start..end);
            }
        }
        ordered.sort_unstable_by_key(|r| r.start);
        if ordered.windows(2).any(|r| r[0].end > r[1].start) {
            return Err(bad());
        }
        let prev = prev.ok_or_else(bad)?;
        let lengths: Vec<_> = extras.iter().map(|t| t.len()).collect();
        let prefix = dir.encode(root, segment, prev, rules, commit, &lengths);
        if prev >= id
            || !buf.starts_with(&prefix)
            || prefix.len().checked_add(
                lengths
                    .iter()
                    .try_fold(0usize, |n, t| n.checked_add(*t))
                    .ok_or_else(bad)?,
            ) != Some(buf.len())
        {
            return Err(bad());
        }
        let (map, migration) = match extras.as_slice() {
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
        Ok((
            dir,
            migration,
            std::str::from_utf8(commit)
                .ok()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            segment,
            map,
            extras.first().copied().unwrap_or_default().to_vec(),
        ))
    }
}
