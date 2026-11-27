//! Payload of the checkpoint table. The enclosing snapshot supplies integrity checks.
//! Dense catalog ordinals address stamp rows. Only fully consumed files have blob rows.
use crate::adapters::repository::BlobId;
use std::io;

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

impl Row {
    fn append_blob(&self, out: &mut Vec<u8>, ordinal: usize, width: usize) -> io::Result<bool> {
        self.validate_identity(width)?;
        let Some(blob) = self.blob else {
            return Ok(false);
        };
        if blob.bytes[width..].iter().any(|&b| b != 0) {
            return Err(invalid());
        }
        out.extend_from_slice(&(ordinal as u64).to_le_bytes());
        out.push(u8::from(self.committed));
        out.extend_from_slice(blob.as_bytes());
        Ok(true)
    }
    fn validate_identity(&self, width: usize) -> io::Result<()> {
        let valid = match self.blob {
            Some(id) => usize::from(id.len) == width,
            None => !self.committed,
        };
        if valid { Ok(()) } else { Err(invalid()) }
    }
}

/// Sparse stamps encode only changed fields, in ascending bit order.
struct StampFields(u8);
impl StampFields {
    fn between(stamp: &[u64; 7], base: &[u64; 7]) -> Self {
        let mut mask = 0;
        for (i, (value, original)) in stamp.iter().zip(base).enumerate() {
            mask |= u8::from(value != original) << i;
        }
        Self(mask)
    }
}
impl Iterator for StampFields {
    type Item = usize;
    fn next(&mut self) -> Option<usize> {
        if self.0 == 0 {
            return None;
        }
        let next = self.0.trailing_zeros() as usize;
        self.0 &= self.0 - 1;
        Some(next)
    }
}
fn stamp_override(out: &mut Vec<u8>, ordinal: usize, stamp: &[u64; 7], original: &[u64; 7]) {
    let fields = StampFields::between(stamp, original);
    out.extend_from_slice(&(ordinal as u64).to_le_bytes());
    out.push(fields.0);
    for i in fields {
        out.extend_from_slice(&stamp[i].to_le_bytes());
    }
}

#[derive(Clone, Copy)]
enum CatalogBlobs {
    Matching,
    Mismatch,
    Missing,
}

impl CatalogBlobs {
    fn observe(&mut self, unhashed: bool, catalog: Option<BlobId>, checkpoint: Option<BlobId>) {
        if unhashed {
            *self = Self::Missing;
        } else if matches!(self, Self::Matching) && catalog != checkpoint {
            *self = Self::Mismatch;
        }
    }
}

#[derive(Clone, Copy)]
enum HashFormat {
    Sha1,
    Sha256,
}
impl HashFormat {
    fn for_checkpoint(sha256: bool) -> Self {
        if sha256 { Self::Sha256 } else { Self::Sha1 }
    }
    fn read(bytes: &mut &[u8]) -> io::Result<Self> {
        match take(bytes, 1)?[0] {
            1 => Ok(Self::Sha1),
            2 => Ok(Self::Sha256),
            _ => Err(invalid()),
        }
    }
    fn tag(self) -> u8 {
        match self {
            Self::Sha1 => 1,
            Self::Sha256 => 2,
        }
    }
    fn width(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
}
impl Checkpoint {
    pub(super) fn encode_catalog(
        &self,
        out: &mut Vec<u8>,
        files: &[crate::application::index::Entry],
    ) -> io::Result<()> {
        self.append(
            out,
            files.iter().map(|file| {
                (
                    file.catalog_stamp(),
                    file.blob,
                    file.bytes.is_some() && file.blob.is_none(),
                )
            }),
        )
    }

    fn append(
        &self,
        out: &mut Vec<u8>,
        base: impl ExactSizeIterator<Item = ([u64; 7], Option<BlobId>, bool)>,
    ) -> io::Result<()> {
        if base.len() != self.rows.len() {
            return Err(invalid());
        }
        let header = out.len();
        out.extend_from_slice(CATALOG);
        let hash = HashFormat::for_checkpoint(self.sha256);
        out.push(hash.tag());
        out.extend_from_slice(&(self.rows.len() as u64).to_le_bytes());
        let mut overrides = 0u64;
        let mut catalog_blobs = CatalogBlobs::Matching;
        for (ordinal, (row, (stamp, blob, unhashed))) in self.rows.iter().zip(base).enumerate() {
            catalog_blobs.observe(unhashed, blob, row.blob);
            if row.stamp != stamp {
                stamp_override(out, ordinal, &row.stamp, &stamp);
                overrides += 1;
            }
        }
        let width = hash.width();
        // Old catalogs can lack hashes. Their checkpoint owns all blob identities.
        match catalog_blobs {
            CatalogBlobs::Missing => {
                out[header..header + SPARSE.len()].copy_from_slice(SPARSE);
                let blobs = self.append_blobs(out, width)?;
                out.extend_from_slice(&overrides.to_le_bytes());
                out.extend_from_slice(&blobs.to_le_bytes());
            }
            CatalogBlobs::Mismatch => return Err(invalid()),
            CatalogBlobs::Matching => {
                self.append_flags(out, width)?;
                out.extend_from_slice(&overrides.to_le_bytes());
            }
        }
        Ok(())
    }

