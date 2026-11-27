use super::{Analysis, Token, before, call, declare, word};
use std::collections::HashMap;

#[derive(Default)]
struct Scope<'a> {
    owner: Option<usize>,
    bindings: HashMap<&'a str, Option<usize>>,
}

impl<'a> Scope<'a> {
    fn open(stack: &mut Vec<Self>, owner: Option<usize>, names: Vec<&'a str>) {
        stack.push(Self {
            owner,
            bindings: names.into_iter().map(|name| (name, None)).collect(),
        });
    }
    fn close(stack: &mut Vec<Self>) {
        if stack.len() > 1 {
            stack.pop();
        }
    }
}

struct Expressions<'a, 's> {
    tokens: &'a [Token<'s>],
    ends: HashMap<usize, usize>,
}

impl Expressions<'_, '_> {
    // A lookup for an outer group also discovers the end of every nested group
    // of its grammar. Block and punctuation openers are disjoint.
    fn group_end(&mut self, start: usize, opens: &[&str], closes: &[&str]) -> usize {
        if let Some(&end) = self.ends.get(&start) {
            return end;
        }
        let mut pending = Vec::new();
        for (i, token) in self.tokens.iter().enumerate().skip(start + 1) {
            match GroupBoundary::read(token.text, opens, closes) {
                GroupBoundary::Open => pending.push(i),
                GroupBoundary::Close => {
                    let Some(open) = pending.pop() else {
                        return i + 1;
                    };
                    self.ends.insert(open, i + 1);
                }
                GroupBoundary::Other => {}
            }
        }
        for open in pending {
            self.ends.insert(open, self.tokens.len());
        }
        self.tokens.len()
    }

    // A local's scope begins after its initializer. The repeat block, conversely,
    // remains visible throughout its until-expression. Find that boundary without
    // consuming the tokens the main loop still needs for calls and declarations.
    fn expression_end(&mut self, mut at: usize) -> usize {
        loop {
            match self.expression_item(at) {
                ExpressionStep::Next(next) => at = next,
                ExpressionStep::End(end) => return end,
            }
        }
    }

    fn expression_item(&mut self, at: usize) -> ExpressionStep {
        let primary = after_prefix_operators(self.tokens, at);
        let Some(end) = self.operand_end(primary) else {
            return ExpressionStep::End(primary);
        };
        operator_end(self.tokens, end).map_or(ExpressionStep::End(end), ExpressionStep::Next)
    }

    // Parenthesized, table and string arguments may follow a completed expression.
    fn argument_end(&mut self, i: usize) -> Option<usize> {
        let t = self.tokens;
        match t.get(i)?.text {
            "(" | "{" | "[" => Some(self.group_end(i, &["(", "{", "["], &[")", "}", "]"])),
            quoted if quoted.starts_with(['\'', '"']) => Some(i + 1),
            _ => None,
        }
    }

    fn operand_end(&mut self, i: usize) -> Option<usize> {
        let mut next = self.primary_end(i)?;
        while let Some(end) = self.argument_end(next) {
            next = end;
        }
        Some(next)
    }

    fn primary_end(&mut self, i: usize) -> Option<usize> {
        let t = self.tokens;
        if let Some(end) = self.argument_end(i) {
            return Some(end);
        }
        match t.get(i)?.text {
            "end" | "until" | "else" | ";" | ")" => None,
            "function" => Some(self.group_end(i, &["function", "if", "do"], &["end"])),
            number if number.starts_with(|c: char| c.is_ascii_digit()) => Some(number_end(t, i)),
            _ => Some(i + 1),
        }
    }
}

enum ExpressionStep {
    Next(usize),
    End(usize),
}

enum GroupBoundary {
    Open,
    Close,
    Other,
}

impl GroupBoundary {
    fn read(word: &str, opens: &[&str], closes: &[&str]) -> Self {
        if opens.contains(&word) {
            Self::Open
        } else if closes.contains(&word) {
            Self::Close
        } else {
            Self::Other
        }
    }
}

fn after_prefix_operators(t: &[Token], start: usize) -> usize {
    start
        + t[start.min(t.len())..]
            .iter()
            .take_while(|token| matches!(token.text, "not" | "-" | "#" | "~"))
            .count()
}

fn number_end(t: &[Token], at: usize) -> usize {
    let rest = &t[at..];
    at + 1
        + rest
            .windows(2)
            .take_while(|pair| {
                pair[1].at == pair[0].span().end
                    && pair[1]
                        .text
                        .starts_with(|c: char| c.is_ascii_alphanumeric())
            })
            .count()
}

