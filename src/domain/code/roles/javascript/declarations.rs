//! Collect lexical bindings that can shadow or alias a test registration.
use super::*;

fn scope(s: &Syntax, open: Option<usize>) -> Range<usize> {
    open.map_or(0..s.tokens.len(), |i| {
        i + 1..s.close(i).unwrap_or(s.tokens.len())
    })
}

fn required_runner(s: &Syntax, equals: usize) -> Runner {
    let start = equals + 1;
    (s.w(equals) == "=" && s.seq(start, &["require", "("]) && s.w(start + 3) == ")")
        .then(|| literal(s, start + 2))
        .map_or(Runner::Unknown, Runner::from_module)
}

fn bind_properties<'a>(
    s: &'a Syntax,
    properties: Range<usize>,
    area: &Range<usize>,
    bindings: &mut Scopes<'a>,
) {
    let runner = required_runner(s, properties.end + 1);
    for property in arguments(s, properties.start, properties.end) {
        let p = property.start;
        if ident(s, p) {
            let alias = parameter_alias(s, p).unwrap_or(s.w(p));
            bindings.bind(area.clone(), alias, runner.export(s.w(p)));
        }
    }
}

fn variables<'a>(s: &'a Syntax, i: usize, area: Range<usize>, bindings: &mut Scopes<'a>) {
    let mut at = i + 1;
    while let Some(end) = bind_variable(s, at, &area, bindings) {
        let Some(next) = next_declarator(s, end) else {
            break;
        };
        at = next;
    }
}

fn bind_variable<'a>(
    s: &'a Syntax,
    at: usize,
    area: &Range<usize>,
    bindings: &mut Scopes<'a>,
) -> Option<usize> {
    if s.w(at) == "{" {
        let end = s.close(at)?;
        bind_properties(s, at + 1..end, area, bindings);
        return Some(end + 1);
    }
    if !ident(s, at) {
        return None;
    }
    let runner = required_runner(s, at + 1);
    let binding = match s.w(at + 6) {
        "." => runner.export(s.w(at + 7)),
        _ => runner.require(),
    };
    bindings.bind(area.clone(), s.w(at), binding);
    Some(at + 1)
}

// Walk an initializer to the next declarator, stopping at automatic semicolon insertion.
fn next_declarator(s: &Syntax, mut at: usize) -> Option<usize> {
    while at < s.tokens.len()
        && !matches!(s.w(at), "," | ";" | ")" | "}")
        && s.tokens[at].line == s.tokens[at.saturating_sub(1)].line
    {
        at = s.close(at).unwrap_or(at) + 1;
    }
    (s.w(at) == ",").then_some(at + 1)
}

pub(super) fn declarations<'a>(s: &'a Syntax, bindings: &mut Scopes<'a>) {
    let mut state = DeclarationScan::default();
    let mut i = 0;
    while i < s.tokens.len() {
        i = state.observe(s, bindings, i) + 1;
    }
}

#[derive(Default)]
struct DeclarationScan<'s> {
    braces: Vec<usize>,
    imports: Vec<(&'s str, &'s str)>,
    expressions: ExpressionBodies,
}

impl<'a> DeclarationScan<'a> {
    fn observe(&mut self, s: &'a Syntax, bindings: &mut Scopes<'a>, i: usize) -> usize {
        match s.w(i) {
            "{" => self.braces.push(i),
            "}" => {
                self.braces.pop();
            }
            "import" => return self.import(s, i, bindings),
            "function" | "class" | "let" | "const" | "var" => self.bind_declaration(s, i, bindings),
            "(" => self.bind_parenthesized(s, i, bindings),
            "=" => self.bind_arrow(s, i, bindings),
            _ => {}
        }
        i
    }

    fn bind_declaration(&self, s: &'a Syntax, i: usize, bindings: &mut Scopes<'a>) {
        let scope = scope(s, self.braces.last().copied());
        if matches!(s.w(i), "function" | "class") {
            if ident(s, i + 1) {
                bindings.bind(scope, s.w(i + 1), Binding::Other);
            }
        } else {
            variables(s, i, scope, bindings);
        }
    }

