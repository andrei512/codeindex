//! Written Rust syntax and declaration-backed navigation. Separate source scopes,
//! crate/module names, and nominal receiver types constrain binding. Proc-macro
//! expansion and compiler-derived trait obligations remain outside this profile.
mod binding;
mod bindings;
mod cargo;
mod config;
mod extract;
#[cfg(test)]
mod tests;
use binding::Resolver;
pub(super) use binding::resolve;
pub(super) use extract::{analyze, analyze_paired};
mod generics;
mod identity;
mod members;
mod patterns;
mod type_hints;
#[cfg(test)]
use super::Names;
use super::{Analysis, Call, Declaration, Kind, ForwardNames, Site, Token, Unit};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use type_hints::Projection;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Namespace {
    Value,
    Type,
    Either,
}
#[derive(Clone, Copy)]
struct Binding {
    decl: usize,
    visibility: usize,
    local: bool,
    ns: Namespace,
}
#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    owner: Option<usize>,
    associated: bool,
    literal: Vec<String>,
    module: usize,
    names: HashMap<String, bindings::Bindings>,
    imports: HashMap<String, Vec<(usize, Vec<String>)>>,
    globs: Vec<(usize, Vec<String>)>,
    children: HashMap<String, Vec<(usize, Module)>>,
}
impl Scope {
    fn module(&mut self, key: &str, child: Module, at: usize) {
        self.children
            .entry(name(key).into())
            .or_default()
            .push((at, child));
    }
}
enum Module {
    Inline(usize),
    File(Option<String>),
}
struct Lookup {
    scope: usize,
    ns: Namespace,
    path: Vec<String>,
}
#[derive(Default)]
pub(super) struct Evidence {
    conditional_crate: bool,
    conditions: Vec<config::Condition>,
    members: members::Members,
    direct: HashMap<usize, usize>,
    scopes: Vec<Scope>,
    uses: Vec<Lookup>,
}

fn word<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |t| t.text)
}
fn name(s: &str) -> &str {
    s.strip_prefix("r#").unwrap_or(s)
}
fn ident(s: &str) -> bool {
    if s.starts_with("r#") {
        return true;
    }
    s.starts_with(|c: char| c == '_' || c.is_alphabetic())
        && !matches!(
            s,
            "_" | "as"
                | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "yield"
                | "try"
                | "macro_rules"
        )
}
fn declaration(
    a: &mut Analysis,
    e: &mut Evidence,
    t: &[Token],
    i: usize,
    scope: usize,
    kind: &'static str,
    visibility: usize,
    ns: Namespace,
) -> usize {
    let id = a.declarations.len();
    a.declarations.push(Declaration {
        span: t[i].span.clone(),
        kind,
    });
    e.scopes[scope]
        .names
        .entry(name(t[i].text).to_owned())
        .or_default()
        .push(Binding {
            decl: id,
            visibility,
            local: matches!(kind, "parameter" | "local_binding"),
            ns,
        });
    id
}

pub(super) fn matches(t: &[Token]) -> Vec<usize> {
    let mut pairs = vec![usize::MAX; t.len()];
    let mut stack = Vec::new();
    for (i, token) in t.iter().enumerate() {
        match token.text {
            "(" | "[" | "{" => stack.push((i, token.text)),
            ")" | "]" | "}" => {
                if let Some((j, open)) = stack.pop() {
                    if matches!((open, token.text), ("(", ")") | ("[", "]") | ("{", "}")) {
                        pairs[i] = j;
                        pairs[j] = i;
                    }
                }
            }
            _ => {}
        }
    }
    pairs
}
fn has_type_assignment(t: &[Token], mut i: usize) -> bool {
    let mut depth = 0usize;
    while i < t.len() {
        match word(t, i) {
            "<" | "(" | "[" => depth += 1,
            ">" | ")" | "]" => depth = depth.saturating_sub(1),
            "=" if depth == 0 => return true,
            ";" | "}" => return false,
            _ => {}
        }
        i += 1;
    }
    false
}
fn until(t: &[Token], pairs: &[usize], mut i: usize, targets: &[&str]) -> usize {
    while i < t.len() {
        if targets.contains(&t[i].text)
            && !(word(t, i) == ":" && (word(t, i + 1) == ":" || i > 0 && word(t, i - 1) == ":"))
        {
            break;
        }
        if matches!(t[i].text, "(" | "[" | "{") && pairs[i] != usize::MAX {
            i = pairs[i] + 1;
        } else {
            i += 1;
        }
    }
    i
}

