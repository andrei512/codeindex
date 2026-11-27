//! A scope usually has one declaration with a given spelling.
use super::Binding;
use crate::domain::code::cache::{Wire, bad};
use std::{io, ops::Deref};

#[derive(Default)]
pub(super) enum Bindings {
    #[default]
    Empty,
    One(Binding),
    // Only locals with strictly increasing visibility permit a backward first-match lookup.
    Shadowed(Vec<Binding>),
    Many(Vec<Binding>),
}

impl Bindings {
    /// Highest-priority visible declarations, in source order. A declaration
    /// masks lower-priority names even if its own target cannot be resolved.
    pub(super) fn select<T>(
        &self,
        offset: usize,
        same_owner: bool,
        eligible: impl Fn(&Binding) -> bool,
        project: impl Fn(&Binding) -> Option<T>,
    ) -> Vec<T> {
        match self {
            Self::Empty => Vec::new(),
            Self::One(b) => b
                .visible(offset, same_owner)
                .filter(|b| eligible(b))
                .and_then(project)
                .into_iter()
                .collect(),
            Self::Shadowed(_) if !same_owner => Vec::new(),
            Self::Shadowed(chain) => {
                let end = chain.partition_point(|b| b.visibility <= offset);
                chain[..end]
                    .iter()
                    .rev()
                    .find(|b| eligible(b))
                    .and_then(project)
                    .into_iter()
                    .collect()
            }
            Self::Many(items) => select_tied(
                items.iter().filter_map(|b| b.visible(offset, same_owner)),
                eligible,
                project,
            ),
        }
    }

    pub(super) fn push(&mut self, binding: Binding) {
        *self = match self {
            Self::Empty => Self::One(binding),
            Self::One(first) => Self::pair(*first, binding),
            Self::Shadowed(items) if !shadows(*items.last().unwrap(), binding) => {
                items.push(binding);
                Self::Many(std::mem::take(items))
            }
            Self::Shadowed(items) | Self::Many(items) => {
                items.push(binding);
                return;
            }
        };
    }

    fn pair(first: Binding, second: Binding) -> Self {
        let items = vec![first, second];
        if shadows(first, second) {
            Self::Shadowed(items)
        } else {
            Self::Many(items)
        }
    }
}

impl Binding {
    fn visible(&self, offset: usize, same_owner: bool) -> Option<&Self> {
        (self.visibility <= offset && (!self.local || same_owner)).then_some(self)
    }
}

fn select_tied<'a, T>(
    visible: impl Iterator<Item = &'a Binding>,
    eligible: impl Fn(&Binding) -> bool,
    project: impl Fn(&Binding) -> Option<T>,
) -> Vec<T> {
    let mut result = Vec::new();
    let mut best = (false, 0);
    for binding in visible {
        let priority = (binding.local, binding.visibility);
        if priority < best || !eligible(binding) {
            continue;
        }
        reset_if_outranked(&mut result, &mut best, priority);
        result.extend(project(binding));
    }
    result
}

fn reset_if_outranked<T>(result: &mut Vec<T>, best: &mut (bool, usize), priority: (bool, usize)) {
    if priority > *best {
        result.clear();
        *best = priority;
    }
}

fn shadows(earlier: Binding, later: Binding) -> bool {
    earlier.local && later.local && earlier.visibility < later.visibility
}

impl Deref for Bindings {
    type Target = [Binding];
    fn deref(&self) -> &[Binding] {
        match self {
            Self::Empty => &[],
            Self::One(item) => std::slice::from_ref(item),
            Self::Shadowed(items) | Self::Many(items) => items,
        }
    }
}

impl<'a> IntoIterator for &'a Bindings {
    type Item = &'a Binding;
    type IntoIter = std::slice::Iter<'a, Binding>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Wire for Bindings {
    fn save(&self, out: &mut impl crate::domain::code::cache::Output) {
        self.len().save(out);
        for binding in self {
            if out.overflowed() {
                break;
            }
            binding.save(out);
        }
    }
    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let len = usize::load(input)?;
        if len > input.len() {
            return Err(bad());
        }
        if len == 0 {
            return Ok(Self::Empty);
        }
        let mut last = Binding::load(input)?;
        if len == 1 {
            return Ok(Self::One(last));
        }
        let mut items = Vec::with_capacity(len);
        items.push(last);
        let mut ordered = true;
        for _ in 1..len {
            let binding = Binding::load(input)?;
            ordered &= shadows(last, binding);
            items.push(binding);
            last = binding;
        }
        Ok(if ordered {
            Self::Shadowed(items)
        } else {
            Self::Many(items)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_winners_mask_older_sites_but_keep_equal_priority_ties() {
        let mut bindings = Bindings::default();
        for (decl, (local, visibility)) in
            [(false, 0), (true, 2), (true, 2)].into_iter().enumerate()
        {
            bindings.push(Binding {
                decl,
                local,
                visibility,
                ns: super::super::Namespace::Value,
            });
        }
        let target = |b: &Binding| (b.decl != 1).then_some(b.decl);
        assert_eq!(bindings.select(1, true, |_| true, target), [0]);
        assert_eq!(bindings.select(3, false, |_| true, target), [0]);
        assert!(bindings.select(3, true, |b| b.decl != 2, target).is_empty());
        assert_eq!(bindings.select(3, true, |_| true, target), [2]);
    }

    #[test]
    fn ordered_local_chain_survives_reload_and_keeps_ties_in_source_order() {
        let mut bindings = Bindings::default();
        for (decl, visibility) in [4, 8, 12, 12, 16].into_iter().enumerate() {
            bindings.push(Binding {
                decl,
                visibility,
                local: true,
                ns: super::super::Namespace::Value,
            });
            let mut bytes = Vec::new();
            bindings.save(&mut bytes);
            let restored = Bindings::load(&mut bytes.as_slice()).unwrap();
            let ordered = matches!(decl, 1 | 2);
            assert_eq!(matches!(bindings, Bindings::Shadowed(_)), ordered);
            assert_eq!(matches!(restored, Bindings::Shadowed(_)), ordered);
            assert!(restored.iter().map(|b| b.decl).eq(0..=decl));
        }
    }
}