    fn import(&mut self, s: &'a Syntax, i: usize, bindings: &mut Scopes<'a>) -> usize {
        let Some(clause) = imports::Clause::parse(s, i + 1, &mut self.imports) else {
            return i;
        };
        let from = clause.from;
        bindings.import(clause);
        from
    }

    fn bind_parenthesized(&mut self, s: &'a Syntax, i: usize, bindings: &mut Scopes<'a>) {
        if let Some((parameters, body)) = parameter_scope(s, i, &mut self.expressions) {
            bind_parameters(s, parameters, body, bindings);
        }
    }

    fn bind_arrow(&mut self, s: &'a Syntax, i: usize, bindings: &mut Scopes<'a>) {
        let Some(name) = i.checked_sub(1).filter(|&name| ident(s, name)) else {
            return;
        };
        if s.w(i + 1) == ">" {
            bindings.bind(
                body_scope(s, i + 2, &mut self.expressions),
                s.w(name),
                Binding::Other,
            );
        }
    }
}

fn body_scope(s: &Syntax, body: usize, expressions: &mut ExpressionBodies) -> Range<usize> {
    let end = if s.w(body) == "{" {
        s.close(body).unwrap_or(s.tokens.len())
    } else {
        registration_expression_end(expressions, s, body)
    };
    body..end
}

fn parameter_scope(
    s: &Syntax,
    open: usize,
    expressions: &mut ExpressionBodies,
) -> Option<(Range<usize>, Range<usize>)> {
    let end = s.close(open)?;
    if end == open + 1 {
        return None;
    }
    let next = after_return_type(s, end + 1);
    let body = if s.seq(next, &["=", ">"]) {
        next + 2
    } else {
        let previous = s.w(open.wrapping_sub(1));
        if s.w(next) != "{" || matches!(previous, "if" | "switch" | "while" | "for" | "with") {
            return None;
        }
        next
    };
    Some((open + 1..end, body_scope(s, body, expressions)))
}

fn after_return_type(s: &Syntax, mut next: usize) -> usize {
    if s.w(next) == ":" {
        next += 1;
        while next < s.tokens.len() && !matches!(s.w(next), "{" | "=" | ";" | ")" | "}" | ",") {
            next = s.close(next).unwrap_or(next) + 1;
        }
    }
    next
}

fn parameter_alias<'a>(s: &'a Syntax, key: usize) -> Option<&'a str> {
    let alias = if s.w(key + 1) == ":" { key + 2 } else { key };
    ident(s, alias).then(|| s.w(alias))
}

pub(super) fn parameter_name<'a>(s: &'a Syntax, parameter: &Range<usize>) -> Option<&'a str> {
    let start = parameter.start
        + if s.seq(parameter.start, &[".", ".", "."]) {
            3
        } else {
            0
        };
    (start < parameter.end && ident(s, start)).then(|| s.w(start))
}

fn bind_parameters<'a>(
    s: &'a Syntax,
    parameters: Range<usize>,
    body: Range<usize>,
    bindings: &mut Scopes<'a>,
) {
    for parameter in arguments(s, parameters.start, parameters.end) {
        bind_parameter(s, parameter, &body, bindings);
    }
}

fn bind_parameter<'a>(
    s: &'a Syntax,
    parameter: Range<usize>,
    body: &Range<usize>,
    bindings: &mut Scopes<'a>,
) {
    if let Some(name) = parameter_name(s, &parameter) {
        bindings.bind(body.clone(), name, Binding::Other);
        return;
    }
    let open = parameter.start;
    let Some(close) = s.close(open).filter(|_| s.w(open) == "{") else {
        return;
    };
    let aliases =
        arguments(s, open + 1, close).filter_map(|property| parameter_alias(s, property.start));
    for alias in aliases {
        bindings.bind(body.clone(), alias, Binding::Other);
    }
}

/// Expression-bodied arrows on the same unparenthesized chain end together.
/// A balanced group is skipped, so calls and callbacks inside it keep their
/// own boundaries. Ordinary expressions never allocate a cache entry.
pub(super) fn registration_expression_end(
    ends: &mut ExpressionBodies,
    s: &Syntax,
    body: usize,
) -> usize {
    ends.end(s.tokens, s.pairs, body, |word| {
        matches!(
            word,
            "const" | "let" | "var" | "function" | "class" | "export" | "import"
        )
    })
}
