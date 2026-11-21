//! Direct lexical bindings for Python and ECMAScript. A module edge must name a
//! captured module. Dynamic members and unresolved modules yield no target.
mod imports;
mod jsx;
mod labels;
pub(super) use labels::public_labels;
mod projects;
mod resolution;
mod strings;
mod syntax;
use super::{Analysis, Call, Declaration, Kind, Language, Names, Site, Token, Unit};
use imports::imports;
pub(super) use resolution::resolve;
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};
use syntax::*;

#[derive(Default)]
struct Scope {
    shape: bool,
    receiver: Option<usize>,
    this_boundary: bool,
    parent: Option<usize>,
    owner: Option<usize>,
    bindings: HashMap<String, Binding>,
    directives: HashMap<String, bool>,
}
enum Binding {
    Local(usize),
    Import(usize),
    Package,
    Conflicting,
}
enum Forward {
    Namespace(usize),
    Local(String),
    Import(usize),
}
struct Import {
    module: String,
    name: Option<String>,
}
enum Lookup {
    Name {
        scope: usize,
        receiver: Option<Range<usize>>,
    },
    Argument {
        scope: usize,
        callee: Range<usize>,
    },
    Known(usize),
    Chained {
        prior: usize,
        scope: usize,
        qualification: Option<Range<usize>>,
    },
    Construct {
        scope: usize,
    },
    Prop {
        scope: usize,
        component: Range<usize>,
    },
    Imported(usize),
}
#[derive(Default)]
pub(super) struct Evidence {
    scopes: Vec<Scope>,
    imports: Vec<Import>,
    uses: Vec<Lookup>,
    exports: HashMap<String, usize>,
    forward: HashMap<String, Forward>,
    stars: Vec<String>,
    packages: HashMap<(usize, String), usize>,
    wildcards: Vec<(usize, usize)>,
    all: Option<Vec<String>>,
    dynamic_all: bool,
    families: HashMap<usize, Vec<usize>>,
    shapes: HashMap<usize, usize>,
    result_shapes: HashMap<usize, usize>,
    annotated_returns: HashMap<usize, (usize, Range<usize>)>,
    annotated_shapes: HashMap<usize, usize>,
    result_types: HashMap<usize, (usize, Range<usize>)>,
    element_types: HashMap<usize, (usize, Range<usize>)>,
    iterated: HashMap<usize, (usize, Range<usize>)>,
    props: HashMap<usize, usize>,
    members: HashMap<(usize, String), usize>,
    types: HashMap<usize, (usize, Range<usize>)>,
    functions: HashMap<usize, usize>,
    destructured_parameters: Vec<Range<usize>>,
    self_params: HashMap<usize, usize>,
}
impl Evidence {
    fn binding(&self, mut scope: usize, name: &str) -> Option<&Binding> {
        let mut nonlocal = false;
        loop {
            if scope == 0 && nonlocal {
                return None;
            }
            if scope != 0
                && let Some(global) = self.scopes[scope].directives.get(name)
            {
                if *global {
                    scope = 0;
                    nonlocal = false;
                } else {
                    scope = self.scopes[scope].parent?;
                    nonlocal = true;
                    continue;
                }
            }

            let s = &self.scopes[scope];
            if let Some(b) = s.bindings.get(name) {
                return Some(b);
            }
            scope = s.parent?;
        }
    }
    fn declare(&mut self, scope: usize, name: &str, binding: Binding) {
        self.scopes[scope]
            .bindings
            .entry(name.to_owned())
            .and_modify(|b| *b = Binding::Conflicting)
            .or_insert(binding);
    }
}
fn word(t: &Token) -> bool {
    t.kind == Kind::Word && !t.text.starts_with(|c: char| c.is_ascii_digit())
}
fn keyword(lang: Language, s: &str) -> bool {
    let common = matches!(
        s,
        "as" | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "else"
            | "for"
            | "from"
            | "if"
            | "import"
            | "in"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
    );
    common
        || match lang {
            Language::Python => matches!(
                s,
                "False"
                    | "None"
                    | "True"
                    | "and"
                    | "assert"
                    | "case"
                    | "def"
                    | "del"
                    | "elif"
                    | "except"
                    | "finally"
                    | "global"
                    | "is"
                    | "lambda"
                    | "match"
                    | "nonlocal"
                    | "not"
                    | "or"
                    | "pass"
                    | "raise"
            ),
            _ => matches!(
                s,
                "abstract"
                    | "any"
                    | "boolean"
                    | "case"
                    | "catch"
                    | "const"
                    | "constructor"
                    | "debugger"
                    | "declare"
                    | "default"
                    | "delete"
                    | "do"
                    | "enum"
                    | "export"
                    | "extends"
                    | "false"
                    | "finally"
                    | "function"
                    | "get"
                    | "implements"
                    | "instanceof"
                    | "interface"
                    | "let"
                    | "module"
                    | "namespace"
                    | "new"
                    | "null"
                    | "number"
                    | "of"
                    | "private"
                    | "protected"
                    | "public"
                    | "readonly"
                    | "set"
                    | "static"
                    | "string"
                    | "super"
                    | "switch"
                    | "this"
                    | "throw"
                    | "true"
                    | "type"
                    | "typeof"
                    | "undefined"
                    | "unknown"
                    | "var"
                    | "void"
            ),
        }
}
fn tok<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |x| x.text)
}
fn add_use(
    a: &mut Analysis,
    e: &mut Evidence,
    t: &Token,
    scope: usize,
    is_call: bool,
    lookup: Lookup,
) {
    a.calls.push(Call {
        span: t.span.clone(),
        owner: e.scopes[scope].owner,
        is_call,
    });
    e.uses.push(lookup);
}
pub(super) fn analyze(lang: Language, source: &str, t: &[Token]) -> Analysis {
    let python = lang == Language::Python;
    let mut a = Analysis::default();
    let mut e = Evidence::default();
    e.scopes.push(Scope::default());
    let end = paired(t);
    let markup = if python {
        jsx::Markup::default()
    } else {
        jsx::classify(t, &end)
    };
    let mut active = vec![(0usize, usize::MAX)];
    let mut i = 0;
    let mut comprehensions = HashSet::new();
    if python {
        let mut stack = Vec::new();
        for (j, token) in t.iter().enumerate() {
            match token.text {
                "(" | "[" | "{" if end[j] < t.len() => stack.push(j),
                ")" | "]" | "}" => {
                    if stack.last() == Some(&end[j]) {
                        stack.pop();
                    }
                }
                "for" => {
                    if let Some(&open) = stack.last() {
                        comprehensions.insert(open);
                    }
                }
                _ => {}
            }
        }
    }
    let mut comp_stack: Vec<(usize, usize)> = Vec::new();
    let mut extra_vars = HashSet::new();
    let mut pending_shapes = HashMap::new();
    let mut mixed_returns = HashSet::new();
    let mut written_uses = HashMap::new();
    let mut signatures: HashMap<(usize, String), Vec<usize>> = HashMap::new();
    while i < t.len() {
        let token = &t[i];
        if python {
            if i == 0 || t[i - 1].line != token.line {
                let indent = super::indentation(source, token.span.start);
                while active.len() > 1 && active.last().unwrap().1 >= indent {
                    active.pop();
                }
            }
        } else {
            while active.len() > 1 && active.last().unwrap().1 <= i {
                active.pop();
            }
        }
        while comp_stack.last().is_some_and(|&(stop, _)| stop <= i) {
            comp_stack.pop();
        }
        let mut scope = comp_stack.last().map_or(active.last().unwrap().0, |v| v.1);
        if comprehensions.contains(&i) {
            let child = e.scopes.len();
            e.scopes.push(Scope {
                parent: Some(scope),
                owner: e.scopes[scope].owner,
                ..Default::default()
            });
            comp_stack.push((end[i], child));
            scope = child;
        }
        if markup.ignored(i) {
            if let Some(span) = markup.props.get(&i) {
                add_use(
                    &mut a,
                    &mut e,
                    token,
                    scope,
                    false,
                    Lookup::Prop {
                        scope,
                        component: span.clone(),
                    },
                );
            }
            i += 1;
            continue;
        }
        let prev = i.checked_sub(1).map_or("", |j| tok(t, j));
        let next = tok(t, i + 1);
        if python && scope == 0 && token.text == "__all__" {
            let begin = i + 2;
            if next == "="
                && matches!(tok(t, begin), "[" | "(")
                && end[begin] < t.len()
                && e.all.is_none()
            {
                let mut vals = Vec::new();
                for t in &t[begin + 1..end[begin]] {
                    if t.text == "," {
                        continue;
                    }
                    if t.kind != Kind::String || t.text.len() < 2 || t.text.contains('\\') {
                        e.dynamic_all = true;
                        break;
                    }
                    vals.push(t.text[1..t.text.len() - 1].to_owned());
                }
                e.all = Some(vals);
            } else {
                e.dynamic_all = true;
            }
        }
        if token.kind == Kind::String {
            let prefix = if i > 0 && t[i - 1].span.end == token.span.start {
                prev
            } else {
                ""
            };
            strings::visit(lang, token, prefix, scope, &mut a, &mut e);
            i += 1;
            continue;
        }
        if !python
            && token.text == "export"
            && let Some(after) = imports::exports(t, i, scope, &mut a, &mut e)
        {
            i = after;
            continue;
        }
        if prev != "."
            && (token.text == "import" || python && token.text == "from")
            && let Some(after) = imports(t, i, lang, scope, &mut a, &mut e)
        {
            i = after;
            continue;
        }
        if !python
            && prev != "."
            && matches!(token.text, "for" | "catch")
            && next == "("
            && end[i + 1] < t.len()
        {
            let close = end[i + 1];
            let body = close + 1;
            let stop = if tok(t, body) == "{" {
                end[body]
            } else {
                expr_end(t, &end, body)
            };
            let child = e.scopes.len();
            e.scopes.push(Scope {
                parent: Some(scope),
                owner: e.scopes[scope].owner,
                ..Default::default()
            });
            active.push((child, stop));
            if token.text == "catch" {
                parameters(t, &end, i + 2, close, child, &mut a, &mut e);
                i = body;
            } else {
                i += 2;
            }
            continue;
        }
        if !python && token.text == "{" && end[i] < t.len() {
            let id = e.scopes.len();
            e.scopes.push(Scope {
                parent: Some(scope),
                owner: e.scopes[scope].owner,
                ..Default::default()
            });
            if prev == "return"
                && let Some(owner) = e.scopes[scope].owner
            {
                e.scopes[id].shape = true;
                if e.result_shapes.insert(owner, id).is_some() {
                    mixed_returns.insert(owner);
                }
            }
            if let Some(decl) = pending_shapes.remove(&i) {
                e.scopes[id].shape = true;
                e.shapes.insert(decl, id);
            }
            active.push((id, end[i]));
            i += 1;
            continue;
        }
        if !python && (token.text == "(" || word(token)) {
            let close = if token.text == "(" { end[i] } else { i };
            if close < t.len() && tok(t, close + 1) == "=" && tok(t, close + 2) == ">" {
                let child = e.scopes.len();
                e.scopes.push(Scope {
                    parent: Some(scope),
                    owner: None,
                    ..Default::default()
                });
                parameters(
                    t,
                    &end,
                    if close == i { i } else { i + 1 },
                    if close == i { i + 1 } else { close },
                    child,
                    &mut a,
                    &mut e,
                );
                let begin = close + 3;
                let block = tok(t, begin) == "{";
                active.push((
                    child,
                    if block {
                        end[begin]
                    } else {
                        expr_end(t, &end, begin)
                    },
                ));
                i = begin + usize::from(block);
                continue;
            }
        }
        let member_end = if token.text == "[" { end[i] } else { i };
        if !python
            && (word(token) || token.text == "[")
            && (!keyword(lang, token.text)
                || matches!(token.text, "constructor" | "get" | "set" | "of"))
            && matches!(
                prev,
                "{" | "}"
                    | ","
                    | ";"
                    | "async"
                    | "static"
                    | "get"
                    | "set"
                    | "public"
                    | "private"
                    | "protected"
                    | "override"
                    | "*"
                    | "#"
            )
            && !(prev == "*" && i >= 2 && tok(t, i - 2) == "function")
            && let Some(paren) = args(t, member_end + 1)
            && let Some(body) = method_body(t, &end, end[paren])
        {
            let id = a.declarations.len();
            let mut span = token.span.start..t[member_end].span.end;
            if prev == "#" {
                span.start = t[i - 1].span.start;
            }
            a.declarations.push(Declaration {
                span,
                kind: "method_definition",
            });
            if e.scopes[scope].shape {
                e.members.insert(
                    (scope, source[a.declarations[id].span.clone()].to_owned()),
                    id,
                );
            }
            let child = e.scopes.len();
            e.scopes.push(Scope {
                parent: Some(scope),
                owner: Some(id),
                ..Default::default()
            });
            e.scopes[child].receiver = Some(scope);
            signature(t, &end, member_end, paren, body, child, &mut a, &mut e);
            parameters(t, &end, paren + 1, end[paren], child, &mut a, &mut e);
            active.push((child, end[body]));
            i = body + 1;
            continue;
        }
        if !python
            && token.text == "return"
            && next != "{"
            && let Some(owner) = e.scopes[scope].owner
        {
            mixed_returns.insert(owner);
        }
        let extra = extra_vars.remove(&i);
        if !python && matches!(token.text, "const" | "let" | "var") && matches!(next, "[" | "{") {
            pattern(t, &end, i + 1, scope, "local_binding", &mut a, &mut e);
            i = end[i + 1].min(t.len()) + 1;
            continue;
        }
        if !word(token) && !extra {
            i += 1;
            continue;
        }
        let function = if python {
            token.text == "def"
        } else {
            token.text == "function"
        };
        let kind = if extra {
            Some("variable_declarator")
        } else {
            lang.decl(token.text)
        };
        let generator = function && !python && next == "*";
        let name = i + 1 + usize::from(generator);
        if let Some(kind) = kind.filter(|_| prev != "." && t.get(name).is_some_and(word)) {
            let expr_prev = if prev == "async" && i >= 2 {
                tok(t, i - 2)
            } else {
                prev
            };
            let expression = !python
                && function
                && matches!(
                    expr_prev,
                    "=" | "(" | "," | ":" | "return" | "[" | "?" | "!"
                );
            let kind = if expression {
                "function_expression"
            } else if generator {
                "generator_function_declaration"
            } else {
                kind
            };
            if !python
                && function
                && let Some(paren) = args(t, name + 1)
                && method_body(t, &end, end[paren]).is_none()
            {
                let mut stop = end[paren].saturating_add(1);
                while stop < t.len() && !matches!(tok(t, stop), ";" | "}" | "export" | "function") {
                    if matches!(tok(t, stop), "(" | "{" | "[") {
                        stop = end[stop];
                    }
                    stop += 1;
                }
                if tok(t, stop) == ";" {
                    let id = a.declarations.len();
                    a.declarations.push(Declaration {
                        span: t[name].span.clone(),
                        kind: "function_overload",
                    });
                    signatures
                        .entry((scope, t[name].text.to_owned()))
                        .or_default()
                        .push(id);
                    let child = e.scopes.len();
                    e.scopes.push(Scope {
                        parent: Some(scope),
                        owner: Some(id),
                        ..Default::default()
                    });
                    parameters(t, &end, paren + 1, end[paren], child, &mut a, &mut e);
                    signature(t, &end, name, paren, stop, child, &mut a, &mut e);
                    i = stop + 1;
                    continue;
                }
                i = name + 1;
                continue;
            }
            if (!python
                && (prev == "as"
                    || (kind == "variable_declarator"
                        && matches!(tok(t, name + 1), "extends" | "is"))
                    || (token.text == "type"
                        && tok(t, name + 1) != "="
                        && tok(t, name + 1) != "<")))
                || function && args(t, name + 1).is_none()
            {
                i += 1;
                continue;
            }
            let kind = if kind == "variable_declarator" && matches!(tok(t, name + 1), "of" | "in") {
                "local_binding"
            } else {
                kind
            };
            if !python && kind == "variable_declarator" && !extra {
                let mut start = name + 1;
                while start < t.len() {
                    let comma = expr_end(t, &end, start);
                    if tok(t, comma) != ","
                        || !t.get(comma + 1).is_some_and(word)
                        || !matches!(tok(t, comma + 2), "=" | ":" | "," | ";" | "!")
                    {
                        break;
                    }
                    extra_vars.insert(comma);
                    start = comma + 2;
                }
            }
            let id = a.declarations.len();
            a.declarations.push(Declaration {
                span: t[name].span.clone(),
                kind,
            });
            if !expression {
                e.declare(scope, t[name].text, Binding::Local(id));
            }
            if !expression && e.scopes[scope].shape {
                e.members.insert((scope, t[name].text.to_owned()), id);
            }
            if !python && matches!(token.text, "class" | "interface" | "type") {
                let mut p = name + 1;
                if tok(t, p) == "<" {
                    p = angle_end(t, p).map_or(p, |n| n + 1);
                }
                if tok(t, p) == "=" {
                    p += 1;
                }
                while matches!(tok(t, p), "extends" | "implements" | ",") {
                    p += 1;
                    if !t.get(p).is_some_and(word) {
                        break;
                    }
                    p += 1;
                    while tok(t, p) == "." && t.get(p + 1).is_some_and(word) {
                        p += 2;
                    }
                    if tok(t, p) == "<" {
                        p = angle_end(t, p).map_or(p, |n| n + 1);
                    }
                }
                if tok(t, p) == "{" {
                    pending_shapes.insert(p, id);
                }
            }
            if kind == "variable_declarator" && tok(t, name + 1) == "=" && tok(t, name + 2) == "{" {
                pending_shapes.insert(name + 2, id);
            }
            if !python {
                if tok(t, name + 1) == "of" && t.get(name + 2).is_some_and(word) {
                    e.iterated.insert(id, (scope, t[name + 2].span.clone()));
                }
                if tok(t, name + 1) == "="
                    && tok(t, name + 2) == "new"
                    && matches!(tok(t, name + 3), "Set" | "Array")
                    && tok(t, name + 4) == "<"
                    && t.get(name + 5).is_some_and(word)
                {
                    e.element_types
                        .insert(id, (scope, t[name + 5].span.clone()));
                }
                if tok(t, name + 1) == "="
                    && t.get(name + 2).is_some_and(word)
                    && tok(t, name + 3) == "("
                {
                    e.result_types.insert(id, (scope, t[name + 2].span.clone()));
                }
                if tok(t, name + 1) == ":" && tok(t, name + 2) == "{" {
                    pending_shapes.insert(name + 2, id);
                }
            }
            if kind == "variable_declarator" {
                let ty = if tok(t, name + 1) == ":" {
                    Some(name + 2)
                } else if tok(t, name + 1) == "=" && tok(t, name + 2) == "new" {
                    Some(name + 3)
                } else {
                    None
                };
                if let Some(j) = ty.filter(|&j| t.get(j).is_some_and(word)) {
                    e.types.insert(id, (scope, t[j].span.clone()));
                }
            }

            let exported = python
                || t[..i]
                    .iter()
                    .rev()
                    .take_while(|x| {
                        matches!(
                            x.text,
                            "default" | "async" | "export" | "declare" | "abstract"
                        )
                    })
                    .any(|x| x.text == "export");
            if exported && scope == 0 {
                let default = t[..i]
                    .iter()
                    .rev()
                    .take_while(|x| matches!(x.text, "async" | "default" | "export"))
                    .any(|x| x.text == "default");
                e.exports
                    .insert(if default { "default" } else { t[name].text }.into(), id);
            }
            if !python && kind == "variable_declarator" && tok(t, name + 1) == "=" {
                let mut first = name + 2;
                if tok(t, first) == "async" {
                    first += 1;
                }
                let (begin, close) = if tok(t, first) == "(" && end[first] < t.len() {
                    (first + 1, end[first])
                } else {
                    (first, first)
                };
                if tok(t, close + 1) == "=" && tok(t, close + 2) == ">" && tok(t, close + 3) == "{"
                {
                    let child = e.scopes.len();
                    e.scopes.push(Scope {
                        parent: Some(scope),
                        owner: Some(id),
                        ..Default::default()
                    });
                    parameters(
                        t,
                        &end,
                        begin,
                        if begin == first { close + 1 } else { close },
                        child,
                        &mut a,
                        &mut e,
                    );
                    active.push((child, end[close + 3]));
                    i = close + 4;
                    continue;
                }
            }
            if function || python && token.text == "class" {
                let paren = args(t, name + 1).unwrap_or(name + 1);
                let close = if tok(t, paren) == "(" {
                    end[paren]
                } else {
                    name
                };
                if close >= t.len() {
                    i = name + 1;
                    continue;
                }
                let mut body = close + 1;
                while body < t.len()
                    && tok(t, body) != if python { ":" } else { "{" }
                    && tok(t, body) != ";"
                    && t[body].line <= t[close].line + 1
                {
                    body += 1;
                }
                if !python {
                    body = method_body(t, &end, close).unwrap_or(t.len());
                }
                if body < t.len() && tok(t, body) == if python { ":" } else { "{" } {
                    let child = e.scopes.len();
                    e.scopes.push(Scope {
                        parent: Some(scope),
                        owner: Some(id),
                        ..Default::default()
                    });
                    if python && token.text == "class" {
                        e.scopes[child].shape = true;
                        e.shapes.insert(id, child);
                    }
                    if python
                        && function
                        && e.scopes[scope].shape
                        && matches!(tok(t, paren + 1), "self" | "cls")
                    {
                        e.scopes[child].receiver = Some(scope);
                    }
                    if function {
                        e.scopes[child].this_boundary = true;
                        if python && e.scopes[scope].shape {
                            e.scopes[child].parent = e.scopes[scope].parent;
                        }
                        if expression {
                            e.declare(child, t[name].text, Binding::Local(id));
                        }
                        e.functions.insert(id, child);
                        if !python {
                            signature(t, &end, name, paren, body, child, &mut a, &mut e);
                        }
                        parameters(t, &end, paren + 1, close, child, &mut a, &mut e);
                        if python
                            && e.scopes[child].receiver.is_some()
                            && let Some(Binding::Local(param)) =
                                e.scopes[child].bindings.get(tok(t, paren + 1))
                        {
                            e.self_params.insert(*param, scope);
                        }
                    }
                    active.push((
                        child,
                        if python {
                            super::indentation(source, token.span.start)
                        } else {
                            end[body]
                        },
                    ));
                    i = body + 1;
                    continue;
                }
            }
            i = name + 1;
            continue;
        }
        if !python
            && e.scopes[scope].shape
            && matches!(prev, "{" | ",")
            && word(token)
            && matches!(next, "," | "}")
        {
            let id = a.declarations.len();
            e.members.insert((scope, token.text.to_owned()), id);
            a.declarations.push(Declaration {
                span: token.span.clone(),
                kind: "local_binding",
            });
        }
        if !python
            && e.scopes[scope].shape
            && word(token)
            && (next == ":"
                || next == "?" && tok(t, i + 2) == ":"
                || matches!(prev, "private" | "readonly" | "public" | "protected" | "#")
                    && matches!(next, "=" | ";"))
        {
            let mut span = token.span.clone();
            if prev == "#" {
                span.start = t[i - 1].span.start;
            }
            let id = a.declarations.len();
            e.members
                .insert((scope, source[span.clone()].to_owned()), id);
            a.declarations.push(Declaration {
                span,
                kind: "local_binding",
            });
            let mut ty = i + 1;
            if tok(t, ty) == "?" {
                ty += 1;
            }
            if tok(t, ty) == ":" && t.get(ty + 1).is_some_and(word) {
                if tok(t, ty + 1) == "ReturnType"
                    && tok(t, ty + 2) == "<"
                    && tok(t, ty + 3) == "typeof"
                    && t.get(ty + 4).is_some_and(word)
                {
                    e.result_types.insert(id, (scope, t[ty + 4].span.clone()));
                } else {
                    e.types.insert(id, (scope, t[ty + 1].span.clone()));
                }
            }
            i += 1;
            continue;
        }
        if python && matches!(token.text, "global" | "nonlocal") {
            let line = token.line;
            let global = token.text == "global";
            i += 1;
            while i < t.len() && t[i].line == line && (word(&t[i]) || tok(t, i) == ",") {
                if word(&t[i]) {
                    e.scopes[scope]
                        .directives
                        .insert(t[i].text.to_owned(), global);
                    add_use(
                        &mut a,
                        &mut e,
                        &t[i],
                        scope,
                        false,
                        Lookup::Name {
                            scope,
                            receiver: None,
                        },
                    );
                }
                i += 1;
            }
            continue;
        }
        if python && prev == "as" && word(token) {
            if !e.scopes[scope].bindings.contains_key(token.text) {
                bind_token(token, scope, "local_binding", &mut a, &mut e);
            }
            i += 1;
            continue;
        }
        if python
            && next == "="
            && tok(t, i + 2) != "="
            && prev == "."
            && i >= 2
            && let Some(Binding::Local(id)) = e.binding(scope, tok(t, i - 2))
            && let Some(&shape) = e.self_params.get(id)
        {
            let key = (shape, token.text.to_owned());
            if !e.members.contains_key(&key) {
                e.members.insert(key, a.declarations.len());
                a.declarations.push(Declaration {
                    span: token.span.clone(),
                    kind: "local_binding",
                });
                i += 1;
                continue;
            }
        }
        if python && (i == 0 || t[i - 1].line != token.line) && next == "," {
            let mut j = i;
            let mut vars = Vec::new();
            while t.get(j).is_some_and(word) {
                vars.push(j);
                j += 1;
                if tok(t, j) != "," {
                    break;
                }
                j += 1;
            }
            if tok(t, j) == "=" {
                for j in vars {
                    if !e.scopes[scope].bindings.contains_key(tok(t, j)) {
                        bind_token(&t[j], scope, "local_binding", &mut a, &mut e);
                    }
                }
                i = j + 1;
                continue;
            }
        }
        if python && next == "=" && tok(t, i + 2) != "=" && matches!(prev, "," | "(") {
            let mut j = i;
            while j > 0 {
                j -= 1;
                if tok(t, j) == "(" {
                    if j > 0 && word(&t[j - 1]) && (j < 2 || tok(t, j - 2) != ".") {
                        add_use(
                            &mut a,
                            &mut e,
                            token,
                            scope,
                            false,
                            Lookup::Argument {
                                scope,
                                callee: t[j - 1].span.clone(),
                            },
                        );
                    }
                    break;
                }
                if matches!(tok(t, j), ")" | "]" | "}") && end[j] < j {
                    j = end[j];
                }
                if matches!(tok(t, j), ";") {
                    break;
                }
            }
            i += 1;
            continue;
        }
        if python && token.text == "for" {
            let mut j = i + 1;
            let mut names = Vec::new();
            while j < t.len()
                && tok(t, j) != "in"
                && (word(&t[j]) || matches!(tok(t, j), "(" | ")" | "," | "*"))
            {
                if word(&t[j]) {
                    names.push(j);
                }
                j += 1;
            }
            if tok(t, j) == "in" {
                for j in names {
                    if !e.scopes[scope].bindings.contains_key(tok(t, j)) {
                        bind_token(&t[j], scope, "local_binding", &mut a, &mut e);
                    }
                }
                i = j + 1;
                continue;
            }
        }
        if keyword(lang, token.text) {
            i += 1;
            continue;
        }
        if python
            && next == "="
            && tok(t, i + 2) != "="
            && prev != "."
            && (i == 0 || t[i - 1].line != token.line || prev == ";")
        {
            if matches!(
                e.scopes[scope].bindings.get(token.text),
                Some(Binding::Import(_) | Binding::Package)
            ) {
                e.scopes[scope]
                    .bindings
                    .insert(token.text.to_owned(), Binding::Conflicting);
            }
            if !e.scopes[scope].bindings.contains_key(token.text)
                && !e.scopes[scope].directives.contains_key(token.text)
            {
                let id = a.declarations.len();
                a.declarations.push(Declaration {
                    span: token.span.clone(),
                    kind: "local_binding",
                });
                e.declare(scope, token.text, Binding::Local(id));
                if e.scopes[scope].shape {
                    e.members.insert((scope, token.text.to_owned()), id);
                }
                if t.get(i + 2).is_some_and(word) && tok(t, i + 3) == "(" {
                    e.types.insert(id, (scope, t[i + 2].span.clone()));
                }
                if scope == 0 {
                    e.exports.insert(token.text.into(), id);
                }
            } else {
                add_use(
                    &mut a,
                    &mut e,
                    token,
                    scope,
                    false,
                    Lookup::Name {
                        scope,
                        receiver: None,
                    },
                );
            }
            i += 1;
            continue;
        }
        if !python && prev == "new" && args(t, i + 1).is_some() {
            add_use(
                &mut a,
                &mut e,
                token,
                scope,
                true,
                Lookup::Construct { scope },
            );
            i += 1;
            continue;
        }
        if prev == "." && i >= 2 {
            let base = i - 2;
            let key = if tok(t, base) == ")" && end[base] > 0 && end[base] < base {
                Some(end[base] - 1)
            } else if base > 0 && tok(t, base - 1) == "." {
                Some(base)
            } else {
                None
            };
            if let Some(prior) = key.and_then(|j| written_uses.get(&j).copied()) {
                let row = a.calls.len();
                add_use(
                    &mut a,
                    &mut e,
                    token,
                    scope,
                    next == "(",
                    Lookup::Chained {
                        prior,
                        scope,
                        qualification: if python && word(&t[base]) {
                            let mut k = base;
                            while k >= 2 && tok(t, k - 1) == "." && word(&t[k - 2]) {
                                k -= 2;
                            }
                            Some(t[k].span.start..t[base].span.end)
                        } else {
                            None
                        },
                    },
                );
                written_uses.insert(i, row);
                i += 1;
                continue;
            }
        }
        // Qualified names are resolved only for a known module import. Never
        // project a dynamic object's member onto an unrelated declaration.
        let receiver = if prev == "." {
            i.checked_sub(2)
                .filter(|&j| word(&t[j]) && (j == 0 || tok(t, j - 1) != "."))
                .map(|j| t[j].span.clone())
        } else {
            None
        };
        if prev == "#" && i >= 2 && tok(t, i - 2) == "." {
            i += 1;
            continue;
        }
        if (prev != "." || receiver.is_some()) && !(next == ":" && !python) {
            if next == "(" || next == "." || prev == "." {
                written_uses.insert(i, a.calls.len());
            }
            add_use(
                &mut a,
                &mut e,
                token,
                scope,
                next == "(",
                Lookup::Name { scope, receiver },
            );
        }
        i += 1;
    }
    for owner in mixed_returns {
        e.result_shapes.remove(&owner);
    }
    for ((scope, name), mut ids) in signatures {
        let base = match e.scopes[scope].bindings.get(&name) {
            Some(Binding::Local(id)) => *id,
            None => {
                let id = ids.remove(0);
                e.declare(scope, &name, Binding::Local(id));
                id
            }
            _ => continue,
        };
        e.families.insert(base, ids);
    }
    a.script = Some(e);
    a
}

