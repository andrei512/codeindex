//! A sparse declaration-to-exporter-label table. Package prefixes are stored once.
use super::{
    binary::{bad, string},
    layout::ExtentSink,
    offsets::{text, word},
};
use crate::domain::{code::Site, content_lookup::ExtentRead};
use std::{collections::HashMap, io, ops::Range};

pub(super) fn write(
    w: &mut impl ExtentSink,
    start: u64,
    base: &[usize],
    labels: &HashMap<Site, String>,
) -> io::Result<()> {
    let mut labels: Vec<_> = labels
        .iter()
        .map(|(s, name)| (base[s.file] + s.local, name))
        .collect();
    labels.sort_unstable_by_key(|(id, _)| *id);
    let mut prefixes = HashMap::new();
    let mut rows = Vec::with_capacity(labels.len());
    for (id, label) in labels {
        let split = label.rfind(' ').map_or(0, |i| i + 1);
        let (prefix, suffix) = label.split_at(split);
        let prefix_offset = write_prefix(w, start, prefix, prefixes.entry(prefix))?;
        let suffix_offset = w.offset() - start;
        string(w, suffix)?;
        rows.push([id as u64, prefix_offset, suffix_offset]);
    }
    let index = w.offset() - start;
    for row in rows {
        for n in row {
            w.write_all(&n.to_le_bytes())?;
        }
    }
    w.write_all(&index.to_le_bytes())
}
pub(super) fn directory(r: &mut impl ExtentRead, end: u64) -> io::Result<Range<u64>> {
    let tail = end.checked_sub(8).ok_or_else(bad)?;
    let index = word(r, tail)?;
    if index > tail || !(tail - index).is_multiple_of(24) {
        return Err(bad());
    }
    Ok(index..tail)
}
pub(super) fn lookup(
    r: &mut impl ExtentRead,
    index: &Range<u64>,
    id: u64,
) -> io::Result<Option<String>> {
    let (mut lo, mut hi) = (0, (index.end - index.start) / 24);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let entry = index.start + mid * 24;
        match word(r, entry)?.cmp(&id) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                let mut prefix = word(r, entry + 8)?;
                let mut suffix = word(r, entry + 16)?;
                let mut label = text(r, &mut prefix, index.start)?;
                label.push_str(&text(r, &mut suffix, index.start)?);
                return Ok(Some(label));
            }
        }
    }
    Ok(None)
}

fn write_prefix<'a>(
    w: &mut impl ExtentSink,
    start: u64,
    prefix: &'a str,
    entry: std::collections::hash_map::Entry<'a, &str, u64>,
) -> io::Result<u64> {
    match entry {
        std::collections::hash_map::Entry::Occupied(entry) => Ok(*entry.get()),
        std::collections::hash_map::Entry::Vacant(entry) => {
            let offset = w.offset() - start;
            string(w, prefix)?;
            Ok(*entry.insert(offset))
        }
    }
}
