//! Lexical let/lambda scopes and statically selected literal attribute sets.
use super::{
    Span,
    config::{self, Kind, Token},
    span,
};
use crate::domain::code::Analysis;
use crate::domain::code::record_unique;
use std::collections::HashMap;

struct Scope<'a> {
    parent: Option<usize>,
    names: HashMap<&'a str, Option<usize>>,
}
/// Bindings are recorded in the set's own scope. Their values can see those
/// bindings only in a recursive set or a let expression.
#[derive(Clone, Copy)]
struct BindingScopes {
    table: usize,
    lexical: usize,
}
#[derive(Clone, Copy)]
struct ExpressionScope {
    lexical: usize,
    owner: Option<usize>,
    depth: usize,
}
impl ExpressionScope {
    fn nested(self, lexical: usize) -> Self {
        Self {
            lexical,
            depth: self.depth + 1,
            ..self
        }
    }
}
struct Use {
    scope: usize,
    owner: Option<usize>,
    parts: std::ops::RangeInclusive<usize>,
    application: bool,
}
enum Value {
    Other,
    Lambda,
    Set(usize),
}

struct Lambda {
    colon: usize,
    prefix: Option<usize>,
    formals: Option<std::ops::Range<usize>>,
    suffix: Option<usize>,
}
struct Parser<'a> {
    source: &'a str,
    t: Vec<Token<'a>>,
    mates: Vec<usize>,
    scopes: Vec<Scope<'a>>,
    sets: HashMap<usize, usize>,
    uses: Vec<Use>,
    facts: Analysis,
}

fn delimiter_pairs(t: &[Token]) -> Vec<usize> {
    let mut mates = vec![usize::MAX; t.len()];
    let mut stack = Vec::new();
    let tokens = t
        .iter()
        .enumerate()
        .filter(|(_, token)| matches!(token.kind, Kind::Punct | Kind::Name));
    for (i, token) in tokens {
        match group_close(token.text) {
            Some(close) => stack.push((i, close)),
            None => pair_closing(&mut stack, &mut mates, i, token.text),
        }
    }
    mates
}

fn pair_closing(stack: &mut Vec<(usize, &str)>, mates: &mut [usize], at: usize, word: &str) {
    if let Some((open, _)) = stack.pop_if(|(_, expected)| word == *expected) {
        mates[open] = at;
    }
}

fn group_close(word: &str) -> Option<&'static str> {
    const GROUPS: &[(&str, &str)] = &[
        ("(", ")"),
        ("[", "]"),
        ("{", "}"),
        ("${", "}"),
        ("let", "in"),
    ];
    GROUPS
        .iter()
        .find_map(|(open, close)| (*open == word).then_some(*close))
}

pub(in crate::domain::code) fn analyze(source: &str) -> Analysis {
    let t = config::tokens(source, true);
    let mates = delimiter_pairs(&t);
    let end = t.len();
    let mut p = Parser {
        source,
        t,
        mates,
        scopes: vec![],
        sets: HashMap::new(),
        uses: vec![],
        facts: Analysis::default(),
    };
    let root = p.scope(None);
    p.expr(
        0..end,
        ExpressionScope {
            lexical: root,
            owner: None,
            depth: 0,
        },
        false,
    );
    for u in std::mem::take(&mut p.uses) {
        p.resolve_path(u);
    }
    p.facts
}

impl<'a> Parser<'a> {
    fn resolve_path(&mut self, u: Use) {
        let mut id = std::iter::successors(Some(u.scope), |&s| self.scopes[s].parent)
            .find_map(|s| self.scopes[s].names.get(self.t[*u.parts.start()].text))
            .copied()
            .flatten();
        let last = *u.parts.end();
        for token in u.parts.step_by(2) {
            let Some(decl) = id else { break };
            let call = token == last
                && u.application
                && self.facts.declarations[decl].kind == "function_definition";
            self.facts
                .direct_edge(decl, self.location(token), u.owner, call);
            id = self.next_path_target(decl, token, last);
        }
    }

    fn next_path_target(&self, declaration: usize, token: usize, last: usize) -> Option<usize> {
        if token == last {
            return None;
        }
        self.attribute_of(declaration, self.t[token + 2].text)
    }

    fn attribute_of(&self, declaration: usize, name: &str) -> Option<usize> {
        let scope = *self.sets.get(&declaration)?;
        self.scopes[scope].names.get(name).copied().flatten()
    }

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
    fn close_before(&self, i: usize, end: usize) -> Option<usize> {
        self.mates.get(i).copied().filter(|&close| close < end)
    }
    fn next(&self, i: usize, end: usize) -> usize {
        self.close_before(i, end).unwrap_or(i) + 1
    }
    fn stop(&self, mut i: usize, end: usize, words: &[&str]) -> usize {
        while i < end && !words.iter().any(|w| self.is(i, w)) {
            i = self.next(i, end)
        }
        i
    }
    fn decl(&mut self, scope: usize, i: usize, kind: &'static str) -> usize {
        let id = self.facts.declare_at(self.location(i), kind);
        record_unique(&mut self.scopes[scope].names, self.t[i].text, id);
        id
    }
    fn lambda(&self, start: usize, end: usize) -> Option<Lambda> {
        let following = self.name(start).then(|| self.t.get(start + 1)).flatten();
        let prefix = match following.map(|token| token.text) {
            Some(":") if start + 1 < end => {
                return Some(Lambda {
                    colon: start + 1,
                    prefix: Some(start),
                    formals: None,
                    suffix: None,
                });
            }
            Some("@") => Some(start),
            _ => None,
        };
        self.formal_lambda(start, end, prefix)
    }

