//! A type selection is a last-match-wins decision on a basename. Direct
//! lookups can settle a file before any older wildcard rule is evaluated.
use super::Glob;
use crate::domain::pattern::BasenameGlob;
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(super) struct Choice {
    pub order: usize,
    pub positive: bool,
}

#[derive(Default)]
pub(super) struct Matchers {
    exact: HashMap<Box<str>, Choice>,
    suffix: HashMap<Box<str>, Choice>,
    wildcard: Vec<(Choice, Glob)>,
}

impl Matchers {
    pub fn new(patterns: Vec<(Choice, Glob)>) -> Self {
        let mut result = Self::default();
        for (choice, pattern) in patterns {
            match pattern.for_basename() {
                BasenameGlob::Exact(key) => {
                    result.exact.insert(key, choice);
                }
                BasenameGlob::Extension(key) => {
                    result.suffix.insert(key, choice);
                }
                BasenameGlob::Other(glob) => result.wildcard.push((choice, glob)),
            }
        }
        result
            .wildcard
            .sort_unstable_by_key(|(choice, _)| std::cmp::Reverse(choice.order));
        result
    }

    pub fn accepts(&self, name: &str, default: bool) -> bool {
        let exact = self.exact.get(name);
        let extension = if self.suffix.is_empty() {
            None
        } else {
            name.rfind('.')
                .and_then(|dot| self.suffix.get(&name[dot..]))
        };
        let direct = exact
            .into_iter()
            .chain(extension)
            .max_by_key(|choice| choice.order);
        let wildcard = self.wildcard.iter()
            .take_while(|(choice, _)| direct.is_none_or(|matched| matched.order < choice.order))
            .find(|(_, pattern)| pattern.is_match(name))
            .map(|(choice, _)| choice);
        wildcard.or(direct).map_or(default, |choice| choice.positive)
    }
}