#[cfg(test)]
mod tests;

crate::domain::code::cache::record!(Evidence { scopes, imports, uses, exports, forward, stars, packages, wildcards, all, dynamic_all, families, shapes, result_shapes, annotated_returns, annotated_shapes, result_types, element_types, iterated, props, members, types, functions, destructured_parameters, self_params });

crate::domain::code::cache::record!(Scope { shape, receiver, this_boundary, parent, owner, bindings, directives });

crate::domain::code::cache::record!(Import { module, name });

impl crate::domain::code::cache::Wire for Binding{fn save(&self,w:&mut Vec<u8>){match self{Self::Local(a)=>{ w.push(0); a.save(w);},Self::Import(a)=>{ w.push(1); a.save(w);},Self::Package=>{ w.push(2); },Self::Conflicting=>{ w.push(3); }}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Local(Wire::load(r)?)),1=>Ok(Self::Import(Wire::load(r)?)),2=>Ok(Self::Package),3=>Ok(Self::Conflicting),_=>Err(crate::domain::code::cache::bad())}}}

impl crate::domain::code::cache::Wire for Forward{fn save(&self,w:&mut Vec<u8>){match self{Self::Namespace(a)=>{ w.push(0); a.save(w);},Self::Local(a)=>{ w.push(1); a.save(w);},Self::Import(a)=>{ w.push(2); a.save(w);}}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Namespace(Wire::load(r)?)),1=>Ok(Self::Local(Wire::load(r)?)),2=>Ok(Self::Import(Wire::load(r)?)),_=>Err(crate::domain::code::cache::bad())}}}

