//! Rules for reading function values and binding method receivers.
//! Only an explicit Java super call needs the ancestor signature cache.
use super::*;
use languages::{Dart, Go, Hack, Java, Kotlin, ObjectiveC, Php};

pub(super) enum MemberLookup {
    Explicit,
    Lexical,
    Shadowable,
}

pub(super) enum Receiver {
    This,
    DeclaredReceiver,
    Super,
}

pub(super) trait ReferenceGrammar {
    const MEMBERS: MemberLookup = MemberLookup::Explicit;
    const CLASS_METHOD_REFERENCE: bool = false;
    fn value_at(_: &Syntax<'_>, _: usize) -> bool {
        false
    }
    fn message_end(_: &Syntax, _: usize) -> Option<usize> {
        None
    }
    fn selector_literal(_: &Syntax, _: usize) -> bool {
        false
    }
    fn initializer(_: &Syntax<'_>, _: usize) -> Option<usize> {
        None
    }
    fn named_class(_: &Syntax<'_>, _: Option<usize>) -> bool {
        false
    }
    fn receiver(word: &str) -> Option<Receiver> {
        (word == "this").then_some(Receiver::This)
    }
}
fn bare_value(s: &Syntax<'_>, i: usize) -> bool {
    matches!(s.prev(i), "=" | "(" | "," | "return" | ".")
}

impl ReferenceGrammar for Go {
    fn value_at(s: &Syntax<'_>, i: usize) -> bool {
        bare_value(s, i)
    }
    fn receiver(_: &str) -> Option<Receiver> {
        Some(Receiver::DeclaredReceiver)
    }
}
impl ReferenceGrammar for Java {
    const MEMBERS: MemberLookup = MemberLookup::Lexical;
    const CLASS_METHOD_REFERENCE: bool = true;
    fn value_at(s: &Syntax<'_>, i: usize) -> bool {
        i >= 3 && s.prev(i) == ":" && s.w(i - 2) == ":" && s.id(i - 3)
    }
    fn receiver(word: &str) -> Option<Receiver> {
        match word {
            "this" => Some(Receiver::This),
            "super" => Some(Receiver::Super),
            _ => None,
        }
    }
}
impl ReferenceGrammar for Kotlin {
    const MEMBERS: MemberLookup = MemberLookup::Shadowable;
    fn value_at(s: &Syntax<'_>, i: usize) -> bool {
        i >= 2
            && s.prev(i) == ":"
            && s.w(i - 2) == ":"
            && matches!(s.prev(i - 2), "=" | "(" | "," | "return" | "{")
    }
    fn initializer(s: &Syntax<'_>, i: usize) -> Option<usize> {
        s.kotlin_initializer_type(i)
    }
}
impl ReferenceGrammar for Dart {
    const MEMBERS: MemberLookup = MemberLookup::Shadowable;
    fn value_at(s: &Syntax<'_>, i: usize) -> bool {
        bare_value(s, i)
    }
}
impl ReferenceGrammar for ObjectiveC {
    fn value_at(s: &Syntax<'_>, i: usize) -> bool {
        bare_value(s, i) || s.prev(i) == "&"
    }
    fn message_end(s: &Syntax, i: usize) -> Option<usize> {
        s.message_end(i)
    }
    fn selector_literal(s: &Syntax, i: usize) -> bool {
        s.prev(i) == "selector"
            || i >= 3 && s.prev(i) == "(" && s.w(i - 2) == "selector" && s.w(i - 3) == "@"
    }
}
impl ReferenceGrammar for Php {
    fn named_class(_: &Syntax<'_>, _: Option<usize>) -> bool {
        true
    }
}
impl ReferenceGrammar for Hack {
    fn named_class(s: &Syntax<'_>, caller: Option<usize>) -> bool {
        // A reified type parameter can hide a class name.
        !caller.is_some_and(|id| {
            let decl = &s.decls[id];
            s.w(decl.at + 1) == "<" || decl.scope != ROOT && s.w(decl.scope + 1) == "<"
        })
    }
}
