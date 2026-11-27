//! Written Rust syntax and declaration-backed navigation. Separate source scopes,
//! crate/module names, and nominal receiver types constrain binding. Proc-macro
//! expansion and compiler-derived trait obligations remain outside this profile.
pub(in crate::domain::code) mod analyzer;
mod binding;
mod bindings;
mod cargo;
mod config;
mod extract;
#[cfg(test)]
mod tests;
use binding::Resolver;
pub(super) use binding::Prepared;
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
type ModulePaths = Vec<(Vec<config::Predicate>, String)>;
enum Module {
    Inline(usize),
    File(Option<String>),
    Paths(ModulePaths),
    InlinePaths(usize, ModulePaths),
}
struct Lookup {
    scope: usize,
    ns: Namespace,
    path: Vec<String>,
}
impl Lookup {
    fn spelling<'a>(&'a self, written: &'a str) -> (&'a str, &'a [String]) {
        self.path
            .split_first()
            .map(|(head, tail)| (head.as_str(), tail))
            .unwrap_or_else(|| (name(written), &[]))
    }
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
struct Delimiters {
    mates: Vec<usize>,
    balanced: bool,
}

fn delimiters(t: &[Token]) -> Delimiters {
    let mut balanced = true;
    let mut pairs = vec![usize::MAX; t.len()];
    let mut stack = Vec::new();
    for (i, token) in t.iter().enumerate() {
        match token.text {
            "(" | "[" | "{" => stack.push((i, token.text)),
            ")" | "]" | "}" => {
                if let Some((j, open)) = stack.pop()
                    && matches!((open, token.text), ("(", ")") | ("[", "]") | ("{", "}"))
                {
                    pairs[i] = j;
                    pairs[j] = i;
                } else {
                    balanced = false;
                }
            }
            _ => {}
        }
    }
    Delimiters {
        mates: pairs,
        balanced: balanced && stack.is_empty(),
    }
}
// Both trait arguments and turbofish calls consume nested angle brackets.
pub(super) fn after_angles(t: &[Token], pairs: &[usize], open: usize) -> Option<usize> {
    let mut depth = 1;
    let close = top_level(t, pairs, open + 1..t.len(), |i, token| {
        match token {
            "<" => depth += 1,
            ">" if generic_close(t, i) => depth -= 1,
            ";" | "}" => return true,
            _ => {}
        }
        depth == 0
    });
    (depth == 0).then_some(close + 1)
}

// Expression paths spell generic arguments with an explicit :: before <.
fn expression_arguments(t: &[Token], pairs: &[usize], after: usize) -> usize {
    if (word(t, after), word(t, after + 1), word(t, after + 2)) == (":", ":", "<") {
        after_angles(t, pairs, after + 2).unwrap_or(after)
    } else {
        after
    }
}

fn preceding_segment(t: &[Token], i: usize) -> Option<usize> {
    let previous = i.checked_sub(3)?;
    (word(t, i - 2) == ":" && word(t, i - 1) == ":").then_some(previous)
}

fn path_segment(s: &str) -> bool {
    ident(s) || matches!(s, "self" | "Self" | "crate" | "super")
}

// The lexer emits the characters of -> separately. Its greater-than token
// never closes generic arguments, even inside a nested function-trait bound.
fn generic_close(t: &[Token], i: usize) -> bool {
    word(t, i) == ">" && (i == 0 || word(t, i - 1) != "-")
}
/// Only a top-level equals assigns the alias. Equalities inside generic
/// defaults and associated type constraints belong to the signature.
fn type_rhs(t: &[Token], pairs: &[usize], start: usize) -> Option<usize> {
    let mut angles = 0usize;
    let at = top_level(t, pairs, start..t.len(), |i, token| match token {
        "<" => {
            angles += 1;
            false
        }
        ">" if generic_close(t, i) => {
            angles = angles.saturating_sub(1);
            false
        }
        "=" => angles == 0,
        ";" | "}" => true,
        _ => false,
    });
    (word(t, at) == "=").then_some(at + 1)
}
fn top_level(
    tokens: &[Token],
    pairs: &[usize],
    range: std::ops::Range<usize>,
    mut stop: impl FnMut(usize, &str) -> bool,
) -> usize {
    let mut at = range.start;
    while at < range.end {
        let token = tokens[at].text;
        if stop(at, token) {
            return at;
        }
        at = next_top_level(pairs, at, token);
    }
    range.end
}

fn next_top_level(pairs: &[usize], at: usize, token: &str) -> usize {
    match token {
        "(" | "[" | "{" => after_group(pairs, at).unwrap_or(at + 1),
        _ => at + 1,
    }
}

// Index immediately after a matched delimiter. Callers choose their own
// recovery for a truncated or unmatched group.
fn after_group(pairs: &[usize], open: usize) -> Option<usize> {
    pairs.get(open)?.checked_add(1)
}

fn until_before(t: &[Token], pairs: &[usize], mut range: std::ops::Range<usize>, targets: &[&str]) -> usize {
    range.end = range.end.min(t.len());
    top_level(t, pairs, range, |i, token| targets.contains(&token) && !path_colon(t, i))
}

