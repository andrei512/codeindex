//! Lexical let/lambda scopes and statically selected literal attribute sets.
use super::{
    Facts, Span,
    config::{self, Kind, Token},
    span,
};
use crate::domain::code::Analysis;
use std::collections::HashMap;

struct Scope<'a> {
    parent: Option<usize>,
    names: HashMap<&'a str, Option<usize>>,
}
struct Use {
    scope: usize,
    owner: Option<usize>,
    parts: Vec<usize>,
    application: bool,
}
struct Parser<'a> {
    source: &'a str,
    t: Vec<Token<'a>>,
    mates: Vec<usize>,
    scopes: Vec<Scope<'a>>,
    sets: HashMap<usize, usize>,
    uses: Vec<Use>,
    facts: Facts,
}

pub(super) fn analyze(source: &str) -> Analysis {
    let t = config::tokens(source, true);
    let mut mates = vec![usize::MAX; t.len()];
    let mut stack = Vec::new();
    for (i, tok) in t.iter().enumerate() {
        if !matches!(tok.kind, Kind::Punct | Kind::Name) {
            continue;
        }
        match tok.text {
            "(" => stack.push((i, ")")),
            "[" => stack.push((i, "]")),
            "{" | "${" => stack.push((i, "}")),
            "let" => stack.push((i, "in")),
            ")" | "]" | "}" | "in" if stack.last().is_some_and(|(_, s)| *s == tok.text) => {
                let (open, _) = stack.pop().unwrap();
                mates[open] = i;
            }
            _ => (),
        }
    }
    let end = t.len();
    let mut p = Parser {
        source,
        t,
        mates,
        scopes: vec![],
        sets: HashMap::new(),
        uses: vec![],
        facts: Facts::default(),
    };
    let root = p.scope(None);
    p.expr(0, end, root, None, false, 0);
    for u in std::mem::take(&mut p.uses) {
        let mut scope = Some(u.scope);
        let name = p.t[u.parts[0]].text;
        let mut id = None;
        while let Some(s) = scope {
            if let Some(binding) = p.scopes[s].names.get(name) {
                id = *binding;
                break;
            }
            scope = p.scopes[s].parent;
        }
        for (at, &token) in u.parts.iter().enumerate() {
            let Some(decl) = id else { break };
            let call = at + 1 == u.parts.len()
                && u.application
                && p.facts.analysis.declarations[decl].kind == "function_definition";
            p.facts.edge(decl, p.location(token), u.owner, call);
            id = u.parts.get(at + 1).and_then(|&next| {
                let s = *p.sets.get(&decl)?;
                *p.scopes[s].names.get(p.t[next].text)?
            });
        }
    }
    p.facts.finish()
}