fn operator_end(t: &[Token], mut p: usize) -> Option<usize> {
    // Adjacent punctuation may form a compound binary operator.
    let op = t.get(p)?.text;
    if !matches!(
        op,
        "," | "and"
            | "or"
            | ":"
            | "."
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "^"
            | "&"
            | "|"
            | "~"
            | "<"
            | ">"
            | "="
    ) {
        return None;
    }
    p += 1;
    while p < t.len()
        && t[p].at == t[p - 1].span().end
        && matches!(
            (op, t[p].text),
            (".", ".") | ("=", "=") | ("<", "=" | "<") | (">", "=" | ">") | ("~", "=") | ("/", "/")
        )
    {
        p += 1;
    }
    Some(p)
}

/// Assigned anonymous functions and ordinary dotted method declarations place
/// their public name on opposite sides of the function keyword.
fn function_name(t: &[Token], i: usize) -> Option<(usize, usize, bool)> {
    if let Some(name) = assigned_name(t, i) {
        return Some((name, i, false));
    }
    let mut name = i + 1;
    if !t.get(name).is_some_and(|token| token.name()) {
        return None;
    }
    while matches!(word(t, name + 1), "." | ":")
        && t.get(name + 2).is_some_and(|token| token.name())
    {
        name += 2;
    }
    let local = name == i + 1 && before(t, i) == "local";
    (word(t, name + 1) == "(").then_some((name, name, local))
}

fn assigned_name(t: &[Token], keyword: usize) -> Option<usize> {
    let name = keyword.checked_sub(2)?;
    (before(t, keyword) == "=" && t[name].name()).then_some(name)
}

fn function_parameters<'a>(tokens: &[Token<'a>], after: usize, body: &mut Scope<'a>) -> usize {
    if word(tokens, after + 1) != "(" {
        return after;
    }
    let start = after + 2;
    let parameters = tokens[start..].iter().take_while(|token| token.text != ")");
    let mut end = start - 1;
    for token in parameters {
        end += 1;
        if token.name() {
            body.bindings.insert(token.text, None);
        }
    }
    end
}

fn function_scope<'a>(
    t: &[Token<'a>],
    mut i: usize,
    parent: &mut Scope<'a>,
    out: &mut Analysis,
) -> (usize, Scope<'a>) {
    let mut body = Scope::default();
    if let Some((name, after, local)) = function_name(t, i) {
        let id = declare(out, t[name].span(), "function_definition");
        body.owner = Some(id);
        if local {
            parent.bindings.insert(t[name].text, Some(id));
        }
        i = after;
    }
    i = function_parameters(t, i, &mut body);
    (i, body)
}

fn attribute_end(t: &[Token], at: usize) -> usize {
    if word(t, at) == "<" && word(t, at + 2) == ">" {
        at + 3
    } else {
        at
    }
}

fn shadowed_names<'a>(
    t: &[Token<'a>],
    mut p: usize,
    scopes: &[Scope<'a>],
) -> (Vec<&'a str>, usize) {
    let mut names = Vec::new();
    while let Some(token) = t.get(p).filter(|token| token.name()) {
        let candidate = shadowed_name(token.text, scopes);
        names.extend(candidate);
        let end = attribute_end(t, p + 1);
        if word(t, end) != "," {
            return (names, end);
        }
        p = end + 1;
    }
    (names, p)
}

fn shadowed_name<'a>(name: &'a str, scopes: &[Scope<'a>]) -> Option<&'a str> {
    let target = scopes
        .iter()
        .rev()
        .find_map(|scope| scope.bindings.get(name))?;
    target.map(|_| name)
}

enum Change<'a> {
    Hide(usize, Vec<&'a str>),
    EndRepeat,
}

impl<'a> Change<'a> {
    fn apply(self, scopes: &mut Vec<Scope<'a>>) {
        match self {
            Self::Hide(depth, names) => {
                if let Some(scope) = scopes.get_mut(depth) {
                    scope.bindings.extend(names.into_iter().map(|n| (n, None)));
                }
            }
            Self::EndRepeat => Scope::close(scopes),
        }
    }
}

#[derive(Default)]
struct AtToken<'a> {
    before: Vec<Change<'a>>,
    loop_names: Vec<&'a str>,
}

fn locals<'a>(
    expr: &mut Expressions<'_, 'a>,
    i: usize,
    scopes: &[Scope<'a>],
    changes: &mut HashMap<usize, AtToken<'a>>,
) -> usize {
    let t = expr.tokens;
    let (names, p) = shadowed_names(t, i + 1, scopes);
    if names.is_empty() {
        return p;
    }
    let operator = word(t, p);
    match t[i].text {
        "local" => schedule_local_names(expr, p, (scopes.len() - 1, names), changes),
        _ if matches!(operator, "=" | "in") => schedule_for_names(expr, p, names, changes),
        _ => {}
    }
    p
}

fn schedule_local_names<'a>(
    expr: &mut Expressions<'_, 'a>,
    at: usize,
    (scope, names): (usize, Vec<&'a str>),
    changes: &mut HashMap<usize, AtToken<'a>>,
) {
    let stop = if word(expr.tokens, at) == "=" {
        expr.expression_end(at + 1)
    } else {
        at
    };
    changes
        .entry(stop)
        .or_default()
        .before
        .push(Change::Hide(scope, names));
}

fn schedule_for_names<'a>(
    expr: &mut Expressions<'_, 'a>,
    operator: usize,
    names: Vec<&'a str>,
    changes: &mut HashMap<usize, AtToken<'a>>,
) {
    let stop = expr.expression_end(operator + 1);
    if word(expr.tokens, stop) == "do" {
        changes.entry(stop).or_default().loop_names = names;
    }
}