    fn append_flags(&self, out: &mut Vec<u8>, width: usize) -> io::Result<()> {
        for group in self.rows.chunks(8) {
            let mut flags = 0u8;
            for (bit, row) in group.iter().enumerate() {
                row.validate_identity(width)?;
                flags |= u8::from(row.committed) << bit;
            }
            out.push(flags);
        }
        Ok(())
    }

    fn append_blobs(&self, out: &mut Vec<u8>, width: usize) -> io::Result<u64> {
        let mut blobs = 0;
        for (ordinal, row) in self.rows.iter().enumerate() {
            blobs += u64::from(row.append_blob(out, ordinal, width)?);
        }
        Ok(blobs)
    }
    #[cfg(test)]
    fn encode(
        &self,
        out: &mut Vec<u8>,
        base: impl ExactSizeIterator<Item = [u64; 7]>,
    ) -> io::Result<()> {
        self.append(out, base.map(|stamp| (stamp, None, true)))
    }

    #[cfg(test)]
    fn decode(bytes: &[u8], base: impl ExactSizeIterator<Item = [u64; 7]>) -> io::Result<Self> {
        Self::decode_catalog(bytes, base.map(|s| (s, None)))
    }
    pub(super) fn decode_catalog(
        mut bytes: &[u8],
        base: impl ExactSizeIterator<Item = ([u64; 7], Option<BlobId>)>,
    ) -> io::Result<Self> {
        let format = take(&mut bytes, 8)?;
        let hash = HashFormat::read(&mut bytes)?;
        let width = hash.width();
        let n = count(&mut bytes)?;
        if n != base.len() {
            return Err(invalid());
        }
        let rows = match format {
            f if f == CATALOG => catalog_rows(bytes, base, width)?,
            f if f == DENSE || f == SPARSE => legacy_rows(bytes, base, width, f == SPARSE)?,
            _ => return Err(invalid()),
        };
        Ok(Self {
            sha256: matches!(hash, HashFormat::Sha256),
            rows,
        })
    }
}

// Legacy checkpoints carry their own blob table. Dense versions also own every stamp.
fn legacy_rows(
    bytes: &[u8],
    base: impl ExactSizeIterator<Item = ([u64; 7], Option<BlobId>)>,
    width: usize,
    sparse: bool,
) -> io::Result<Vec<Row>> {
    let n = base.len();
    let (data, mut trailer) = split_tail(bytes, if sparse { 16 } else { 8 })?;
    let overrides = if sparse { count(&mut trailer)? } else { 0 };
    let blobs = count(&mut trailer)?;
    if blobs > n || overrides > n {
        return Err(invalid());
    }
    let (stamps, identities) = split_tail(data, blobs.checked_mul(9 + width).ok_or_else(invalid)?)?;
    let mut rows = if sparse {
        let mut rows: Vec<_> = base
            .map(|(stamp, _)| Row {
                stamp,
                blob: None,
                committed: false,
            })
            .collect();
        apply_overrides(stamps, overrides, &mut rows)?;
        rows
    } else {
        dense_rows(stamps, n)?
    };
    read_blobs(identities, &mut rows, width)?;
    Ok(rows)
}

fn dense_rows(stamps: &[u8], n: usize) -> io::Result<Vec<Row>> {
    if n.checked_mul(STAMP_BYTES) != Some(stamps.len()) {
        return Err(invalid());
    }
    Ok(stamps
        .chunks_exact(STAMP_BYTES)
        .map(|fields| {
            let stamp = crate::application::index::binary::stamp(fields.try_into().unwrap());
            Row {
                stamp,
                blob: None,
                committed: false,
            }
        })
        .collect())
}

// Both sparse sections address rows in strictly increasing catalog order.
fn next_row<'a>(
    bytes: &mut &[u8],
    rows: &'a mut [Row],
    first: &mut usize,
) -> io::Result<&'a mut Row> {
    let ordinal = count(bytes)?;
    let row = rows.get_mut(ordinal).ok_or_else(invalid)?;
    if ordinal < *first {
        return Err(invalid());
    }
    *first = ordinal + 1;
    Ok(row)
}