    fn formal_lambda(&self, start: usize, end: usize, prefix: Option<usize>) -> Option<Lambda> {
        let open = if prefix.is_some() { start + 2 } else { start };
        if !self.is(open, "{") {
            return None;
        }
        let close = self.close_before(open, end)?;
        let suffix = (self.is(close + 1, "@") && self.name(close + 2)).then_some(close + 2);
        let colon = suffix.map_or(close + 1, |i| i + 1);
        (colon < end && self.is(colon, ":")).then_some(Lambda {
            colon,
            prefix,
            formals: Some(open + 1..close),
            suffix,
        })
    }
    fn params(&mut self, lambda: &Lambda, scope: usize) {
        if let Some(i) = lambda.prefix {
            self.decl(scope, i, "parameter");
        }
        if let Some(formals) = &lambda.formals {
            let mut i = formals.start;
            while i < formals.end {
                if self.name(i) {
                    self.decl(scope, i, "parameter");
                }
                i = self.stop(i, formals.end, &[","]) + 1;
            }
        }
        if let Some(i) = lambda.suffix {
            self.decl(scope, i, "parameter");
        }
    }
    fn lambda_body(&mut self, lambda: Lambda, end: usize, scope: ExpressionScope) {
        let child = self.scope(Some(scope.lexical));
        self.params(&lambda, child);
        self.expr(lambda.colon + 1..end, scope.nested(child), false);
    }

    fn lambda_value(
        &mut self,
        lambda: Lambda,
        end: usize,
        scope: ExpressionScope,
        begins_expression: bool,
    ) -> Value {
        if scope.depth < 96 {
            self.lambda_body(lambda, end, scope);
        }
        if begins_expression {
            Value::Lambda
        } else {
            Value::Other
        }
    }

    fn bindings(&mut self, mut i: usize, end: usize, scopes: BindingScopes, depth: usize) {
        if depth >= 96 {
            return;
        }
        while i < end {
            let semi = self.stop(i, end, &[";"]);
            if semi == end {
                return;
            }
            self.binding(i, semi, scopes, depth);
            i = semi + 1;
        }
    }
    fn binding(&mut self, start: usize, end: usize, scopes: BindingScopes, depth: usize) {
        if self.is(start, "inherit") {
            self.inherit(start + 1, end, scopes.table);
        } else {
            self.attribute(start, end, scopes, depth);
        }
    }

    fn inherit(&mut self, mut name: usize, semi: usize, table: usize) {
        if name < semi && self.is(name, "(") {
            name = self.next(name, semi);
        }
        for j in name..semi {
            if self.name(j) {
                self.scopes[table].names.insert(self.t[j].text, None);
            }
        }
    }
    fn path_end(
        &self,
        mut last: usize,
        end: usize,
        component: impl Fn(&Self, usize) -> bool,
    ) -> usize {
        while self.path_dot(last + 1, end) && component(self, last + 2) {
            last += 2;
        }
        last
    }
    fn path_dot(&self, at: usize, end: usize) -> bool {
        at + 1 < end && self.is(at, ".")
    }

    fn attribute_name(&self, i: usize, semi: usize) -> Option<(usize, usize)> {
        let key = |t: Token| {
            t.kind == Kind::Name
                || t.kind == Kind::String && !t.text.is_empty() && !t.text.contains(['$', '\\'])
        };
        if !key(self.t[i]) {
            return None;
        }
        let last = self.path_end(i, semi, |parser, token| key(parser.t[token]));
        if last + 1 >= semi || !self.is(last + 1, "=") {
            return None;
        }
        Some((last, last + 2))
    }