fn name_use<'a>(t: &[Token<'a>], i: usize, scopes: &mut [Scope<'a>], out: &mut Analysis) {
    let next = word(t, i + 1);
    if !matches!(next, "(" | "=") {
        return;
    }
    let tok = &t[i];
    let owner = scopes.last().unwrap().owner;
    let Some(binding) = scopes
        .iter_mut()
        .rev()
        .find_map(|s| s.bindings.get_mut(tok.text))
    else {
        return;
    };
    match (next, *binding) {
        ("(", Some(id)) => call(out, tok.span(), owner, id),
        ("=", _) => *binding = None,
        _ => {}
    }
}

enum Statement {
    Function,
    Locals,
    Open,
    Branch { otherwise: bool },
    Until,
    End,
    Name,
    Other,
}

impl Statement {
    fn read(tokens: &[Token], at: usize) -> Self {
        match tokens[at].text {
            "function" => Self::Function,
            "do" | "then" | "repeat" => Self::Open,
            "until" => Self::Until,
            "end" => Self::End,
            _ => Self::name_or_header(tokens, at),
        }
    }

    fn name_or_header(tokens: &[Token], at: usize) -> Self {
        let token = tokens[at];
        match token.text {
            "for" | "local" if word(tokens, at + 1) != "function" => Self::Locals,
            "else" | "elseif" => Self::Branch {
                otherwise: token.text == "else",
            },
            _ if token.name() && !matches!(before(tokens, at), "." | ":") => Self::Name,
            _ => Self::Other,
        }
    }
}

struct Bindings<'t, 'a> {
    out: Analysis,
    scopes: Vec<Scope<'a>>,
    expr: Expressions<'t, 'a>,
    changes: HashMap<usize, AtToken<'a>>,
}

impl<'a> Bindings<'_, 'a> {
    fn advance(&mut self, at: usize) -> usize {
        let scheduled = self.changes.remove(&at).unwrap_or_default();
        self.apply_changes(scheduled.before);
        let t = self.expr.tokens;
        let statement = Statement::read(t, at);
        match statement {
            Statement::Function | Statement::Locals => return self.declaration(statement, at),
            Statement::Open => {
                let owner = self.scopes.last().unwrap().owner;
                Scope::open(&mut self.scopes, owner, scheduled.loop_names);
            }
            Statement::Branch { otherwise } => self.change_branch(otherwise),
            Statement::Until => self.schedule_repeat_end(at),
            Statement::End => Scope::close(&mut self.scopes),
            Statement::Name => name_use(t, at, &mut self.scopes, &mut self.out),
            Statement::Other => {}
        }
        at + 1
    }

    fn apply_changes(&mut self, changes: Vec<Change<'a>>) {
        for change in changes {
            change.apply(&mut self.scopes);
        }
    }

    fn declaration(&mut self, statement: Statement, at: usize) -> usize {
        if matches!(statement, Statement::Locals) {
            return locals(&mut self.expr, at, &self.scopes, &mut self.changes);
        }
        let (last, body) = function_scope(
            self.expr.tokens,
            at,
            self.scopes.last_mut().unwrap(),
            &mut self.out,
        );
        self.scopes.push(body);
        last + 1
    }

    fn change_branch(&mut self, otherwise: bool) {
        let owner = self.scopes.last().unwrap().owner;
        Scope::close(&mut self.scopes);
        if otherwise {
            Scope::open(&mut self.scopes, owner, Vec::new());
        }
    }

    fn schedule_repeat_end(&mut self, at: usize) {
        let stop = self.expr.expression_end(at + 1);
        self.changes
            .entry(stop)
            .or_default()
            .before
            .push(Change::EndRepeat);
    }
}

pub(super) fn analyze(t: &[Token<'_>]) -> Analysis {
    let mut bindings = Bindings {
        out: Analysis::default(),
        scopes: vec![Scope::default()],
        expr: Expressions {
            tokens: t,
            ends: HashMap::new(),
        },
        changes: HashMap::new(),
    };
    let mut at = 0;
    while at < t.len() {
        at = bindings.advance(at);
    }
    bindings.out
}
