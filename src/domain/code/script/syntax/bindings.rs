//! Bindings and type references written in one lexical scope.
use super::*;
use crate::domain::code::script::keyword;
use std::ops::Range;

#[derive(Clone, Copy)]
enum TypeLiteral {
    Parameter,
    Return,
}

impl TypeLiteral {
    fn fields<'a>(
        tokens: &'a [Token],
        pairs: &'a [usize],
        open: usize,
        limit: usize,
    ) -> impl Iterator<Item = usize> + 'a {
        let first = open + 1;
        let mut at = first;
        let limit = pairs[open].min(limit);
        std::iter::from_fn(move || {
            while at < limit {
                let token = at;
                at = paired_end(tokens, pairs, at).unwrap_or(at) + 1;
                if Self::field(&tokens[..limit], token, first) {
                    return Some(token);
                }
            }
            None
        })
    }

    fn field(t: &[Token], at: usize, first: usize) -> bool {
        if !word(&t[at]) {
            return false;
        }
        let prev = &t[at - 1];
        let same_line_field =
            prev.line == t[at].line && !matches!(prev.text, "," | ";" | "readonly");
        if at != first && same_line_field {
            return false;
        }
        let optional = usize::from(tok(t, at + 1) == "?");
        let suffix = at + 1 + optional;
        match tok(t, suffix) {
            ":" | "(" => true,
            "<" => super::super::angle_end(t, suffix).is_some_and(|close| tok(t, close + 1) == "("),
            _ => false,
        }
    }
}

fn paired_end(tokens: &[Token], pairs: &[usize], at: usize) -> Option<usize> {
    matches!(tokens[at].text, "(" | "[" | "{").then(|| pairs[at])
}

/// Compound parameter syntax hides separators until its matching delimiter.
fn group_end(angles: &mut AngleEnds, pairs: &[usize], at: usize) -> Option<usize> {
    if angles.tokens[at].text == "<" {
        angles.following(at)
    } else {
        paired_end(angles.tokens, pairs, at)
    }
}

/// Yields the token that can introduce a binding in each parameter. Commas
/// inside annotations, calls, defaults and destructuring belong to that parameter.
fn parameter_entries<'a>(
    angles: &'a mut AngleEnds,
    pairs: &'a [usize],
    params: Range<usize>,
) -> impl Iterator<Item = Entry> + 'a {
    let t = angles.tokens;
    entries(
        t,
        params,
        |word| {
            matches!(
                word,
                "*" | "/" | "." | "readonly" | "public" | "private" | "protected"
            )
        },
        move |p, head| {
            if p == head && t[p].text == "<" {
                None
            } else {
                group_end(angles, pairs, p)
            }
        },
    )
}

struct Entry {
    initializer: usize,
    start: usize,
    after_head: usize,
    end: usize,
}

impl Entry {
    fn annotation(&self, t: &[Token]) -> Option<Range<usize>> {
        let colon = self.after_head + usize::from(tok(t, self.after_head) == "?");
        (colon < self.end && tok(t, colon) == ":").then_some(colon + 1..self.initializer)
    }
}

fn type_parameter_name(token: &Token) -> bool {
    word(token) && !matches!(token.text, "const" | "out" | "in")
}

/// A comma inside a paired group belongs to that entry. Parameter annotations
/// also recognize angle groups; destructuring patterns only group JS delimiters.
fn entries<'a>(
    tokens: &'a [Token],
    bounds: Range<usize>,
    skip: impl Fn(&str) -> bool + 'a,
    mut pair: impl FnMut(usize, usize) -> Option<usize> + 'a,
) -> impl Iterator<Item = Entry> + 'a {
    let mut window = EntryWindow { tokens, bounds };
    std::iter::from_fn(move || window.next(&skip, &mut pair))
}

struct EntryWindow<'a, 's> {
    tokens: &'a [Token<'s>],
    bounds: Range<usize>,
}

impl EntryWindow<'_, '_> {
    fn next(
        &mut self,
        skip: impl Fn(&str) -> bool,
        pair: &mut impl FnMut(usize, usize) -> Option<usize>,
    ) -> Option<Entry> {
        let start = self.bounds.clone().find(|&p| {
            let text = self.tokens[p].text;
            text != "," && !skip(text)
        })?;
        let after_head = pair(start, start).unwrap_or(start) + 1;
        let mut cursor = after_head;
        let mut initializer = self.bounds.end;
        while cursor < self.bounds.end && self.tokens[cursor].text != "," {
            initializer = initializer.min(self.initializer(cursor));
            cursor = pair(cursor, start).unwrap_or(cursor) + 1;
        }
        self.bounds.start = cursor;
        Some(Entry {
            initializer: initializer.min(cursor),
            start,
            after_head,
            end: cursor.min(self.bounds.end),
        })
    }