impl crate::domain::code::cache::Wire for Lookup{fn save(&self,w:&mut Vec<u8>){match self{Self::Name {scope, receiver}=>{ w.push(0); scope.save(w);receiver.save(w);},Self::Argument {scope, callee}=>{ w.push(1); scope.save(w);callee.save(w);},Self::Known(a)=>{ w.push(2); a.save(w);},Self::Chained {prior, scope, qualification}=>{ w.push(3); prior.save(w);scope.save(w);qualification.save(w);},Self::Construct {scope}=>{ w.push(4); scope.save(w);},Self::Prop {scope, component}=>{ w.push(5); scope.save(w);component.save(w);},Self::Imported(a)=>{ w.push(6); a.save(w);}}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Name {scope:Wire::load(r)?, receiver:Wire::load(r)?}),1=>Ok(Self::Argument {scope:Wire::load(r)?, callee:Wire::load(r)?}),2=>Ok(Self::Known(Wire::load(r)?)),3=>Ok(Self::Chained {prior:Wire::load(r)?, scope:Wire::load(r)?, qualification:Wire::load(r)?}),4=>Ok(Self::Construct {scope:Wire::load(r)?}),5=>Ok(Self::Prop {scope:Wire::load(r)?, component:Wire::load(r)?}),6=>Ok(Self::Imported(Wire::load(r)?)),_=>Err(crate::domain::code::cache::bad())}}}

