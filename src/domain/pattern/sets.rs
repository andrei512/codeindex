use super::unicode;
use std::sync::Arc;
#[derive(Clone, Debug)]
pub(super) struct Set(pub Arc<[(u32, u32)]>);
#[derive(Clone, Copy)]
pub(super) enum Op {
    Union,
    Intersection,
    Difference,
    Xor,
}
impl Set {
    pub fn ranges(mut pairs: Vec<(u32, u32)>) -> Self {
        pairs.sort_unstable();
        let mut used = 0;
        for i in 0..pairs.len() {
            let (a, b) = pairs[i];
            if used != 0 && pairs[used - 1].1 + 1 >= a {
                pairs[used - 1].1 = pairs[used - 1].1.max(b)
            } else {
                pairs[used] = (a, b);
                used += 1
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
        let mut ranges = vec![];
        while let Some(n) = left.peek().into_iter().chain(right.peek()).min().copied() {
            let enabled = match op {
                Op::Union => a || b,
                Op::Intersection => a && b,
                Op::Difference => a && !b,
                Op::Xor => a != b,
            };
            if enabled && prev < n {
                ranges.push((prev, n - 1))
            }
            if left.peek() == Some(&n) {
                left.next();
                a = !a
            }
            if right.peek() == Some(&n) {
                right.next();
                b = !b
            }
            prev = n;
        }
        Self::ranges(ranges)
    }
    pub fn complement(&self) -> Self {
        Self::ranges(vec![(0, 0x10ffff)]).combine(self, Op::Difference)
    }
    pub fn folded(&self) -> Self {
        let mut ranges = self.0.to_vec();
        for &(c, alts) in unicode::folds() {
            if self.contains(c) {
                ranges.extend(alts.iter().map(|&a| (a as u32, a as u32)))
            }
        }
        Self::ranges(ranges)
    }
}
