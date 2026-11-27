pub mod content;
mod document;
mod facts;

pub use document::{BlobId, Document, FileId, ObservedPath, Span};
pub use facts::{Capability, Coverage, Fact, Product};
pub mod code;
pub mod content_lookup;
pub mod markdown;
pub mod pattern;
pub mod ranked;

/// Byte-identical ends of a replacement, with the suffix disjoint from the prefix.
pub(crate) fn matching_edges(old: &[u8], new: &[u8]) -> (usize, usize) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, suffix)
}

/// Find a sorted prefix boundary from a monotonic cursor. Exponential probing
/// keeps the number of comparisons proportional to the log of the consumed
/// prefix length even when the unconsumed suffix is large.
pub(crate) fn prefix_len<T>(items: &[T], before: impl Fn(&T) -> bool) -> usize {
    let mut upper = 0usize;
    while items.get(upper).is_some_and(&before) {
        upper = upper.saturating_mul(2).saturating_add(1);
    }
    let lower = upper / 2 + usize::from(upper != 0);
    lower + items[lower..upper.min(items.len())].partition_point(before)
}

/// Find the first slot whose key is at least the requested key. Each ordered
/// probe may fail while reading or decoding the corresponding on-disk record.
pub(crate) fn lower_bound(
    mut slots: std::ops::Range<u64>,
    mut before: impl FnMut(u64) -> std::io::Result<bool>,
) -> std::io::Result<u64> {
    while slots.start < slots.end {
        let midpoint = slots.start + (slots.end - slots.start) / 2;
        slots = lower_bound_half(slots, midpoint, before(midpoint)?);
    }
    Ok(slots.start)
}

fn lower_bound_half(slots: std::ops::Range<u64>, midpoint: u64, before: bool) -> std::ops::Range<u64> {
    if before { midpoint + 1..slots.end } else { slots.start..midpoint }
}

#[cfg(test)]
mod tests {
    #[test]
    fn prefix_boundaries_match_binary_search_around_each_probe() {
        for len in [0, 1, 2, 3, 4, 7, 8, 9, 16, 31, 32, 256, 257] {
            let sorted: Vec<_> = (0..len).collect();
            for split in 0..=len {
                assert_eq!(super::prefix_len(&sorted, |&x| x < split), split);
            }
        }
    }
}