    fn initializer(&self, i: usize) -> usize {
        if self.tokens[i].text == "=" && tok(self.tokens, i + 1) != ">" {
            i
        } else {
            self.bounds.end
        }
    }
}

/// A JS destructuring entry distinguishes the written property, the binding target,
/// and the expressions evaluated to compute a key or default value.
struct PatternField {
    key: Option<Range<usize>>,
    target: usize,
    default: Range<usize>,
}

fn pattern_fields<'a>(
    t: &'a [Token],
    pairs: &'a [usize],
    open: usize,
) -> impl Iterator<Item = PatternField> + 'a {
    let object = t[open].text == "{";
    let limit = pairs[open].min(t.len());
    entries(
        t,
        open + 1..limit,
        |word| matches!(word, "." | "*"),
        |p, _| paired_end(t, pairs, p),
    )
    .filter_map(move |field| {
        let named = object && tok(t, field.after_head) == ":";
        let target = if named {
            field.after_head + 1
        } else {
            field.start
        };
        (target < field.end).then(|| PatternField {
            key: (named && t[field.start].text == "[")
                .then(|| field.start + 1..field.after_head - 1),
            target,
            default: field.initializer + 1..field.end,
        })
    })
}

#[derive(Clone, Copy)]
struct BindingSyntax<'a, 's> {
    tokens: &'a [Token<'s>],
    pairs: &'a [usize],
}

#[derive(Clone, Copy)]
pub(in crate::domain::code::script) struct CallableSignature {
    pub head: usize,
    pub parentheses: (usize, usize),
    pub body: usize,
}

enum Initializer<'t, 's> {
    Shape(usize),
    Iterated(&'t Token<'s>),
    Type(&'t Token<'s>),
    Assignment(usize),
}

impl<'t, 's> Initializer<'t, 's> {
    fn read(tokens: &'t [Token<'s>], name: usize, variable: bool) -> Option<Self> {
        let first = name + 2;
        let value = tokens.get(first)?;
        let operator = tokens[name + 1].text;
        if value.text == "{" && Self::shape(operator, variable) {
            return Some(Self::Shape(first));
        }
        match operator {
            "of" => Some(Self::Iterated(value)),
            ":" if variable => Some(Self::Type(value)),
            "=" => Some(Self::Assignment(first)),
            _ => None,
        }
    }

    fn shape(operator: &str, variable: bool) -> bool {
        operator == ":" || (variable && operator == "=")
    }
}

/// Declarations and references produced in one lexical scope.
pub(in crate::domain::code::script) struct ScopeFacts<'a> {
    source: &'a str,
    scope: usize,
    analysis: &'a mut Analysis,
    evidence: &'a mut Evidence,
}

impl<'a> ScopeFacts<'a> {
    pub(in crate::domain::code::script) fn new(
        source: &'a str,
        scope: usize,
        analysis: &'a mut Analysis,
        evidence: &'a mut Evidence,
    ) -> Self {
        Self {
            source,
            scope,
            analysis,
            evidence,
        }
    }

    // Record written type evidence independently of whether the named type
    // can be resolved. An object literal acquires its scope at its opening brace.
    pub(in crate::domain::code::script) fn initializer(
        &mut self,
        tokens: &[Token],
        name: usize,
        id: usize,
        variable: bool,
    ) -> Option<usize> {
        let initializer = Initializer::read(tokens, name, variable)?;
        let (value, table) = match initializer {
            Initializer::Shape(open) => return Some(open),
            Initializer::Iterated(value) => (value, &mut self.evidence.iterated),
            Initializer::Type(value) => (value, &mut self.evidence.types),
            Initializer::Assignment(at) => {
                self.assignment(tokens, at, id, variable);
                return None;
            }
        };
        if word(value) {
            table.insert(id, (self.scope, value.span.clone()));
        }
        None
    }

    fn assignment(&mut self, tokens: &[Token], at: usize, id: usize, variable: bool) {
        let value = &tokens[at];
        let next = at + 1;
        if value.text == "new" {
            self.constructor_type(tokens, next, id, variable);
        }
        if tok(tokens, next) == "(" && word(value) {
            self.evidence
                .result_types
                .insert(id, (self.scope, value.span.clone()));
        }
    }

    fn constructor_type(
        &mut self,
        tokens: &[Token],
        constructor: usize,
        id: usize,
        variable: bool,
    ) {
        if variable && tokens.get(constructor).is_some_and(word) {
            self.evidence
                .types
                .insert(id, (self.scope, tokens[constructor].span.clone()));
        }
        if matches!(tok(tokens, constructor), "Set" | "Array")
            && tok(tokens, constructor + 1) == "<"
            && let Some(element) = tokens.get(constructor + 2).filter(|t| word(t))
        {
            self.evidence
                .element_types
                .insert(id, (self.scope, element.span.clone()));
        }
    }