fn read_stamp_override(bytes: &mut &[u8], stamp: &mut [u64; 7]) -> io::Result<()> {
    let mask = take(bytes, 1)?[0];
    if mask == 0 || mask & 0x80 != 0 {
        return Err(invalid());
    }
    for i in StampFields(mask) {
        let field = &mut stamp[i];
        let next = u64::from_le_bytes(take(bytes, 8)?.try_into().unwrap());
        if *field == next {
            return Err(invalid());
        }
        *field = next;
    }
    Ok(())
}

fn apply_overrides(mut bytes: &[u8], count: usize, rows: &mut [Row]) -> io::Result<()> {
    let mut first = 0;
    for _ in 0..count {
        let row = next_row(&mut bytes, rows, &mut first)?;
        read_stamp_override(&mut bytes, &mut row.stamp)?;
    }
    if bytes.is_empty() {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn split_tail(bytes: &[u8], len: usize) -> io::Result<(&[u8], &[u8])> {
    Ok(bytes.split_at(bytes.len().checked_sub(len).ok_or_else(invalid)?))
}

fn catalog_rows(
    bytes: &[u8],
    base: impl ExactSizeIterator<Item = ([u64; 7], Option<BlobId>)>,
    width: usize,
) -> io::Result<Vec<Row>> {
    let n = base.len();
    let (data, mut trailer) = split_tail(bytes, 8)?;
    let overrides = count(&mut trailer)?;
    if overrides > n {
        return Err(invalid());
    }
    let (stamps, flags) = split_tail(data, n.div_ceil(8))?;
    let used_bits = n % 8;
    if used_bits != 0 && flags.last().is_some_and(|last| last >> used_bits != 0) {
        return Err(invalid());
    }
    let mut rows: Vec<_> = base
        .enumerate()
        .map(|(i, (stamp, blob))| {
            let committed = flags[i / 8] & (1 << (i % 8)) != 0;
            let row = Row {
                stamp,
                blob,
                committed,
            };
            row.validate_identity(width)?;
            Ok::<_, io::Error>(row)
        })
        .collect::<io::Result<_>>()?;
    apply_overrides(stamps, overrides, &mut rows)?;
    Ok(rows)
}

fn read_blobs(mut bytes: &[u8], rows: &mut [Row], width: usize) -> io::Result<()> {
    let mut first = 0;
    while !bytes.is_empty() {
        let target = next_row(&mut bytes, rows, &mut first)?;
        target.committed = match take(&mut bytes, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(invalid()),
        };
        target.blob = Some(BlobId::from_raw(take(&mut bytes, width)?));
    }
    Ok(())
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
        check_checkpoint_round_trip(false);
        check_checkpoint_round_trip(true);
    }

    fn check_checkpoint_round_trip(sha256: bool) {
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
        assert_rejects_changes(&bytes, &point, [
            (0, 0), (8, 3), (9, 4), (start, 3), (start + 8, 2), (start + stride, 0),
        ]);
        assert!(Checkpoint::decode(&bytes[..bytes.len() - 1], base()).is_err());
        assert!(Checkpoint::decode(&bytes[..start - 1], base()).is_err());
    }

    fn assert_rejects_changes(bytes: &[u8], base: &Checkpoint, changes: [(usize, u8); 6]) {
        for (i, value) in changes {
            let mut broken = bytes.to_vec();
            broken[i] = value;
            assert!(Checkpoint::decode(&broken, base.rows.iter().map(|row| row.stamp)).is_err());
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
            p.append(&mut bytes, base.iter().map(|&(s, b)| (s, b, false)))
                .unwrap();
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
    fn late_unhashed_catalog_entry_keeps_checkpoint_owned_identities() {
        let point = fixture(false);
        let mut base: Vec<_> = point
            .rows
            .iter()
            .map(|r| (r.stamp, r.blob, false))
            .collect();
        base[0].1 = None;
        assert!(point.append(&mut Vec::new(), base.iter().copied()).is_err());
        base[2].1 = None;
        base[2].2 = true;
        let mut bytes = b"manifest".to_vec();
        point.append(&mut bytes, base.iter().copied()).unwrap();
        assert_eq!(&bytes[..16], b"manifestCIXCHK02");
        assert_eq!(
            Checkpoint::decode(&bytes[8..], base.iter().map(|r| r.0)).unwrap(),
            point
        );
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
