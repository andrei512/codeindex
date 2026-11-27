//! Grammar rules selected once by the declaration extractor. All implementations
//! write to the same declaration, scope and reference representation.
use super::{Binding, CallableKind, CallableName, ROOT, Range, Syntax};

pub(super) use super::super::languages::*;

pub(super) trait CallableGrammar {
    const EXPRESSION_BODY: bool = false;
    fn ends_expression(_: &str) -> bool {
        false
    }
    fn parameters(_: &mut Syntax<'_>, _: usize, _: Range<usize>) {}
}

pub(super) trait KeywordGrammar: CallableGrammar {
    fn name(s: &Syntax<'_>, start: usize, at: usize) -> Option<CallableName> {
        let scope = s.decl_scope(start);
        s.lexical_callable(start, at, scope)
    }
    fn after_name(_: &Syntax<'_>, next: usize) -> Option<usize> {
        Some(next)
    }
    fn method_binding(_: &Syntax<'_>, _: usize, _: usize) -> Binding {
        Binding::Direct
    }
}

pub(super) trait PhpGrammar: KeywordGrammar {
    fn type_kind(word: &str) -> Option<&'static str>;
}

pub(super) trait TypedGrammar: CallableGrammar {
    fn type_kind(_: &Syntax<'_>, _: usize) -> Option<&'static str> {
        None
    }
    const IMPLICIT_RETURN: bool = false;
    fn property(_: &str) -> bool {
        false
    }
    fn scope(s: &Syntax<'_>, name: usize) -> Option<usize>;
    fn valid_suffix(s: &Syntax<'_>, at: usize, class: usize) -> bool;
}

pub(super) fn file_or_type(s: &Syntax<'_>, name: usize) -> Option<usize> {
    let brace = s.parent[name];
    if brace == ROOT || s.namespaces.blocks.contains(&brace) {
        Some(ROOT)
    } else {
        s.types.get(&brace).copied()
    }
}

impl Syntax<'_> {
    pub(super) fn lexical_callable(
        &self,
        start: usize,
        at: usize,
        scope: usize,
    ) -> Option<CallableName> {
        let kind = if scope == ROOT && self.declaration_level(start) {
            CallableKind::Function
        } else {
            CallableKind::Method
        };
        self.id(at).then_some(CallableName {
            at,
            scope,
            kind,
            receiver: None,
        })
    }
}