    fn bind(&mut self, token: &Token, kind: &'static str) -> usize {
        self.evidence
            .bind_token(self.analysis, self.scope, (token, kind))
    }

    pub(in crate::domain::code::script) fn pattern(
        &mut self,
        t: &[Token],
        end: &[usize],
        open: usize,
        kind: &'static str,
    ) -> Range<usize> {
        let first = self.analysis.declarations.len();
        let mut nested = vec![open];
        while let Some(begin) = nested.pop() {
            for field in pattern_fields(t, end, begin) {
                nested.extend(self.bind_field(t, end, kind, field));
            }
        }
        first..self.analysis.declarations.len()
    }

    /// Apply one field in its enclosing scope. A nested target is returned to
    /// the existing worklist, preserving declaration order for sibling patterns.
    fn bind_field(
        &mut self,
        t: &[Token],
        pairs: &[usize],
        kind: &'static str,
        field: PatternField,
    ) -> Option<usize> {
        if let Some(key) = field.key {
            self.expression_names(t, pairs, key);
        }
        if !field.default.is_empty() {
            self.expression_names(t, pairs, field.default);
        }
        let target = &t[field.target];
        match target.text {
            "[" | "{" => return Some(field.target),
            _ if word(target) => {
                self.bind(target, kind);
            }
            _ => {}
        }
        None
    }

    fn expression_names(&mut self, t: &[Token], pairs: &[usize], range: Range<usize>) {
        super::super::reference_chain::References::new(t, pairs, Language::Js).expression(
            range,
            self.scope,
            self.analysis,
            self.evidence,
        );
    }

    pub(in crate::domain::code::script) fn parameters(
        &mut self,
        angles: &mut AngleEnds,
        end: &[usize],
        params: Range<usize>,
        typed: bool,
    ) -> usize {
        let t = angles.tokens;
        let syntax = BindingSyntax {
            tokens: t,
            pairs: end,
        };
        let mut parameter_environment = false;
        let first_param = params.start;
        let mut first_shape = None;
        for entry in parameter_entries(angles, end, params) {
            let annotation = entry.annotation(t);
            parameter_environment |= typed && self.js_parameter(t, end, &entry, annotation.clone());
            let bindings = self.parameter_binding(t, end, entry.start);
            first_shape = annotation
                .filter(|region| entry.start == first_param && tok(t, region.start) == "{")
                .map(|region| (region, bindings))
                .or(first_shape);
        }
        if let Some((region, bindings)) = first_shape {
            let shape = self.type_literal(syntax, region, TypeLiteral::Parameter);
            self.parameter_shape(tok(t, first_param), shape, bindings);
        }
        // A default can see the function's parameters and enclosing bindings.
        // Declarations in the body are introduced in a child environment.
        if parameter_environment {
            self.evidence
                .child_scope(self.scope, self.evidence.scopes[self.scope].owner)
        } else {
            self.scope
        }
    }

    // Only JavaScript evaluates parameter defaults in the parameter environment.
    // A destructuring parameter can also contain executable keys and defaults.
    fn js_parameter(
        &mut self,
        t: &[Token],
        pairs: &[usize],
        entry: &Entry,
        annotation: Option<Range<usize>>,
    ) -> bool {
        if let Some(region) = annotation {
            self.type_uses(t, region);
        }
        if entry.initializer < entry.end {
            self.expression_names(t, pairs, entry.initializer + 1..entry.end);
            return true;
        }
        matches!(t[entry.start].text, "{" | "[")
    }

    fn parameter_binding(&mut self, t: &[Token], end: &[usize], at: usize) -> Option<Range<usize>> {
        match t[at].text {
            "{" | "[" => Some(self.destructured_parameter(t, end, at)),
            _ if word(&t[at]) => {
                let id = self.bind(&t[at], "parameter");
                self.named_type(t, at, id);
                None
            }
            _ => None,
        }
    }

    fn destructured_parameter(&mut self, t: &[Token], pairs: &[usize], at: usize) -> Range<usize> {
        let ids = self.pattern(t, pairs, at, "parameter");
        if !ids.is_empty() {
            self.evidence.destructured_parameters.push(ids.clone());
        }
        ids
    }

    /// Only immediate fields belong to this shape. Nested literals and
    /// method arguments are independent scopes.
    fn type_literal(
        &mut self,
        syntax: BindingSyntax,
        region: Range<usize>,
        site: TypeLiteral,
    ) -> usize {
        let BindingSyntax { tokens: t, pairs } = syntax;
        let Range {
            start: open,
            end: limit,
        } = region;
        let shape = self.evidence.scopes.len();
        self.evidence.scopes.push(Default::default());
        for at in TypeLiteral::fields(t, pairs, open, limit) {
            let field = &t[at];
            let id =
                self.evidence
                    .declare_member(self.analysis, shape, field.text, field.span.clone());
            if matches!(site, TypeLiteral::Return) {
                self.named_type(t, at, id);
            }
        }
        shape
    }

