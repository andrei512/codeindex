//! Payload of the checkpoint table. The enclosing snapshot supplies integrity checks.
//! Dense catalog ordinals address stamp rows. Only fully consumed files have blob rows.
use crate::adapters::repository::BlobId;
use std::io::{self, Write};

const DENSE: &[u8; 8] = b"CIXCHK01";
const SPARSE: &[u8; 8] = b"CIXCHK02";
const CATALOG: &[u8; 8] = b"CIXCHK03";
const STAMP_BYTES: usize = 7 * 8;

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub struct Row {
    pub blob: Option<BlobId>,
    pub committed: bool,
    pub stamp: [u64; 7],
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Checkpoint {
    pub sha256: bool,
    pub rows: Vec<Row>,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid checkpoint extent")
}

fn take<'a>(bytes: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    let (head, tail) = bytes.split_at_checked(n).ok_or_else(invalid)?;
    *bytes = tail;
    Ok(head)
}

fn count(bytes: &mut &[u8]) -> io::Result<usize> {
    usize::try_from(u64::from_le_bytes(take(bytes, 8)?.try_into().unwrap())).map_err(|_| invalid())
}

impl Checkpoint {
    pub(super) fn encode_catalog(
        &self,
        w: &mut impl Write,
        files: &[crate::application::index::Entry],
    ) -> io::Result<()> {
        let base = files.iter().map(|f| f.catalog_stamp());
        // A metadata-only migration can keep an old, unhashed catalog alive.
        if files.iter().any(|f| f.bytes.is_some() && f.blob.is_none()) {
            return self.encode(w, base);
        }
        if self
            .rows
            .iter()
            .zip(files)
            .any(|(row, file)| row.blob != file.blob)
        {
            return Err(invalid());
        }
        self.write(w, base, true)
    }

