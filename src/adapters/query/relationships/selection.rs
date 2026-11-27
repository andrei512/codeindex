//! At a cursor position only the shortest matching span contributes targets.
//! Equal spans can contribute several roles, including an unresolved role.
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Roles = BTreeMap<&'static str, BTreeSet<usize>>;
pub(super) struct Selection {
    pub contexts: BTreeMap<usize, Roles>,
    pub targets: BTreeSet<usize>,
    pub roles: Option<BTreeMap<&'static str, BTreeSet<usize>>>,
    shortest: usize,
}

impl Selection {
    pub fn new(include_roles: bool) -> Self {
        Self {
            contexts: BTreeMap::new(),
            targets: BTreeSet::new(),
            roles: include_roles.then(BTreeMap::new),
            shortest: usize::MAX,
        }
    }

    pub fn scoped(
        &mut self,
        length: usize,
        ids: &[usize],
        role: &'static str,
        context: Option<usize>,
    ) {
        self.consider(length, ids, role);
        if length == self.shortest
            && let Some(ctx) = context
        {
            self.contexts
                .entry(ctx)
                .or_default()
                .entry(role)
                .or_default()
                .extend(ids);
        }
    }

    /// An unscoped declaration has one implicit context. A scoped declaration
    /// contributes evidence once for each context that bound its target.
    pub fn definition_contexts(&self, id: usize) -> impl Iterator<Item = Option<usize>> + '_ {
        self.contexts.is_empty().then_some(None).into_iter().chain(
            self.contexts.iter().filter_map(move |(&ctx, roles)| {
                roles.values().any(|ids| ids.contains(&id)).then_some(Some(ctx))
            })
        )
    }

    pub fn accepts(&self, id: usize, ctx: Option<usize>) -> bool {
        self.contexts.is_empty()
            || ctx
                .and_then(|c| self.contexts.get(&c))
                .is_some_and(|roles| roles.values().any(|ids| ids.contains(&id)))
    }

    pub fn consider(&mut self, length: usize, ids: &[usize], role: &'static str) {
        if length > self.shortest {
            return;
        }
        if length < self.shortest {
            self.contexts.clear();
            self.targets.clear();
            if let Some(roles) = &mut self.roles {
                roles.clear();
            }
            self.shortest = length;
        }
        self.targets.extend(ids);
        if let Some(roles) = &mut self.roles {
            roles.entry(role).or_default().extend(ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortest_span_replaces_roles_and_keeps_ties() {
        for include_roles in [true, false] {
            let mut selection = Selection::new(include_roles);
            selection.consider(4, &[1], "reference");
            selection.consider(2, &[], "call_target");
            selection.consider(3, &[2], "reference");
            selection.consider(2, &[3, 3], "definition");
            assert_eq!(selection.targets, BTreeSet::from([3]));
            assert_eq!(
                selection.roles,
                include_roles.then(|| BTreeMap::from([
                    ("call_target", BTreeSet::new()),
                    ("definition", BTreeSet::from([3]))
                ]))
            );
        }
    }
}
