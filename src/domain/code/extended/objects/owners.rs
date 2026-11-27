//! Map token positions to the most recently declared containing callable.
use super::ROOT;
use std::ops::Range;

#[derive(Default)]
pub(super) enum Owners {
    #[default]
    Empty,
    Disjoint(Vec<(Range<usize>, usize)>),
    Overlapping(Vec<usize>),
}

impl Owners {
    pub fn add(&mut self, body: Range<usize>, id: usize, tokens: usize) {
        if body.is_empty() {
            return;
        }
        match self {
            Self::Empty => *self = Self::Disjoint(vec![(body, id)]),
            Self::Disjoint(runs) if runs.last().is_some_and(|(r, _)| r.end <= body.start) => {
                runs.push((body, id));
            }
            Self::Disjoint(runs) => {
                let mut dense = expand(runs.drain(..), tokens);
                dense[body].fill(id);
                *self = Self::Overlapping(dense);
            }
            Self::Overlapping(dense) => dense[body].fill(id),
        }
    }

    /// Each consumer asks in ascending token order, possibly skipping positions.
    pub fn cursor(&self) -> impl FnMut(usize) -> usize + '_ {
        let mut next = 0;
        move |at| match self {
            Self::Empty => ROOT,
            Self::Overlapping(dense) => dense[at],
            Self::Disjoint(runs) => disjoint_owner(runs, &mut next, at),
        }
    }
}

fn expand(runs: impl Iterator<Item = (Range<usize>, usize)>, tokens: usize) -> Vec<usize> {
    let mut dense = vec![ROOT; tokens];
    for (range, owner) in runs {
        dense[range].fill(owner);
    }
    dense
}

fn disjoint_owner(runs: &[(Range<usize>, usize)], next: &mut usize, at: usize) -> usize {
    let remaining = &runs[*next..];
    *next += remaining
        .iter()
        .take_while(|(range, _)| range.end <= at)
        .count();
    runs.get(*next)
        .filter(|(range, _)| range.start <= at)
        .map_or(ROOT, |(_, owner)| *owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_disjoint_nested_and_out_of_order_bodies_match_overwriting_ranges() {
        let ranges: Vec<_> = (0..=5).flat_map(|a| (a..=6).map(move |b| a..b)).collect();
        for first in &ranges {
            for second in &ranges {
                check_owners([first.clone(), second.clone(), 4..6]);
            }
        }
    }

    fn check_owners(ranges: [Range<usize>; 3]) {
        let mut dense = [ROOT; 7];
        let mut owners = Owners::default();
        for (id, range) in ranges.into_iter().enumerate() {
            dense[range.clone()].fill(id);
            owners.add(range, id, 7);
        }
        for stride in [1, 2, 3] {
            let mut get = owners.cursor();
            let expected = (0..7).step_by(stride).map(|i| dense[i]);
            assert!((0..7).step_by(stride).map(&mut get).eq(expected));
        }
    }
}