impl<'a> Parser<'a> {
    fn scope(&mut self, parent: Option<usize>) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent,
            names: HashMap::new(),
        });
        id
    }
    fn location(&self, i: usize) -> Span {
        span(self.source, self.t[i].text)
    }
    fn name(&self, i: usize) -> bool {
        self.t.get(i).is_some_and(|t| {
            t.kind == Kind::Name
                && t.text
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
        })
    }
    fn is(&self, i: usize, s: &str) -> bool {
        self.t
            .get(i)
            .is_some_and(|t| matches!(t.kind, Kind::Name | Kind::Punct) && t.text == s)
    }
    fn next(&self, i: usize, end: usize) -> usize {
        if self.mates[i] < end {
            self.mates[i] + 1
        } else {
            i + 1
        }
    }
    fn stop(&self, mut i: usize, end: usize, words: &[&str]) -> usize {
        while i < end && !words.iter().any(|w| self.is(i, w)) {
            i = self.next(i, end)
        }
        i
    }
    fn decl(&mut self, scope: usize, i: usize, kind: &'static str) -> usize {
        let id = self.facts.analysis.declarations.len();
        self.facts
            .analysis
            .declarations
            .push(crate::domain::code::Declaration {
                span: self.location(i),
                kind,
            });
        self.scopes[scope]
            .names
            .entry(self.t[i].text)
            .and_modify(|v| *v = None)
            .or_insert(Some(id));
        id
    }
    fn lambda_colon(&self, i: usize, end: usize) -> Option<usize> {
        if self.name(i) && i + 1 < end && self.is(i + 1, ":") {
            Some(i + 1)
        } else {
            let start = if self.name(i) && self.is(i + 1, "@") {
                i + 2
            } else {
                i
            };
            if !self.is(start, "{") || self.mates[start] >= end {
                return None;
            }
            let mut j = self.mates[start] + 1;
            if self.is(j, "@") && self.name(j + 1) {
                j += 2
            }
            (j < end && self.is(j, ":")).then_some(j)
        }
    }
    fn params(&mut self, start: usize, colon: usize, scope: usize) {
        if self.name(start) {
            self.decl(scope, start, "parameter");
        }
        let brace = if self.is(start, "{") {
            start
        } else {
            start + 2
        };
        if brace >= colon || !self.is(brace, "{") {
            return;
        }
        let close = self.mates[brace];
        let mut j = brace + 1;
        while j < close {
            if self.name(j) {
                self.decl(scope, j, "parameter");
            }
            j = self.stop(j, close, &[","]) + 1;
        }
        if close + 2 < colon && self.is(close + 1, "@") && self.name(close + 2) {
            self.decl(scope, close + 2, "parameter");
        }
    }
    fn bindings(&mut self, mut i: usize, end: usize, table: usize, lexical: usize, depth: usize) {
        if depth >= 96 {
            return;
        }
        while i < end {
            let semi = self.stop(i, end, &[";"]);
            if semi == end {
                break;
            }
            let key = |t: Token| {
                t.kind == Kind::Name
                    || (t.kind == Kind::String
                        && !t.text.is_empty()
                        && !t.text.contains(['$', '\\']))
            };
            if self.is(i, "inherit") {
                let mut name = i + 1;
                if name < semi && self.is(name, "(") {
                    name = self.next(name, semi);
                }
                for j in name..semi {
                    if self.name(j) {
                        self.scopes[table].names.insert(self.t[j].text, None);
                    }
                }
            } else if key(self.t[i]) {
                let mut last = i;
                while last + 2 < semi && self.is(last + 1, ".") && key(self.t[last + 2]) {
                    last += 2;
                }
                if last + 1 < semi && self.is(last + 1, "=") {
                    let rhs = last + 2;
                    let kind = if rhs < semi && self.lambda_colon(rhs, semi).is_some() {
                        "function_definition"
                    } else {
                        "attribute"
                    };
                    let id = if i == last {
                        self.decl(table, i, kind)
                    } else {
                        let id = self.facts.analysis.declarations.len();
                        self.facts
                            .analysis
                            .declarations
                            .push(crate::domain::code::Declaration {
                                span: self.location(last),
                                kind,
                            });
                        id
                    };
                    let recursive = self.is(rhs, "rec");
                    let open = rhs + usize::from(recursive);
                    if open < semi && self.is(open, "{") && self.mates[open] == semi - 1 {
                        let nested = self.scope(Some(lexical));
                        self.sets.insert(id, nested);
                        self.bindings(
                            open + 1,
                            semi - 1,
                            nested,
                            if recursive { nested } else { lexical },
                            depth + 1,
                        );
                    } else {
                        self.expr(rhs, semi, lexical, Some(id), false, depth + 1);
                    }
                }
            }
            i = semi + 1;
        }
    }
    fn expr(
        &mut self,
        start: usize,
        end: usize,
        scope: usize,
        owner: Option<usize>,
        list: bool,
        depth: usize,
    ) {
        if depth >= 96 {
            return;
        }
        let mut i = start;
        while i < end {
            if let Some(colon) = self.lambda_colon(i, end) {
                let nested = self.scope(Some(scope));
                self.params(i, colon, nested);
                self.expr(colon + 1, end, nested, owner, false, depth + 1);
                break;
            }
            if self.is(i, "let") && self.mates[i] < end {
                let close = self.mates[i];
                let nested = self.scope(Some(scope));
                self.bindings(i + 1, close, nested, nested, depth);
                let stop = self.stop(close + 1, end, &["then", "else"]);
                self.expr(close + 1, stop, nested, owner, false, depth + 1);
                i = stop;
                continue;
            }
            if self.is(i, "{") && self.mates[i] < end {
                let table = self.scope(Some(scope));
                let recursive = i > start && self.is(i - 1, "rec");
                self.bindings(
                    i + 1,
                    self.mates[i],
                    table,
                    if recursive { table } else { scope },
                    depth,
                );
                i = self.mates[i] + 1;
                continue;
            }
            if (self.is(i, "(") || self.is(i, "[")) && self.mates[i] < end {
                self.expr(
                    i + 1,
                    self.mates[i],
                    scope,
                    owner,
                    self.is(i, "["),
                    depth + 1,
                );
                i = self.mates[i] + 1;
                continue;
            }
            if self.is(i, "${") && self.mates[i] < end {
                i = self.mates[i] + 1;
                continue;
            }
            if self.name(i)
                && !matches!(
                    self.t[i].text,
                    "let"
                        | "in"
                        | "with"
                        | "if"
                        | "then"
                        | "else"
                        | "assert"
                        | "rec"
                        | "inherit"
                        | "or"
                        | "true"
                        | "false"
                        | "null"
                )
                && !(i > start && (self.is(i - 1, ".") || self.is(i - 1, "?")))
            {
                let first = i;
                let mut parts = vec![i];
                while i + 2 < end && self.is(i + 1, ".") && self.name(i + 2) {
                    i += 2;
                    parts.push(i);
                }
                let next = i + 1;
                let head = first == start
                    || matches!(
                        self.t[first - 1].text,
                        "then"
                            | "else"
                            | "in"
                            | "="
                            | "+"
                            | "++"
                            | "//"
                            | "&&"
                            | "||"
                            | "=="
                            | "!="
                            | ";"
                    );
                let arg = next < end
                    && (self.name(next)
                        && !matches!(self.t[next].text, "then" | "else" | "in" | "or")
                        || self.t[next].text.starts_with(|c: char| c.is_ascii_digit())
                        || self.t[next].kind == Kind::String
                        || self.is(next, "(")
                        || self.is(next, "{")
                        || self.is(next, "["));
                self.uses.push(Use {
                    scope,
                    owner,
                    parts,
                    application: head && arg && !list,
                });
            }
            i += 1;
        }
    }
}