/// Source bytes read by the script resolver and public-label writer after parsing.
/// Call only when both files have the same serialized parser evidence.
pub(super) fn same_binding_text(analysis: &Analysis, lang: Language, old: &str, new: &str) -> bool {
    let Some(e) = analysis.script.as_ref() else { return false };
    let same = |r: &Range<usize>| old.get(r.clone()).is_some_and(|a| Some(a) == new.get(r.clone()));
    if !analysis.declarations.iter().all(|d| same(&d.span)
        && (lang != Language::Python || super::indentation(old, d.span.start) == super::indentation(new, d.span.start)))
        || !analysis.calls.iter().all(|c| same(&c.span)) {
        return false;
    }
    e.uses.iter().all(|u| match u {
        Lookup::Name { receiver, .. } => receiver.as_ref().is_none_or(same),
        Lookup::Chained { qualification, .. } => qualification.as_ref().is_none_or(same),
        Lookup::Argument { callee, .. } => same(callee),
        Lookup::Prop { component, .. } => same(component),
        Lookup::Construct { .. } | Lookup::Known(_) | Lookup::Imported(_) => true,
    }) && [&e.types, &e.iterated, &e.element_types, &e.result_types, &e.annotated_returns]
        .iter().all(|table| table.values().all(|(_, r)| same(r)))
}