    pub fn encode(
        &self,
        w: &mut impl Write,
        base: impl ExactSizeIterator<Item = [u64; 7]>,
    ) -> io::Result<()> {
        self.write(w, base, false)
    }
    fn write(
        &self,
        w: &mut impl Write,
        base: impl ExactSizeIterator<Item = [u64; 7]>,
        compact: bool,
    ) -> io::Result<()> {
        if base.len() != self.rows.len() {
            return Err(invalid());
        }
        let width = if self.sha256 { 32 } else { 20 };
        w.write_all(if compact { CATALOG } else { SPARSE })?;
        w.write_all(&[if self.sha256 { 2 } else { 1 }])?;
        w.write_all(&(self.rows.len() as u64).to_le_bytes())?;
        let mut overrides = 0u64;
        for (ordinal, (row, original)) in self.rows.iter().zip(base).enumerate() {
            if row.stamp == original {
                continue;
            }
            let mut mask = 0u8;
            for i in 0..7 {
                if row.stamp[i] != original[i] {
                    mask |= 1 << i;
                }
            }
            w.write_all(&(ordinal as u64).to_le_bytes())?;
            w.write_all(&[mask])?;
            for (i, field) in row.stamp.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    w.write_all(&field.to_le_bytes())?;
                }
            }
            overrides += 1;
        }
        if compact {
            for group in self.rows.chunks(8) {
                let mut flags = 0u8;
                for (bit, row) in group.iter().enumerate() {
                    if row.blob.is_some_and(|id| id.len as usize != width)
                        || row.committed && row.blob.is_none()
                    {
                        return Err(invalid());
                    }
                    flags |= u8::from(row.committed) << bit;
                }
                w.write_all(&[flags])?;
            }
            return w.write_all(&overrides.to_le_bytes());
        }
        let mut blobs = 0u64;
        for (ordinal, row) in self.rows.iter().enumerate() {
            let Some(blob) = row.blob else {
                if row.committed {
                    return Err(invalid());
                }
                continue;
            };
            if usize::from(blob.len) != width || blob.bytes[width..].iter().any(|&b| b != 0) {
                return Err(invalid());
            }
            w.write_all(&(ordinal as u64).to_le_bytes())?;
            w.write_all(&[u8::from(row.committed)])?;
            w.write_all(blob.as_bytes())?;
            blobs += 1;
        }
        w.write_all(&overrides.to_le_bytes())?;
        w.write_all(&blobs.to_le_bytes())
    }

    #[cfg(test)]
    fn decode(bytes: &[u8], base: impl ExactSizeIterator<Item = [u64; 7]>) -> io::Result<Self> {
        Self::decode_catalog(bytes, base.map(|s| (s, None)))
    }
    pub(super) fn decode_catalog(
        mut bytes: &[u8],
        base: impl ExactSizeIterator<Item = ([u64; 7], Option<BlobId>)>,
    ) -> io::Result<Self> {
        let compact = bytes.starts_with(CATALOG);
        let sparse = match take(&mut bytes, 8)? {
            magic if magic == SPARSE || magic == CATALOG => true,
            magic if magic == DENSE => false,
            _ => return Err(invalid()),
        };
        let sha256 = match take(&mut bytes, 1)?[0] {
            1 => false,
            2 => true,
            _ => return Err(invalid()),
        };
        let width = if sha256 { 32 } else { 20 };
        let n = count(&mut bytes)?;
        if n != base.len() {
            return Err(invalid());
        }
        let trailer_len = if sparse && !compact { 16 } else { 8 };
        let (data, mut trailer) = bytes
            .split_at_checked(bytes.len().checked_sub(trailer_len).ok_or_else(invalid)?)
            .ok_or_else(invalid)?;
        let overrides = if sparse { count(&mut trailer)? } else { 0 };
        let blobs = if compact { 0 } else { count(&mut trailer)? };
        if blobs > n || overrides > n {
            return Err(invalid());
        }
        let body_end = data
            .len()
            .checked_sub(if compact {
                n.div_ceil(8)
            } else {
                blobs.checked_mul(9 + width).ok_or_else(invalid)?
            })
            .ok_or_else(invalid)?;
        let (mut stamps, blob_rows) = data.split_at(body_end);
        bytes = blob_rows;
        let mut rows: Vec<_> = base
            .map(|(stamp, id)| Row {
                stamp,
                blob: if compact { id } else { None },
                committed: false,
            })
            .collect();
        if sparse {
            let mut previous = None;
            for _ in 0..overrides {
                let ordinal = count(&mut stamps)?;
                if previous.is_some_and(|prior| ordinal <= prior) {
                    return Err(invalid());
                }
                previous = Some(ordinal);
                let row = rows.get_mut(ordinal).ok_or_else(invalid)?;
                let mask = take(&mut stamps, 1)?[0];
                if mask == 0 || mask & 0x80 != 0 {
                    return Err(invalid());
                }
                for (i, field) in row.stamp.iter_mut().enumerate() {
                    if mask & (1 << i) != 0 {
                        let next = u64::from_le_bytes(take(&mut stamps, 8)?.try_into().unwrap());
                        if *field == next {
                            return Err(invalid());
                        }
                        *field = next;
                    }
                }
            }
        } else {
            for row in &mut rows {
                let words = take(&mut stamps, STAMP_BYTES)?;
                for (word, raw) in row.stamp.iter_mut().zip(words.chunks_exact(8)) {
                    *word = u64::from_le_bytes(raw.try_into().unwrap());
                }
            }
        }
        if !stamps.is_empty() {
            return Err(invalid());
        }
        if compact {
            for (&flags, group) in bytes.iter().zip(rows.chunks_mut(8)) {
                if (flags as u16) >> group.len() != 0 {
                    return Err(invalid());
                }
                for (bit, row) in group.iter_mut().enumerate() {
                    row.committed = flags & (1 << bit) != 0;
                    if row.blob.is_some_and(|id| id.len as usize != width)
                        || row.committed && row.blob.is_none()
                    {
                        return Err(invalid());
                    }
                }
            }
            return Ok(Self { sha256, rows });
        }
        let mut previous = None;
        while !bytes.is_empty() {
            let ordinal = count(&mut bytes)?;
            if previous.is_some_and(|prior| ordinal <= prior) {
                return Err(invalid());
            }
            previous = Some(ordinal);
            let target = rows.get_mut(ordinal).ok_or_else(invalid)?;
            target.committed = match take(&mut bytes, 1)?[0] {
                0 => false,
                1 => true,
                _ => return Err(invalid()),
            };
            let mut raw = [0; 32];
            raw[..width].copy_from_slice(take(&mut bytes, width)?);
            target.blob = Some(BlobId {
                bytes: raw,
                len: width as u8,
            });
        }
        Ok(Self { sha256, rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::repository::raw_blob_id;
    fn fixture(sha256: bool) -> Checkpoint {
        Checkpoint {
            sha256,
            rows: vec![
                Row {
                    stamp: [1; 7],
                    committed: true,
                    blob: Some(raw_blob_id(b"a", sha256)),
                },
                Row {
                    stamp: [9; 7],
                    committed: false,
                    blob: None,
                },
                Row {
                    stamp: [3; 7],
                    committed: false,
                    blob: Some(raw_blob_id(b"b", sha256)),
                },
            ],
        }
    }
    #[test]
    fn round_trip_and_reject_malformed_rows() {
        for sha256 in [false, true] {
            let point = fixture(sha256);
            let mut bytes = vec![];
            point
                .encode(&mut bytes, point.rows.iter().map(|r| r.stamp))
                .unwrap();
            let base = || point.rows.iter().map(|r| r.stamp);
            assert_eq!(Checkpoint::decode(&bytes, base()).unwrap(), point);
            assert!(Checkpoint::decode(&bytes, std::iter::empty()).is_err());
            let start = 17;
            let stride = 9 + if sha256 { 32 } else { 20 };
            for (i, value) in [
                (0, 0),
                (8, 3),
                (9, 4),
                (start, 3),
                (start + 8, 2),
                (start + stride, 0),
            ] {
                let mut broken = bytes.clone();
                broken[i] = value;
                assert!(Checkpoint::decode(&broken, base()).is_err());
            }
            assert!(Checkpoint::decode(&bytes[..bytes.len() - 1], base()).is_err());
            assert!(Checkpoint::decode(&bytes[..start - 1], base()).is_err());
        }
    }
    #[test]
    fn sparse_stamp_roundtrip_and_reject_bad_masks() {
        let point = fixture(false);
        let mut base: Vec<_> = point.rows.iter().map(|r| r.stamp).collect();
        base[1][5] = 123;
        let mut bytes = vec![];
        point.encode(&mut bytes, base.iter().copied()).unwrap();
        assert_eq!(bytes[25], 1 << 5);
        assert_eq!(bytes.len(), 17 + 17 + 2 * (9 + 20) + 16);
        assert_eq!(
            Checkpoint::decode(&bytes, base.iter().copied()).unwrap(),
            point
        );
        for (i, value) in [(17, 3), (25, 0), (25, 128), (26, 123)] {
            let mut broken = bytes.clone();
            broken[i] = value;
            assert!(Checkpoint::decode(&broken, base.iter().copied()).is_err());
        }
        let mut dense = Vec::from(DENSE.as_slice());
        dense.push(1);
        dense.extend_from_slice(&3u64.to_le_bytes());
        for row in &point.rows {
            for field in row.stamp {
                dense.extend_from_slice(&field.to_le_bytes());
            }
        }
        dense.extend_from_slice(&bytes[34..bytes.len() - 16]);
        dense.extend_from_slice(&2u64.to_le_bytes());
        assert_eq!(Checkpoint::decode(&dense, base.into_iter()).unwrap(), point);
    }
    #[test]
    fn compact_provenance_reads_hashes_from_the_catalog_and_applies_sparse_stamps() {
        for sha256 in [false, true] {
            let p = fixture(sha256);
            let mut base = p.rows.iter().map(|r| (r.stamp, r.blob)).collect::<Vec<_>>();
            base[2].0[6] = 17;
            let mut bytes = vec![];
            p.write(&mut bytes, base.iter().map(|r| r.0), true).unwrap();
            // Header + one ordinal/mask/field + one byte of flags + override count.
            assert_eq!(bytes.len(), 17 + 17 + 1 + 8);
            assert_eq!(
                Checkpoint::decode_catalog(&bytes, base.iter().copied()).unwrap(),
                p
            );
            let flags = bytes.len() - 9;
            for invalid in [0x80, 0b10] {
                // padding bit; committed path-only binary
                let mut corrupt = bytes.clone();
                corrupt[flags] |= invalid;
                assert!(Checkpoint::decode_catalog(&corrupt, base.iter().copied()).is_err());
            }
            base[0].1 = None;
            assert!(Checkpoint::decode_catalog(&bytes, base.into_iter()).is_err());
        }
    }
    #[test]
    fn reject_impossible_memory_states() {
        let mut point = fixture(false);
        point.rows[1].committed = true;
        assert!(
            point
                .encode(&mut Vec::new(), point.rows.iter().map(|r| r.stamp))
                .is_err()
        );
        point.rows[1].committed = false;
        point.rows[0].blob.as_mut().unwrap().len = 32;
        assert!(
            point
                .encode(&mut Vec::new(), point.rows.iter().map(|r| r.stamp))
                .is_err()
        );
        point.rows[0].blob.as_mut().unwrap().len = 20;
        point.rows[0].blob.as_mut().unwrap().bytes[31] = 1;
        assert!(
            point
                .encode(&mut Vec::new(), point.rows.iter().map(|r| r.stamp))
                .is_err()
        );
    }
}