// Each group carries the prefix accumulated before its open brace.
fn imports(t: &[Token], scope_id: usize, e: &mut Evidence, a: &mut Analysis) {
    let mut prefix = Vec::<String>::new();
    let mut levels = vec![0];
    let mut i = 0;
    while i < t.len() {
        match t[i].text {
            "{" => levels.push(prefix.len()),
            "}" => {
                levels.pop();
                prefix.truncate(*levels.last().unwrap_or(&0));
            }
            "," => prefix.truncate(*levels.last().unwrap_or(&0)),
            ":" => {}
            "*" if !prefix.is_empty() => {
                e.scopes[scope_id]
                    .globs
                    .push((t[i].span.start, prefix.clone()));
            }
            s if ident(s) || matches!(s, "self" | "super" | "crate") => {
                if s != "self" || prefix.is_empty() {
                    prefix.push(name(s).to_owned());
                }
                if ident(s) || s == "self" {
                    let path = prefix.clone();
                    a.calls.push(Call {
                        span: t[i].span.clone(),
                        owner: e.scopes[scope_id].owner,
                        is_call: false,
                    });
                    e.uses.push(Lookup {
                        scope: scope_id,
                        ns: Namespace::Either,
                        path,
                    });
                }
                if t.get(i + 1).is_none_or(|n| n.text != ":") {
                    let local = if word(t, i + 1) == "as" {
                        i += 2;
                        if i < t.len() && ident(t[i].text) {
                            a.calls.push(Call {
                                span: t[i].span.clone(),
                                owner: e.scopes[scope_id].owner,
                                is_call: false,
                            });
                            e.uses.push(Lookup {
                                scope: scope_id,
                                ns: Namespace::Either,
                                path: prefix.clone(),
                            });
                        }
                        word(t, i)
                    } else {
                        prefix.last().map_or("", String::as_str)
                    };
                    if !local.is_empty() {
                        e.scopes[scope_id]
                            .imports
                            .entry(name(local).into())
                            .or_default()
                            .push((t[i].span.start, prefix.clone()));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
}
fn bind_pattern(
    a: &mut Analysis,
    e: &mut Evidence,
    t: &[Token],
    excluded: &mut [bool],
    scope: usize,
    range: std::ops::Range<usize>,
    kind: &'static str,
    visibility: usize,
) {
    let stop = range.end;
    let alternatives = t
        .get(range.clone())
        .is_some_and(|tokens| tokens.iter().any(|token| token.text == "|"));
    let mut first = HashMap::new();
    for i in range {
        let s = word(t, i);
        let previous = if i > 0 { word(t, i - 1) } else { "" };
        if !(ident(s) || s == "self")
            || !name(s).starts_with(|c: char| c == '_' || c.is_lowercase())
            || matches!(previous, "'" | ".")
            || i + 1 < stop && word(t, i + 1) == ":"
            || i > 1 && previous == ":" && word(t, i - 2) == ":"
        {
            continue;
        }
        if alternatives && let Some(&id) = first.get(s) {
            let local = a.calls.len();
            a.calls.push(Call {
                span: t[i].span.clone(),
                owner: e.scopes[scope].owner,
                is_call: false,
            });
            e.uses.push(Lookup {
                scope,
                ns: Namespace::Value,
                path: Vec::new(),
            });
            e.direct.insert(local, id);
        } else {
            let id = declaration(a, e, t, i, scope, kind, visibility, Namespace::Value);
            if alternatives {
                first.insert(s, id);
            }
        }
        excluded[i] = true;
    }
}

fn new_scope(e: &mut Evidence, parent: usize) -> usize {
    let child = Scope {
        parent: Some(parent),
        module: e.scopes[parent].module,
        owner: e.scopes[parent].owner,
        ..Default::default()
    };
    let id = e.scopes.len();
    e.scopes.push(child);
    id
}

type Address = (usize, usize); // file, lexical scope

crate::domain::code::cache::record!(Evidence { conditional_crate, conditions, members, direct, scopes, uses });

crate::domain::code::cache::record!(Scope { parent, owner, associated, literal, module, names, imports, globs, children });

crate::domain::code::cache::record!(Binding { decl, visibility, local, ns });

crate::domain::code::cache::record!(Lookup { scope, ns, path });

impl crate::domain::code::cache::Wire for Namespace{fn save(&self,w:&mut impl crate::domain::code::cache::Output){match self{Self::Value=>{ w.push(0); },Self::Type=>{ w.push(1); },Self::Either=>{ w.push(2); }}}fn load(r:&mut &[u8])->std::io::Result<Self>{match usize::load(r)?{0=>Ok(Self::Value),1=>Ok(Self::Type),2=>Ok(Self::Either),_=>Err(crate::domain::code::cache::bad())}}}

impl crate::domain::code::cache::Wire for Module{fn save(&self,w:&mut impl crate::domain::code::cache::Output){match self{Self::Inline(a)=>{ w.push(0); a.save(w);},Self::File(a)=>{ w.push(1); a.save(w);}}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Inline(Wire::load(r)?)),1=>Ok(Self::File(Wire::load(r)?)),_=>Err(crate::domain::code::cache::bad())}}}
