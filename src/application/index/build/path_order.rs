//! Compare sorted paths once. File IDs can change independently of source bytes.
use super::{ObservedPath, Previous};
use std::borrow::Cow;

#[derive(Clone, Copy)]
pub(crate) enum PriorRow {
    Absent,
    Changed(usize),
    Unchanged(usize),
}

impl PriorRow {
    pub fn unchanged(self) -> Option<usize> {
        match self {
            Self::Unchanged(id) => Some(id),
            _ => None,
        }
    }

    pub fn ordinal(self) -> Option<usize> {
        match self {
            Self::Absent => None,
            Self::Changed(id) | Self::Unchanged(id) => Some(id),
        }
    }
}

pub(crate) enum PathOrder {
    Fresh,
    Stable,
    Changed(Vec<Option<usize>>),
}

impl PathOrder {
    pub fn compare(
        previous: Option<&dyn Previous>,
        files: &[ObservedPath],
        prefix: usize,
    ) -> (Self, Vec<Option<usize>>) {
        let Some(previous) = previous else {
            return (Self::Fresh, Vec::new());
        };
        let entries = previous.entries();
        let mut changed = (entries.len() != files.len()).then(|| Vec::with_capacity(files.len()));
        let mut reuse = Vec::with_capacity(files.len());
        reuse.extend((0..prefix).map(Some));
        if let Some(paths) = &mut changed { paths.extend_from_slice(&reuse); }
        for (i, old) in (prefix..).zip(super::super::prior_rows(&entries[prefix..], &files[prefix..])) {
            let old = old.map(|id| id + prefix);
            record_ordinal(&mut changed, i, old, files.len());
            reuse.push(old.filter(|&id| entries[id].stamp() == files[i].stamp));
        }
        (changed.map_or(Self::Stable, Self::Changed), reuse)
    }

    pub fn previous_at(&self, index: usize) -> Option<usize> {
        match self {
            Self::Fresh => None,
            Self::Stable => Some(index),
            Self::Changed(paths) => paths[index],
        }
    }

    pub fn row(&self, index: usize, unchanged: Option<usize>) -> PriorRow {
        if let Some(id) = unchanged {
            return PriorRow::Unchanged(id);
        }
        self.previous_at(index)
            .map_or(PriorRow::Absent, PriorRow::Changed)
    }

    pub fn stable(&self) -> bool {
        matches!(self, Self::Stable)
    }

    /// A rename may reuse parser facts and source bytes. Path-sensitive products
    /// can borrow the same mapping until an actual rename needs filtering.
    pub fn at_same_path<'a>(&self, sources: &'a [Option<usize>]) -> Cow<'a, [Option<usize>]> {
        let mut reuse = Cow::Borrowed(sources);
        if matches!(self, Self::Fresh) {
            return reuse;
        }
        for (i, &source) in sources.iter().enumerate() {
            if source.is_some() && self.previous_at(i) != source {
                reuse.to_mut()[i] = None;
            }
        }
        reuse
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_unchanged_paths_forward_path_sensitive_products() {
        let sources = [Some(0), None, Some(1), Some(2)];
        let shifted = PathOrder::Changed(vec![Some(0), None, Some(1), None]);
        assert_eq!(
            &*shifted.at_same_path(&sources),
            &[Some(0), None, Some(1), None]
        );
        assert_eq!(
            &*PathOrder::Stable.at_same_path(&sources),
            &[Some(0), None, None, None]
        );
        let stable = [Some(0), None, Some(2)];
        assert!(matches!(
            PathOrder::Stable.at_same_path(&stable),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            PathOrder::Fresh.at_same_path(&[None]),
            Cow::Borrowed(_)
        ));
    }
}

fn record_ordinal(changed: &mut Option<Vec<Option<usize>>>, index: usize, prior: Option<usize>, count: usize) {
    if prior != Some(index) && changed.is_none() {
        let mut paths = Vec::with_capacity(count);
        paths.extend((0..index).map(Some));
        *changed = Some(paths);
    }
    if let Some(paths) = changed {
        paths.push(prior);
    }
}
