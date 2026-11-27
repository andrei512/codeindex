use super::unicode;
use std::sync::Arc;
#[derive(Clone, Debug)]
pub(super) struct Set(pub Arc<[(u32, u32)]>);
#[derive(Clone, Copy)]
pub(super) enum Op {
    #[cfg(test)]
    Union,
    Intersection,
    Difference,
    Xor,
}
impl Op {
    fn accepts(self, left: bool, right: bool) -> bool {
        let mask = match self {
            #[cfg(test)]
            Self::Union => 0b1110,
            Self::Intersection => 0b1000,
            Self::Difference => 0b0100,
            Self::Xor => 0b0110,
        };
        let bit = 2 * u8::from(left) + u8::from(right);
        mask & (1 << bit) != 0
    }
}

type Span = (u32, u32);

fn merge(last: Option<&mut Span>, (start, end): Span) -> bool {
    let Some(last) = last.filter(|last| last.1 + 1 >= start) else { return false };
    last.1 = last.1.max(end);
    true
}

fn append_range(ranges: &mut Vec<Span>, span: Span) {
    if !merge(ranges.last_mut(), span) {
        ranges.push(span);
    }
}

fn toggles_at(points: &mut std::iter::Peekable<impl Iterator<Item = u32>>, offset: u32) -> bool {
    points.next_if_eq(&offset).is_some()
}

fn emit_interval(ranges: &mut Vec<Span>, start: u32, end: u32, enabled: bool) {
    if enabled && start < end {
        append_range(ranges, (start, end - 1));
    }
}

impl Set {
    pub fn ranges(mut pairs: Vec<(u32, u32)>) -> Self {
        pairs.sort_unstable();
        let mut used = 0usize;
        for i in 0..pairs.len() {
            let span = pairs[i];
            let last = used.checked_sub(1).map(|slot| &mut pairs[slot]);
            if !merge(last, span) {
                pairs[used] = span;
                used += 1;
            }
        }
        pairs.truncate(used);
        Self(pairs.into())
    }
    pub fn chars(pairs: &[(char, char)]) -> Self {
        Self::ranges(pairs.iter().map(|&(a, b)| (a as u32, b as u32)).collect())
    }
    pub fn contains(&self, c: char) -> bool {
        let n = c as u32;
        self.0
            .get(self.0.partition_point(|&(_, b)| b < n))
            .is_some_and(|&(a, _)| a <= n)
    }
    pub fn combine(&self, other: &Self, op: Op) -> Self {
        fn points(set: &Set) -> impl Iterator<Item = u32> + '_ {
            set.0.iter().flat_map(|&(a, b)| [a, b + 1])
        }
        let mut left = points(self).peekable();
        let mut right = points(other).peekable();
        let (mut a, mut b, mut prev) = (false, false, 0);
        let mut ranges: Vec<(u32, u32)> = vec![];
        while let Some(n) = left.peek().into_iter().chain(right.peek()).min().copied() {
            emit_interval(&mut ranges, prev, n, op.accepts(a, b));
            a ^= toggles_at(&mut left, n);
            b ^= toggles_at(&mut right, n);
            prev = n;
        }
        Self(ranges.into())
    }
    pub fn complement(&self) -> Self {
        let mut gaps = Vec::with_capacity(self.0.len() + 1);
        let mut start = 0;
        for &(lo, hi) in self.0.iter() {
            if start < lo {
                gaps.push((start, lo - 1));
            }
            start = hi + 1;
        }
        if start <= char::MAX as u32 {
            gaps.push((start, char::MAX as u32));
        }
        Self(gaps.into())
    }

    pub fn folded(self) -> Self {
        let mut table = unicode::folds();
        let mut additions = Vec::new();
        for &(lo, hi) in self.0.iter() {
            let first = table.partition_point(|&(c, _)| (c as u32) < lo);
            table = &table[first..];
            let last = table.partition_point(|&(c, _)| c as u32 <= hi);
            let (in_range, remaining) = table.split_at(last);
            additions.extend(in_range.iter().flat_map(|(_, alternatives)| alternatives.iter())
                .map(|&scalar| (scalar as u32, scalar as u32)));
            table = remaining;
            if table.is_empty() {
                break;
            }
        }
        if additions.is_empty() {
            return self;
        }
        additions.extend_from_slice(&self.0);
        Self::ranges(additions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_operations_return_canonical_ranges_for_touching_endpoints() {
        let set = |mask: u8| {
            Set::ranges(
                (0..5)
                    .filter(|n| mask & (1 << n) != 0)
                    .map(|n| (n, n))
                    .collect(),
            )
        };
        for x in 0..32 {
            for y in 0..32 {
                let (a, b) = (set(x), set(y));
                for (op, want) in [
                    (Op::Union, x | y),
                    (Op::Intersection, x & y),
                    (Op::Difference, x & !y),
                    (Op::Xor, x ^ y),
                ] {
                    assert_eq!(a.combine(&b, op).0, set(want).0);
                }
            }
            let a = set(x);
            assert_eq!(a.complement().complement().0, a.0);
            assert!(a.complement().contains(char::MAX));
        }
    }
}
