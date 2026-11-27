//! Lexical scopes map names to declarations. External owners can have a scope
//! without a declaration when only a qualified definition is present in the file.
use super::{Analyzer, Binding, Declaration, Role, Scope};

#[derive(Clone, Copy)]
struct Visibility {
    at: usize,
    role: Option<Role>,
}

pub(super) struct Candidates<'s, 'a> {
    bindings: &'s [Binding<'a>],
    indices: std::slice::Iter<'s, usize>,
    visibility: Option<Visibility>,
}

impl<'s, 'a> Iterator for Candidates<'s, 'a> {
    type Item = &'s Binding<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let binding = |&id| &self.bindings[id];
        self.indices.by_ref().map(binding).find(|candidate| {
            self.visibility.is_none_or(|visible| {
                candidate.visible <= visible.at && visible.role.is_none_or(|role| role == candidate.role)
            })
        })
    }
}

impl<'a> Analyzer<'a, '_> {
    pub(super) fn scope(&mut self, parent: usize, owner: Option<usize>) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            owner: owner.or(self.scopes[parent].owner),
            container: self.scopes[parent].container,
            const_receiver: self.scopes[parent].const_receiver,
            ..Scope::default()
        });
        id
    }
    pub(super) fn container_scope(&mut self, parent: usize) -> usize {
        let child = self.scope(parent, None);
        self.scopes[child].container = Some(child);
        child
    }

    pub(super) fn namespace_scope(&mut self, parent: usize, name: usize) -> usize {
        let members = self.ids(parent, self.word(name));
        if let Some(body) = members.iter().map(|&id| &self.bindings[id])
            .find(|b| b.role == Role::Namespace).and_then(|b| b.body)
        {
            self.declared[name] = true;
            return body;
        }
        let body = self.container_scope(parent);
        self.add(name, parent, "local_binding", Role::Namespace).body = Some(body);
        body
    }

    pub(super) fn ids(&self, scope: usize, name: &str) -> &[usize] {
        self.tables.ids(&self.scopes[scope].names, name)
    }
    pub(super) fn members(&self, owner: Option<usize>, name: &str) -> std::iter::Peekable<Candidates<'_, 'a>> {
        let indices = owner.map_or(&[][..], |owner| self.ids(owner, name));
        self.candidates(indices, None).peekable()
    }

    fn candidates<'s>(&'s self, ids: &'s [usize], visibility: Option<Visibility>) -> Candidates<'s, 'a> {
        Candidates { bindings: &self.bindings, indices: ids.iter(), visibility }
    }

    /// Stop at the first scope containing a visible binding in this namespace.
    /// A caller asking for one binding never allocates an overload list.
    pub(super) fn lookup<'s>(
        &'s self,
        mut scope: usize,
        name: &str,
        role: Option<Role>,
        at: usize,
    ) -> std::iter::Peekable<Candidates<'s, 'a>> {
        let visible = Some(Visibility { at, role });
        let mut choices = self.candidates(self.ids(scope, name), visible).peekable();
        while let Some(parent) = self.scopes[scope]
            .parent
            .filter(|_| choices.peek().is_none())
        {
            scope = parent;
            choices = self.candidates(self.ids(scope, name), visible).peekable();
        }
        choices
    }
    pub(super) fn add(
        &mut self,
        i: usize,
        scope: usize,
        kind: &'static str,
        role: Role,
    ) -> &mut Binding<'a> {
        let id = self.declaration(i, kind, role).id;
        self.tables.add(&mut self.scopes[scope].names, self.t[i].text, id);
        &mut self.bindings[id]
    }

    /// A prototype parameter has a declaration site but no body in which to resolve its name.
    pub(super) fn declaration(&mut self, i: usize, kind: &'static str, role: Role) -> &mut Binding<'a> {
        let id = self.out.declarations.len();
        self.declared[i] = true;
        self.out.declarations.push(Declaration {
            span: self.t[i].span.clone(),
            kind,
        });
        let visible = if self.grammar.forward_declarations_visible && role != Role::Value
        {
            0
        } else {
            i
        };
        debug_assert_eq!(id, self.bindings.len());
        self.bindings.push(Binding {
            id,
            role,
            visible,
            ty: None,
            is_const: false,
            definition: false,
            body: None,
            signature: None,
            usable: true,
        });
        self.bindings.last_mut().unwrap()
    }
    pub(super) fn external_at(&self, scope: usize, name: &str) -> Option<usize> {
        self.external_owners.get(&scope)?.get(name).copied()
    }

    pub(super) fn create_external_scope(
        &mut self,
        parent: usize,
        name: std::borrow::Cow<'a, str>,
    ) -> usize {
        let body = self.container_scope(parent);
        self.external_owners
            .entry(parent)
            .or_default()
            .insert(name, body);
        body
    }
    pub(super) fn external_owner(&self, mut scope: usize, name: &str) -> Option<usize> {
        if self.external_owners.is_empty() {
            return None;
        }
        loop {
            if let Some(owner) = self.external_at(scope, name) {
                return Some(owner);
            }
            scope = self.scopes[scope].parent?;
        }
    }
    pub(super) fn brace_scope(&mut self, parent: usize, open: usize, previous: &str) -> usize {
        // An unnamed C++ namespace exposes members in the enclosing scope.
        if self.grammar.anonymous_namespace && previous == "namespace" {
            return parent;
        }
        if let Some(body) = self.body.remove(&open) {
            return body;
        }
        // No declaration or use can observe a new scope in an empty block.
        if self.word(open + 1) == "}" && !self.skip[open + 1] {
            return parent;
        }
        self.scope(parent, None)
    }
}
