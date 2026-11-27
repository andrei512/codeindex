//! Member chains share the resolved owner of their immediately preceding use.
use super::{Analyzer, Role, ident, unique};

#[derive(Clone, Copy)]
enum Owner {
    Lexical,
    Member(usize),
    Missing,
}

impl Owner {
    fn scope(self) -> Option<usize> {
        match self {
            Self::Member(id) => Some(id),
            Self::Lexical | Self::Missing => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Prefix {
    member: usize,
    scope: usize,
    owner: Owner,
}

#[derive(Clone, Copy)]
pub(super) enum Access {
    Dot,
    Arrow,
    Scope,
}
enum BindingChoice<'r, 'a> {
    Unique(&'r super::Binding<'a>),
    External(Option<usize>),
    Ambiguous,
}

impl<'a> Analyzer<'a, '_> {
    pub(super) fn access(&self, i: usize, previous: &str) -> Option<Access> {
        let before = i.checked_sub(2).map(|at| self.word(at));
        match (previous, before) {
            (".", _) => Some(Access::Dot),
            (">", Some("-") | None) => Some(Access::Arrow),
            (":", Some(":")) => Some(Access::Scope),
            _ => None,
        }
    }
    pub(super) fn qualifier(&self, i: usize) -> Option<usize> {
        let distance = match self.access(i, self.prev(i))? {
            Access::Dot => 2,
            Access::Arrow | Access::Scope => 3,
        };
        i.checked_sub(distance)
    }
    pub(super) fn receiver(&self, i: usize, scope: usize) -> Option<Option<usize>> {
        self.chain_receiver(i, scope, &mut None)
    }

    pub(super) fn chain_receiver(
        &self,
        i: usize,
        scope: usize,
        prefix: &mut Option<Prefix>,
    ) -> Option<Option<usize>> {
        let last = self.qualifier(i)?;
        if self.grammar.temporary_receiver && self.word(last) == ")" {
            return self.temporary_owner(last, scope, i);
        }
        // Visibility is position-dependent. Reuse only across an adjacent
        // separator in the same scope, with no newly visible declaration.
        let reusable = prefix.filter(|p| self.valid_prefix(*p, last, scope, i));
        let owner = reusable.map_or_else(
            || self.resolve_chain(last, scope, i),
            |p| self.advance_owner(p.owner, last, scope, i),
        );
        *prefix = Some(Prefix {
            member: i,
            scope,
            owner,
        });
        Some(owner.scope())
    }

    fn temporary_owner(&self, last: usize, scope: usize, at: usize) -> Option<Option<usize>> {
        let Some(open) = super::opening(&self.pairs, last) else {
            return Some(None);
        };
        let constructor = open.checked_sub(1)?;
        let choices = self.lookup(scope, self.word(constructor), Some(Role::Type), at);
        Some(unique(choices).and_then(|ty| ty.body))
    }

    fn valid_prefix(&self, prefix: Prefix, last: usize, scope: usize, at: usize) -> bool {
        let adjacent = prefix.member == last && prefix.scope == scope;
        adjacent && !self.declared[last + 1..=at].contains(&true)
    }

    fn resolve_chain(&self, last: usize, scope: usize, at: usize) -> Owner {
        let mut part = last;
        while let Some(previous) = self.qualifier(part) {
            part = previous;
        }
        let mut owner = self.advance_owner(Owner::Lexical, part, scope, at);
        while part != last && !matches!(owner, Owner::Missing) {
            part += 2 + usize::from(self.word(part + 1) != ".");
            owner = self.advance_owner(owner, part, scope, at);
        }
        owner
    }

    fn advance_owner(&self, owner: Owner, part: usize, scope: usize, at: usize) -> Owner {
        if matches!(owner, Owner::Missing) {
            return owner;
        }
        let token = &self.t[part];
        if matches!(token.text, "this" | "self" | "Self") {
            return self.explicit_receiver(token, scope);
        }
        self.named_receiver(token, owner.scope(), scope, at).map_or(Owner::Missing, Owner::Member)
    }

    fn explicit_receiver(&self, token: &super::Token, scope: usize) -> Owner {
        if !ident(token) {
            return Owner::Lexical;
        }
        self.scopes[scope].container.map_or(Owner::Lexical, Owner::Member)
    }

    fn named_receiver(
        &self,
        token: &super::Token,
        owner: Option<usize>,
        scope: usize,
        at: usize,
    ) -> Option<usize> {
        if !ident(token) {
            return None;
        }
        let name = token.text;
        let choice = match owner {
            Some(owner) => self.member_receiver(owner, name),
            None => self.lexical_receiver(scope, name, at),
        };
        let binding = match choice {
            BindingChoice::Unique(binding) => binding,
            BindingChoice::External(scope) => return scope,
            BindingChoice::Ambiguous => return None,
        };
        binding
            .body
            .or_else(|| self.type_owner(binding.ty?, scope, at))
    }

    fn type_owner(&self, ty: &str, scope: usize, at: usize) -> Option<usize> {
        self.lookup(scope, ty, Some(Role::Type), at)
            .next()
            .and_then(|binding| binding.body)
            .or_else(|| self.external_owner(scope, ty))
    }
    fn member_receiver(&self, owner: usize, name: &str) -> BindingChoice<'_, 'a> {
        match self.ids(owner, name) {
            [] => BindingChoice::External(self.external_at(owner, name)),
            &[id] => BindingChoice::Unique(&self.bindings[id]),
            _ => BindingChoice::Ambiguous,
        }
    }

    fn lexical_receiver(&self, scope: usize, name: &str, at: usize) -> BindingChoice<'_, 'a> {
        let mut choices = self.lookup(scope, name, None, at);
        match (choices.next(), choices.next()) {
            (None, _) => BindingChoice::External(self.external_owner(scope, name)),
            (Some(binding), None) => BindingChoice::Unique(binding),
            _ => BindingChoice::Ambiguous,
        }
    }
}
