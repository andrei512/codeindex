//! Collect TypeScript overload declarations until the implementation is known.
use super::*;

#[derive(Default)]
pub(super) struct Overloads<'s>(HashMap<(usize, &'s str), Vec<usize>>);

impl<'s> Overloads<'s> {
    pub(super) fn remember(&mut self, scope: usize, name: &'s str, declaration: usize) {
        self.0.entry((scope, name)).or_default().push(declaration);
    }

    pub(super) fn bind(self, a: &mut Analysis, e: &mut Evidence) {
        for ((scope, name), mut ids) in self.0 {
            let base = match e.scopes[scope].bindings.get(name) {
                Some(Binding::Local(id)) => *id,
                None => {
                    let id = ids.remove(0);
                    a.declarations[id].kind = "function_declaration";
                    e.declare(scope, name, Binding::Local(id));
                    id
                }
                _ => continue,
            };
            e.families.insert(base, ids);
        }
    }
}
