//! Resolve each module's parent chain once, after configuration selects its edges.
use super::{Address, HashMap, ResolverState};
use std::collections::hash_map::Entry;

impl ResolverState<'_, '_> {
    pub(super) fn index_module_roots(&mut self) {
        let mut chain = Vec::new();
        let modules = self.units.iter().enumerate().flat_map(|(file, unit)| {
            unit.analysis.rust.iter().flat_map(move |evidence| {
                evidence
                    .scopes
                    .iter()
                    .enumerate()
                    .filter_map(move |(scope, item)| {
                        (scope == item.module).then_some(((file, scope), item))
                    })
            })
        });
        for (address, item) in modules {
            record_root(address, &self.parents, &mut self.roots, &mut chain);
            let invariant = item
                .names
                .values()
                .flatten()
                .all(|b| !b.local && b.visibility == 0);
            self.invariant_modules.extend(invariant.then_some(address));
        }
    }
}

fn record_root(
    mut address: Address,
    parents: &HashMap<Address, Option<Address>>,
    roots: &mut HashMap<Address, Option<Address>>,
    chain: &mut Vec<Address>,
) {
    let root = loop {
        match visit_parent(address, parents, roots) {
            std::ops::ControlFlow::Break(root) => break root,
            std::ops::ControlFlow::Continue(parent) => {
                chain.push(address);
                address = parent;
            }
        }
    };
    let Some(root) = root else {
        chain.clear();
        return;
    };
    roots.extend(chain.drain(..).map(|child| (child, Some(root))));
}

fn visit_parent(
    address: Address,
    parents: &HashMap<Address, Option<Address>>,
    roots: &mut HashMap<Address, Option<Address>>,
) -> std::ops::ControlFlow<Option<Address>, Address> {
    use std::ops::ControlFlow::{Break, Continue};
    let slot = match roots.entry(address) {
        // An unresolved address already in this chain represents a cycle.
        Entry::Occupied(known) => return Break(*known.get()),
        Entry::Vacant(slot) => slot,
    };
    let edge = parents.get(&address);
    let root = edge.is_none().then_some(address);
    slot.insert(root);
    edge.copied().flatten().map_or(Break(root), Continue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_known_roots_and_propagates_cycles_and_ambiguous_parents() {
        let a = |n| (n, 0);
        let parents = HashMap::from([
            (a(1), Some(a(0))),
            (a(2), Some(a(1))),
            (a(3), Some(a(1))),
            (a(4), Some(a(5))),
            (a(5), Some(a(4))),
            (a(6), Some(a(4))),
            (a(7), None),
            (a(8), Some(a(7))),
        ]);
        let mut roots = HashMap::new();
        let mut chain = Vec::new();
        for n in [2, 3, 6, 8, 0, 4, 1] {
            record_root(a(n), &parents, &mut roots, &mut chain);
            assert!(chain.is_empty());
        }
        for n in 0..9 {
            assert_eq!(roots[&a(n)], (n < 4).then_some(a(0)));
        }
    }
}
