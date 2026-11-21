//! Footer tables index variable-length records in a sealed product extent.
use super::{
    binary::{bad, put},
    layout::ExtentSink,
};
use crate::domain::content_lookup::ExtentRead;
use std::{io, ops::Range};
pub(super) fn finish(w: &mut impl ExtentSink, start: u64, tables: [Vec<u64>; 2]) -> io::Result<()> {
    let directory = w.offset() - start;
    for offsets in tables {
        put(w, offsets.len())?;
        for off in offsets {
            w.write_all(&off.to_le_bytes())?;
        }
    }
    w.write_all(&directory.to_le_bytes())
}
pub(super) fn word(r: &mut impl ExtentRead, offset: u64) -> io::Result<u64> {
    let mut b = [0; 8];
    r.read(offset, &mut b)?;
    Ok(u64::from_le_bytes(b))
}
pub(super) fn table(r: &mut impl ExtentRead, pos: &mut u64) -> io::Result<Range<u64>> {
    let count = word(r, *pos)?;
    let start = pos.checked_add(8).ok_or_else(bad)?;
    *pos = count
        .checked_mul(8)
        .and_then(|n| start.checked_add(n))
        .filter(|&end| end <= r.length())
        .ok_or_else(bad)?;
    if count == 0 {
        return Err(bad());
    }
    Ok(start..*pos)
}
pub(super) fn row(
    r: &mut impl ExtentRead,
    table: &Range<u64>,
    id: u64,
    row_end: u64,
) -> io::Result<Range<u64>> {
    if id >= (table.end - table.start) / 8 - 1 {
        return Err(bad());
    }
    let at = table.start + 8 * id;
    let from = word(r, at)?;
    let to = word(r, at + 8)?;
    if from > to || to > row_end {
        return Err(bad());
    }
    Ok(from..to)
}
pub(super) fn text(r: &mut impl ExtentRead, at: &mut u64, end: u64) -> io::Result<String> {
    let size = word(r, *at)?;
    let start = at.checked_add(8).ok_or_else(bad)?;
    *at = start
        .checked_add(size)
        .filter(|&n| n <= end)
        .ok_or_else(bad)?;
    let mut b = vec![0; usize::try_from(size).map_err(|_| bad())?];
    r.read(start, &mut b)?;
    String::from_utf8(b).map_err(|_| bad())
}
pub(super) fn read_bytes(r: &mut impl ExtentRead, range: Range<u64>) -> io::Result<Vec<u8>> {
    let mut b = vec![0; usize::try_from(range.end - range.start).map_err(|_| bad())?];
    r.read(range.start, &mut b)?;
    Ok(b)
}