fn path_colon(tokens: &[Token], at: usize) -> bool {
    word(tokens, at) == ":"
        && (word(tokens, at + 1) == ":" || word(tokens, at.wrapping_sub(1)) == ":")
}

fn until(t: &[Token], pairs: &[usize], start: usize, targets: &[&str]) -> usize {
    if start > t.len() {
        return start;
    }
    until_before(t, pairs, start..t.len(), targets)
}

impl Evidence {
    fn record_use(
        &mut self,
        analysis: &mut Analysis,
        span: std::ops::Range<usize>,
        is_call: bool,
        lookup: Lookup,
    ) -> usize {
        let id = analysis.calls.len();
        analysis.calls.push(Call {
            span,
            is_call,
            owner: self.scopes[lookup.scope].owner,
        });
        self.uses.push(lookup);
        id
    }
}

enum ImportEvent<'a, 'prefix> {
    Occurrence(&'a Token<'a>),
    Binding {
        spelling: &'prefix str,
        offset: usize,
    },
    Glob(usize),
}

fn import_segment(spelling: &str) -> bool {
    ident(spelling) || matches!(spelling, "self" | "super" | "crate")
}

struct ImportTree {
    prefix: Vec<String>,
    levels: Vec<usize>,
}

impl ImportTree {
    fn scan<'a>(tokens: &'a [Token<'a>], mut emit: impl FnMut(ImportEvent<'a, '_>, &[String])) {
        let mut tree = Self {
            prefix: Vec::new(),
            levels: vec![0],
        };
        let mut i = 0;
        while let Some(token) = tokens.get(i) {
            match token.text {
                "{" | "}" | "," => tree.group_boundary(token.text),
                "*" if !tree.prefix.is_empty() => {
                    emit(ImportEvent::Glob(token.span.start), &tree.prefix)
                }
                spelling if import_segment(spelling) => {
                    i = tree.segment(tokens, i, &mut emit);
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn group_boundary(&mut self, spelling: &str) {
        match spelling {
            "{" => self.levels.push(self.prefix.len()),
            "}" => {
                self.levels.pop();
                self.reset_group();
            }
            _ => self.reset_group(),
        }
    }

    fn reset_group(&mut self) {
        self.prefix.truncate(*self.levels.last().unwrap_or(&0));
    }

    fn segment<'a>(
        &mut self,
        tokens: &'a [Token<'a>],
        i: usize,
        emit: &mut impl FnMut(ImportEvent<'a, '_>, &[String]),
    ) -> usize {
        let token = &tokens[i];
        let spelling = token.text;
        if spelling != "self" || self.prefix.is_empty() {
            self.prefix.push(name(spelling).to_owned());
        }
        if ident(spelling) || spelling == "self" {
            emit(ImportEvent::Occurrence(token), &self.prefix);
        }
        match word(tokens, i + 1) {
            ":" => i,
            "as" => self.alias(tokens, i + 2, emit),
            _ => {
                self.last_binding(token, emit);
                i
            }
        }
    }

    fn last_binding<'a>(
        &self,
        token: &Token<'a>,
        emit: &mut impl FnMut(ImportEvent<'a, '_>, &[String]),
    ) {
        let Some(spelling) = self.prefix.last().filter(|name| !name.is_empty()) else {
            return;
        };
        emit(ImportEvent::Binding { spelling, offset: token.span.start }, &self.prefix);
    }

    fn alias<'a>(
        &self,
        tokens: &'a [Token<'a>],
        at: usize,
        emit: &mut impl FnMut(ImportEvent<'a, '_>, &[String]),
    ) -> usize {
        let Some(alias) = tokens.get(at) else {
            return at;
        };
        if ident(alias.text) {
            emit(ImportEvent::Occurrence(alias), &self.prefix);
        }
        if !alias.text.is_empty() {
            emit(
                ImportEvent::Binding {
                    spelling: alias.text,
                    offset: alias.span.start,
                },
                &self.prefix,
            );
        }
        at
    }
}

fn imports(t: &[Token], scope: usize, evidence: &mut Evidence, analysis: &mut Analysis) {
    ImportTree::scan(t, |event, prefix| match event {
        ImportEvent::Occurrence(token) => {
            evidence.record_use(
                analysis,
                token.span.clone(),
                false,
                Lookup {
                    scope,
                    ns: Namespace::Either,
                    path: prefix.to_vec(),
                },
            );
        }
        ImportEvent::Binding { spelling, offset } => {
            evidence.scopes[scope]
                .imports
                .entry(name(spelling).into())
                .or_default()
                .push((offset, prefix.to_vec()));
        }
        ImportEvent::Glob(offset) => {
            evidence.scopes[scope].globs.push((offset, prefix.to_vec()));
        }
    });
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

crate::domain::code::cache::tagged!(Namespace { 0 => Value, 1 => Type, 2 => Either });

crate::domain::code::cache::tagged!(Module { 0 => Inline(scope), 1 => File(path), 2 => Paths(options), 3 => InlinePaths(scope,options) });
