//! Written generic arguments used only when the path identifies a standard
//! collection or pointer. A same-named repository type keeps its own members.
use super::*;
#[derive(Clone)]
pub(super) struct Argument {
    pub outer: Vec<String>,
    pub inner: Vec<String>,
    pub index: usize,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Projection {
    Value,
    Element,
    Unwrap,
    Tuple(usize),
}
fn skip(t: &[Token], mut i: usize) -> usize {
    while matches!(word(t, i), "&" | "mut" | "dyn" | "ref" | "impl") {
        i += 1;
        if word(t, i) == "'" {
            i += 2;
        }
    }
    i
}
pub(super) fn arguments(t: &[Token], start: usize) -> Vec<Argument> {
    let mut i = skip(t, start);
    let mut result = Vec::new();
    let (outer, close) = match word(t, i) {
        "[" => (vec!["[]".into()], "]"),
        "(" => (vec!["()".into()], ")"),
        _ => {
            let path = members::path(t, i);
            if path.is_empty() {
                return result;
            }
            i += 1 + 3 * (path.len() - 1);
            if word(t, i) != "<" {
                return result;
            }
            (path, ">")
        }
    };
    i += 1;
    let mut index = 0;
    while i < t.len() && word(t, i) != close {
        let inner = members::path(t, skip(t, i));
        if !inner.is_empty() {
            result.push(Argument {
                outer: outer.clone(),
                inner,
                index,
            });
        }
        let mut depth = 0usize;
        while i < t.len() {
            let s = word(t, i);
            if depth == 0 && (s == close || matches!(s, "," | ";")) {
                break;
            }
            match s {
                "<" | "(" | "[" => depth += 1,
                ">" | ")" | "]" => {
                    if depth == 0 {
                        return result;
                    }
                    depth -= 1;
                }
                "{" | "}" => return result,
                _ => {}
            }
            i += 1;
        }
        if word(t, i) != "," {
            break;
        }
        i += 1;
        index += 1;
    }
    result
}
pub(super) fn permits(r: &Resolver, at: Address, a: &Argument, p: Projection) -> bool {
    let last = a.outer.last().map(String::as_str).unwrap_or("");
    if last == "[]" {
        return p == Projection::Element && a.index == 0;
    }
    if last == "()" {
        return p == Projection::Tuple(a.index);
    }
    if a.index != 0 {
        return false;
    }
    let accept = |full: &[String]| {
        let parts: Vec<_> = full.iter().map(String::as_str).collect();
        match (p, parts.as_slice()) {
            (
                Projection::Value,
                ["std" | "alloc", "sync", "Arc"]
                | ["std" | "alloc", "rc", "Rc"]
                | ["std" | "alloc", "boxed", "Box"],
            ) => true,
            (
                Projection::Element,
                ["std" | "alloc", "vec", "Vec"] | ["std" | "alloc", "collections", "VecDeque"],
            ) => true,
            (
                Projection::Unwrap,
                ["std" | "core", "option", "Option"] | ["std" | "core", "result", "Result"],
            ) => true,
            _ => false,
        }
    };
    if a.outer.len() != 1 {
        return accept(&a.outer);
    }
    let mut scope = Some(at.1);
    while let Some(id) = scope {
        let s = &r.evidence(at.0).scopes[id];
        if s.names.contains_key(last) {
            return false;
        }
        if let Some(alias) = r.alias(at.0, id, last) {
            return accept(alias);
        }
        if id == s.module {
            break;
        }
        scope = s.parent;
    }
    // Only names in the standard prelude may resolve without a use item.
    matches!(
        (p, last),
        (Projection::Value, "Box")
            | (Projection::Element, "Vec")
            | (Projection::Unwrap, "Result" | "Option")
    )
}

crate::domain::code::cache::record!(Argument { outer, inner, index });

impl crate::domain::code::cache::Wire for Projection{fn save(&self,w:&mut impl crate::domain::code::cache::Output){match self{Self::Value=>{ w.push(0); },Self::Element=>{ w.push(1); },Self::Unwrap=>{ w.push(2); },Self::Tuple(a)=>{ w.push(3); a.save(w);}}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Value),1=>Ok(Self::Element),2=>Ok(Self::Unwrap),3=>Ok(Self::Tuple(Wire::load(r)?)),_=>Err(crate::domain::code::cache::bad())}}}