    fn attribute(&mut self, i: usize, semi: usize, scopes: BindingScopes, depth: usize) {
        let Some((last, rhs)) = self.attribute_name(i, semi) else {
            return;
        };
        let id = if i == last {
            self.decl(scopes.table, i, "attribute")
        } else {
            self.facts.declare_at(self.location(last), "attribute")
        };
        // The expression walker already recognizes functions and opens set scopes.
        // Commit that result before resolving the accumulated uses.
        let value_scope = ExpressionScope {
            lexical: scopes.lexical,
            owner: Some(id),
            depth: depth + 1,
        };
        match self.expr(rhs..semi, value_scope, false) {
            Value::Lambda => self.facts.declarations[id].kind = "function_definition",
            Value::Set(scope) => {
                self.sets.insert(id, scope);
            }
            Value::Other => {}
        }
    }
    fn attribute_set(
        &mut self,
        pair: std::ops::Range<usize>,
        parent: usize,
        recursive: bool,
        depth: usize,
    ) -> usize {
        let std::ops::Range {
            start: open,
            end: close,
        } = pair;
        let scope = self.scope(Some(parent));
        let lexical = if recursive { scope } else { parent };
        self.bindings(
            open + 1,
            close,
            BindingScopes {
                table: scope,
                lexical,
            },
            depth,
        );
        scope
    }

    fn argument_start(&self, next: usize) -> bool {
        let token = self.t[next];
        token.text.starts_with(|c: char| c.is_ascii_digit())
            || match token.kind {
                Kind::Name => {
                    self.name(next) && !matches!(token.text, "then" | "else" | "in" | "or")
                }
                Kind::String => true,
                Kind::Punct => matches!(token.text, "(" | "{" | "["),
                _ => false,
            }
    }

    fn reference_head(&self, i: usize, start: usize) -> bool {
        self.name(i)
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
    }

    fn record_use(
        &mut self,
        i: usize,
        bounds: std::ops::Range<usize>,
        scope: ExpressionScope,
        list: bool,
    ) -> usize {
        let first = i;
        let std::ops::Range { start, end } = bounds;
        let i = self.path_end(first, end, Self::name);
        let next = i + 1;
        let head = first == start
            || matches!(
                self.t[first - 1].text,
                "then" | "else" | "in" | "=" | "+" | "++" | "//" | "&&" | "||" | "==" | "!=" | ";"
            );
        self.uses.push(Use {
            scope: scope.lexical,
            owner: scope.owner,
            parts: first..=i,
            application: !list && head && next < end && self.argument_start(next),
        });
        i
    }
    /// Consume one paired group; only a set spanning the whole value can be selected by its binding.
    fn group(
        &mut self,
        pair: std::ops::Range<usize>,
        outer: std::ops::Range<usize>,
        scope: ExpressionScope,
    ) -> (usize, Option<usize>) {
        let std::ops::Range {
            start: i,
            end: close,
        } = pair;
        let std::ops::Range { start, end } = outer;
        match self.t[i].text {
            "let" => {
                let nested = self.scope(Some(scope.lexical));
                self.bindings(
                    i + 1,
                    close,
                    BindingScopes {
                        table: nested,
                        lexical: nested,
                    },
                    scope.depth,
                );
                let stop = self.stop(close + 1, end, &["then", "else"]);
                self.expr(close + 1..stop, scope.nested(nested), false);
                return (stop, None);
            }
            "{" => {
                let recursive = i > start && self.is(i - 1, "rec");
                let nested = self.attribute_set(i..close, scope.lexical, recursive, scope.depth);
                let complete = i == start + usize::from(recursive) && close + 1 == end;
                return (close + 1, complete.then_some(nested));
            }
            "(" | "[" => {
                self.expr(i + 1..close, scope.nested(scope.lexical), self.is(i, "["));
            }
            "${" => {}
            _ => unreachable!("delimiter_pairs only records group openers"),
        }
        (close + 1, None)
    }
    fn expression_atom(
        &mut self,
        token: usize,
        bounds: std::ops::Range<usize>,
        scope: ExpressionScope,
        list: bool,
    ) -> usize {
        if self.reference_head(token, bounds.start) {
            self.record_use(token, bounds, scope, list)
        } else {
            token
        }
    }

    fn expr(
        &mut self,
        bounds: std::ops::Range<usize>,
        scope: ExpressionScope,
        list: bool,
    ) -> Value {
        let mut i = bounds.start;
        while i < bounds.end {
            match self.expression_step(i, &bounds, scope, list) {
                ExpressionStep::Next(at) => i = at,
                ExpressionStep::Done(value) => return value,
            }
        }
        Value::Other
    }

    fn expression_step(
        &mut self,
        i: usize,
        bounds: &std::ops::Range<usize>,
        scope: ExpressionScope,
        list: bool,
    ) -> ExpressionStep {
        if let Some(lambda) = self.lambda(i, bounds.end) {
            return ExpressionStep::Done(self.lambda_value(
                lambda,
                bounds.end,
                scope,
                i == bounds.start,
            ));
        }
        if scope.depth >= 96 {
            return ExpressionStep::Done(Value::Other);
        }
        if let Some(close) = self.close_before(i, bounds.end) {
            let (next, set) = self.group(i..close, bounds.clone(), scope);
            return set.map_or(ExpressionStep::Next(next), |set| {
                ExpressionStep::Done(Value::Set(set))
            });
        }
        ExpressionStep::Next(self.expression_atom(i, bounds.clone(), scope, list) + 1)
    }
}

enum ExpressionStep {
    Next(usize),
    Done(Value),
}