    fn named_type(&mut self, t: &[Token], at: usize, id: usize) {
        if tok(t, at + 1) == ":"
            && let Some(ty) = t.get(at + 2).filter(|t| word(t))
        {
            self.evidence
                .types
                .insert(id, (self.scope, ty.span.clone()));
        }
    }

    fn parameter_shape(
        &mut self,
        parameter: &str,
        shape: usize,
        destructured: Option<Range<usize>>,
    ) {
        if let Some(owner) = self.evidence.scopes[self.scope].owner {
            self.evidence.props.insert(owner, shape);
        }
        if let Some(ids) = destructured {
            for id in ids {
                let span = self.analysis.declarations[id].span.clone();
                if let Some(target) = self.evidence.members.get(shape, &self.source[span.clone()]) {
                    UseRequest::reference(span, self.scope, Lookup::Known(target))
                        .publish(self.analysis, self.evidence);
                }
            }
        } else if let Some(Binding::Local(id)) =
            self.evidence.scopes[self.scope].bindings.get(parameter)
        {
            self.evidence.shapes.insert(*id, shape);
        }
    }

    pub(in crate::domain::code::script) fn callable_signature(
        &mut self,
        angles: &mut AngleEnds,
        pairs: &[usize],
        signature: CallableSignature,
    ) -> usize {
        let (open, close) = signature.parentheses;
        self.signature(angles, pairs, signature);
        self.parameters(angles, pairs, open + 1..close, true)
    }

    pub(in crate::domain::code::script) fn signature(
        &mut self,
        angles: &mut AngleEnds,
        pairs: &[usize],
        signature: CallableSignature,
    ) {
        let CallableSignature {
            head,
            parentheses: (open, close),
            body,
        } = signature;
        let t = angles.tokens;
        self.type_parameters(angles, pairs, head + 1, open);
        self.return_annotation(t, pairs, close + 1, body);
        self.type_uses(t, close + 1..body.min(t.len()));
    }

    fn type_parameters(
        &mut self,
        angles: &mut AngleEnds,
        pairs: &[usize],
        open: usize,
        paren: usize,
    ) {
        let t = angles.tokens;
        if tok(t, open) != "<" {
            self.type_uses(t, open..paren);
            return;
        }
        let mut first = true;
        let mut p = open + 1;
        while p < paren.saturating_sub(1) {
            match tok(t, p) {
                "," => first = true,
                _ if first && type_parameter_name(&t[p]) => {
                    self.bind(&t[p], "local_binding");
                    first = false;
                }
                _ => p = self.type_parameter_operand(angles, pairs, p..paren),
            }
            p += 1;
        }
    }

    fn type_parameter_operand(
        &mut self,
        angles: &mut AngleEnds,
        pairs: &[usize],
        region: Range<usize>,
    ) -> usize {
        let p = region.start;
        let t = angles.tokens;
        if !matches!(tok(t, p), "<" | "(" | "{" | "[") {
            self.type_use(t, p);
            return p;
        }
        let stop = group_end(angles, pairs, p).unwrap_or(p);
        self.type_uses(t, p + 1..stop.min(region.end));
        stop
    }

    fn return_annotation(&mut self, t: &[Token], pairs: &[usize], colon: usize, body: usize) {
        let p = colon + 1;
        if tok(t, colon) != ":" || p >= body {
            return;
        }
        let Some(owner) = self.evidence.scopes[self.scope].owner else {
            return;
        };
        if tok(t, p) == "{" {
            let shape = self.type_literal(
                BindingSyntax { tokens: t, pairs },
                p..body,
                TypeLiteral::Return,
            );
            self.evidence.annotated_shapes.insert(owner, shape);
        } else if word(&t[p]) && !keyword(Language::Js, t[p].text) {
            self.evidence
                .annotated_returns
                .insert(owner, (self.scope, t[p].span.clone()));
        }
    }

    fn type_uses(&mut self, t: &[Token], tokens: Range<usize>) {
        for p in tokens {
            self.type_use(t, p);
        }
    }

    fn type_use(&mut self, t: &[Token], p: usize) {
        if word(&t[p])
            && !keyword(Language::Js, t[p].text)
            && !matches!(tok(t, p + 1), ":" | "?")
            && (p == 0 || tok(t, p - 1) != ".")
        {
            let lookup = Lookup::Name {
                scope: self.scope,
                receiver: None,
            };
            UseRequest::reference(t[p].span.clone(), self.scope, lookup)
                .publish(self.analysis, self.evidence);
        }
    }
}
