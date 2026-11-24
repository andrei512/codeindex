//! The spelling of a member name stays in the source when an object-like
//! macro supplies its owner. Do not fabricate a source span for the expansion.
use super::Analyzer;
use std::borrow::Cow;

pub(super) fn owner(value: &str) -> Option<&str> {
    let name = value.trim().strip_suffix("::")?.trim_end();
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        || !chars.all(|c| c == '_' || c.is_alphanumeric())
    {
        return None;
    }
    Some(name)
}

impl Analyzer<'_, '_> {
    pub(super) fn macro_owner_scope(
        &mut self,
        scope: usize,
        name: String,
        at: usize,
    ) -> Option<usize> {
        // Like a written qualified definition, a macro can name a class whose
        // declaration lives in an included header. Other owners stay distinct.
        let choices = self.lookup(scope, &name, None, at);
        match choices.as_slice() {
            [binding] => return binding.body,
            [] => {}
            _ => return None,
        }
        if let Some(existing) = self.external_owner(scope, &name) {
            return Some(existing);
        }
        let body = self.scope(scope, None);
        self.scopes[body].container = Some(body);
        self.scopes[scope]
            .external_owners
            .insert(Cow::Owned(name), body);
        Some(body)
    }
}
