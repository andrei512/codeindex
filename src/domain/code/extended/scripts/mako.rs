use super::{
    Analysis, call, declare,
    lex::{identifier, quoted},
};
use crate::domain::code::record_unique;

fn attribute<'a>(s: &'a str, cursor: &mut usize) -> Option<(usize, &'a str)> {
    let rest = s[*cursor..].trim_start();
    *cursor = s.len() - rest.len();
    let value = rest.strip_prefix('=')?.trim_start();
    let start = s.len() - value.len();
    *cursor = start;
    let quote = *value
        .as_bytes()
        .first()
        .filter(|q| matches!(q, b'\'' | b'"'))?;
    let width = quoted(value, quote);
    *cursor += width;
    if width < 2 || value.as_bytes()[width - 1] != quote {
        return None;
    }
    Some((start + 1, &value[1..width - 1]))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeAction {
    Definition,
    Nested,
    Page,
    Inert,
}

#[derive(Copy, Clone)]
struct Element {
    declaration: Option<&'static str>,
    scope: ScopeAction,
    close: Option<&'static str>,
}
impl Element {
    fn read(name: &str) -> Self {
        use ScopeAction::*;
        const RULES: &[(&str, Element)] = &[
            (
                "def",
                Element {
                    declaration: Some("function_definition"),
                    scope: Definition,
                    close: None,
                },
            ),
            (
                "block",
                Element {
                    declaration: Some("block_declaration"),
                    scope: Nested,
                    close: None,
                },
            ),
            (
                "namespace",
                Element {
                    declaration: Some("module_declaration"),
                    scope: Nested,
                    close: None,
                },
            ),
            (
                "call",
                Element {
                    declaration: None,
                    scope: Nested,
                    close: None,
                },
            ),
            (
                "page",
                Element {
                    declaration: None,
                    scope: Page,
                    close: None,
                },
            ),
            (
                "doc",
                Element {
                    declaration: None,
                    scope: Inert,
                    close: Some("</%doc>"),
                },
            ),
            (
                "text",
                Element {
                    declaration: None,
                    scope: Inert,
                    close: Some("</%text>"),
                },
            ),
        ];
        RULES
            .iter()
            .find_map(|&(tag, element)| (tag == name).then_some(element))
            .unwrap_or(Element {
                declaration: None,
                scope: Inert,
                close: None,
            })
    }
}
struct Tag<'a> {
    scope: ScopeAction,
    consumed: usize,
    declaration: Option<(std::ops::Range<usize>, &'a str, &'static str)>,
    self_closing: bool,
    args: Option<&'a str>,
}

impl<'a> Tag<'a> {
    fn observe(&mut self, kind: Option<&'static str>, key: &str, at: usize, value: &'a str) {
        match (key, kind) {
            ("args", _) => self.args = Some(value),
            ("name", Some(kind)) => self.name(kind, at, value),
            _ => {}
        }
    }
    fn name(&mut self, kind: &'static str, at: usize, value: &'a str) {
        let len = identifier(value);
        if len == 0 {
            return;
        }
        let tail = &value[len..];
        if kind == "function_definition" {
            let Some(parameters) = tail.trim().strip_prefix('(') else {
                return;
            };
            self.args = parameters.strip_suffix(')');
        } else if !tail.is_empty() {
            return;
        }
        self.declaration = Some((at..at + len, &value[..len], kind));
    }
}

struct Attributes<'a> {
    source: &'a str,
    at: usize,
}

impl<'a> Iterator for Attributes<'a> {
    type Item = Result<(&'a str, Option<(usize, &'a str)>), ()>;
    fn next(&mut self) -> Option<Self::Item> {
        let tail =
            self.source[self.at..].trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        self.at = self.source.len() - tail.len();
        let c = tail.chars().next()?;
        if c == '>' {
            self.at += 1;
            return None;
        }
        let len = identifier(tail);
        if len == 0 {
            self.at += c.len_utf8();
            return Some(Err(()));
        }
        self.at += len;
        Some(Ok((&tail[..len], attribute(self.source, &mut self.at))))
    }
}

fn tag<'a>(source: &'a str, start: usize, tag: &str) -> Tag<'a> {
    let s = &source[start..];
    let mut attrs = Attributes {
        source: s,
        at: 2 + tag.len(),
    };
    let element = Element::read(tag);
    let mut result = Tag {
        scope: element.scope,
        self_closing: false,
        consumed: 0,
        declaration: None,
        args: None,
    };
    for attr in attrs.by_ref() {
        match attr {
            Ok((key, Some((at, value)))) => {
                result.observe(element.declaration, key, start + at, value)
            }
            Ok((_, None)) => {}
            Err(()) => {
                result.declaration = None;
                result.consumed = attrs.at;
                return result;
            }
        }
    }
    result.self_closing = s[..attrs.at].ends_with("/>");
    result.consumed = element.close.map_or(attrs.at, |close| {
        s[attrs.at..]
            .find(close)
            .map_or(s.len(), |n| attrs.at + n + close.len())
    });
    result
}

#[derive(Default)]
struct Scope<'a> {
    tag: &'a str,
    parent: Option<usize>,
    owner: Option<usize>,
    bindings: std::collections::HashMap<&'a str, Option<usize>>,
}

enum PythonToken<'a> {
    Name(&'a str),
    String,
    Assign,
    Walrus,
    Comparison,
    Punct(u8),
    Trivia,
}

impl<'a> PythonToken<'a> {
    fn punctuation(text: &str, byte: u8) -> (Self, usize) {
        match text.as_bytes() {
            [b':', b'=', ..] => (Self::Walrus, 2),
            [b'=' | b'!' | b'<' | b'>', b'=', ..] => (Self::Comparison, 2),
            [b'/', b'/', b'=', ..]
            | [b'*', b'*', b'=', ..]
            | [b'<', b'<', b'=', ..]
            | [b'>', b'>', b'=', ..] => (Self::Assign, 3),
            [
                b'+' | b'-' | b'*' | b'@' | b'/' | b'%' | b'&' | b'|' | b'^',
                b'=',
                ..,
            ] => (Self::Assign, 2),
            [b'=', ..] => (Self::Assign, 1),
            _ => (Self::Punct(byte), text.chars().next().unwrap().len_utf8()),
        }
    }
    fn read(text: &'a str, comments: bool) -> Option<(Self, &'a str)> {
        let byte = *text.as_bytes().first()?;
        if byte.is_ascii_whitespace() {
            return Some((
                Self::Trivia,
                text.trim_start_matches(|c: char| c.is_ascii_whitespace()),
            ));
        }
        let (token, end) = Self::content(text, byte, comments);
        Some((token, &text[end..]))
    }

    fn content(text: &'a str, byte: u8, comments: bool) -> (Self, usize) {
        match byte {
            b'\'' | b'"' => {
                let end =
                    crate::domain::code::lexer::quoted_len(text.as_bytes(), true, false, |_| {});
                (Self::String, end)
            }
            b'#' if comments => (Self::Trivia, text.find('\n').unwrap_or(text.len())),
            _ => Self::word_or_punctuation(text, byte),
        }
    }

    fn word_or_punctuation(text: &'a str, byte: u8) -> (Self, usize) {
        let len = identifier(text);
        if len == 0 {
            return Self::punctuation(text, byte);
        }
        (Self::Name(&text[..len]), len)
    }
}

fn python_tokens(
    mut text: &str,
    comments: bool,
) -> impl Iterator<Item = (PythonToken<'_>, &str, usize)> {
    let mut nesting = 0usize;
    std::iter::from_fn(move || {
        let (token, tail) = PythonToken::read(text, comments)?;
        text = tail;
        let depth = nesting;
        match token {
            PythonToken::Punct(b'(' | b'[' | b'{') => nesting += 1,
            PythonToken::Punct(b')' | b']' | b'}') => nesting = nesting.saturating_sub(1),
            _ => {}
        }
        Some((token, text, depth))
    })
}

fn record_params<'a>(args: &'a str, scope: &mut Scope<'a>) {
    let mut first = true;
    for (token, _, depth) in python_tokens(args, false) {
        match token {
            PythonToken::Name(name) if first => {
                scope.bindings.insert(name, None);
                first = false;
            }
            PythonToken::Punct(b',') if depth == 0 => first = true,
            _ => {}
        }
    }
}

/// A Python assignment or for target may unpack nested lists of names.
/// Writing a member or subscript mutates an object, not its lexical name.
fn bind_targets<'a>(source: &'a str, scope: &mut Scope<'a>) {
    let target = source.trim();
    if target.is_empty() {
        return;
    }
    if identifier(target) == target.len() {
        scope.bindings.insert(target, None);
        return;
    }
    unpack_targets(source, scope);
}

// A member or subscript target excludes its operand names until the next sibling.
fn unpack_targets<'a>(source: &'a str, scope: &mut Scope<'a>) {
    let mut foreign = None;
    for (token, tail, depth) in python_tokens(source, true) {
        match token {
            PythonToken::Punct(b',') => {
                foreign.take_if(|level| depth <= *level);
            }
            _ if foreign.is_some() => {}
            PythonToken::Punct(b':') => break,
            PythonToken::Name(name) => foreign = foreign_target(name, tail, scope).then_some(depth),
            _ => {}
        }
    }
}
fn foreign_target<'a>(name: &'a str, tail: &str, scope: &mut Scope<'a>) -> bool {
    if target_ends(tail) {
        scope.bindings.insert(name, None);
        false
    } else {
        true
    }
}

fn target_ends(tail: &str) -> bool {
    matches!(
        tail.trim_start().as_bytes().first(),
        None | Some(b',' | b')' | b']' | b':')
    )
}

/// Host delimiters are only recognized between Python tokens. Preserve the
/// byte range of each token and return the complete island width to the host.
enum Ending {
    Line,
    Block,
    Expression { braces: usize },
}
impl Ending {
    fn finish_token(&mut self, token: &PythonToken<'_>, tail: &str) -> Option<usize> {
        match (self, token) {
            (Self::Block, PythonToken::Punct(b'%')) if tail.starts_with('>') => Some(1),
            (Self::Expression { braces: 0 }, PythonToken::Punct(b'}')) => Some(0),
            (Self::Expression { braces }, PythonToken::Punct(b'{')) => {
                *braces += 1;
                None
            }
            (Self::Expression { braces }, PythonToken::Punct(b'}')) => {
                *braces -= 1;
                None
            }
            _ => None,
        }
    }
}

fn python_region<'a>(
    code: &'a str,
    mut ending: Ending,
    mut visit: impl FnMut(PythonToken<'a>, &'a str, std::ops::Range<usize>, usize),
) -> usize {
    let mut start = 0;
    for (token, tail, depth) in python_tokens(code, true) {
        let end = code.len() - tail.len();
        if let Some(punctuation) = ending.finish_token(&token, tail) {
            return end + punctuation;
        }
        visit(token, tail, start..end, depth);
        start = end;
    }
    code.len()
}

#[derive(Default)]
struct WriteHead {
    statement: usize,
    for_target: Option<usize>,
    names_next: bool,
}

enum WriteTarget<'a> {
    Name(&'a str),
    Pattern(&'a str),
}
impl<'a> WriteTarget<'a> {
    fn bind(self, scope: &mut Scope<'a>) {
        match self {
            Self::Name(name) => {
                scope.bindings.insert(name, None);
            }
            Self::Pattern(pattern) => bind_targets(pattern, scope),
        }
    }
}

impl WriteHead {
    // Yield only confirmed target text. Ordinary reads, comparisons and
    // expression content never update the lexical binding table.
    fn target<'a>(
        &mut self,
        code: &'a str,
        token: PythonToken<'a>,
        span: std::ops::Range<usize>,
    ) -> Option<WriteTarget<'a>> {
        if matches!(token, PythonToken::Trivia) && !code[span.clone()].contains('\n') {
            return None;
        }
        let target = self.written_target(code, &token, span);
        self.names_next = matches!(token, PythonToken::Name("as" | "def" | "class" | "import"));
        target
    }
    fn written_target<'a>(
        &mut self,
        code: &'a str,
        token: &PythonToken<'a>,
        span: std::ops::Range<usize>,
    ) -> Option<WriteTarget<'a>> {
        match token {
            PythonToken::Trivia | PythonToken::Punct(b';') => {
                self.statement = span.end;
                None
            }
            PythonToken::Name("for") => {
                self.for_target = Some(span.end);
                None
            }
            PythonToken::Name("in") => self
                .for_target
                .take()
                .map(|at| WriteTarget::Pattern(&code[at..span.start])),
            PythonToken::Assign => {
                let at = std::mem::replace(&mut self.statement, span.end);
                Some(WriteTarget::Pattern(&code[at..span.start]))
            }
            PythonToken::Name(name) if self.names_next => Some(WriteTarget::Name(name)),
            _ => None,
        }
    }
}

fn record_writes<'a>(code: &'a str, scope: &mut Scope<'a>, ending: Ending) -> usize {
    let mut head = WriteHead::default();
    python_region(code, ending, |token, _, span, depth| {
        if depth == 0
            && let Some(target) = head.target(code, token, span)
        {
            target.bind(scope);
        }
    })
}

struct Template<'a> {
    out: Analysis,
    scopes: Vec<Scope<'a>>,
    current: usize,
    defs: std::collections::HashMap<&'a str, Option<usize>>,
}
impl<'a> Template<'a> {
    // All lexical writes have been collected before binding starts. Memoize
    // proven outer definitions in the using scope; an explicit None shadows.
    fn lexical(&mut self, scope: usize, name: &'a str) -> Option<usize> {
        let here = &self.scopes[scope];
        if let Some(id) = here.bindings.get(name) {
            return *id;
        }
        let outer = std::iter::successors(here.parent, |&id| self.scopes[id].parent)
            .find_map(|id| self.scopes[id].bindings.get(name))
            .copied()
            .flatten()?;
        self.scopes[scope].bindings.insert(name, Some(outer));
        Some(outer)
    }
    fn bind(&mut self, source: &'a str, pending: Vec<(std::ops::Range<usize>, usize, bool)>) {
        for (span, scope, qualified) in pending {
            let name = &source[span.clone()];
            let Some(id) = self.call_target(name, scope, qualified) else {
                continue;
            };
            call(&mut self.out, span, self.scopes[scope].owner, id);
        }
    }
    fn call_target(&mut self, name: &'a str, scope: usize, qualified: bool) -> Option<usize> {
        if qualified {
            return self.defs.get(name).copied().flatten();
        }
        self.lexical(scope, name)
    }

    fn close_scope(&mut self, name: &str) {
        let current = &self.scopes[self.current];
        if current.tag == name
            && let Some(parent) = current.parent
        {
            self.current = parent;
        }
    }

    fn declare_tag(
        &mut self,
        declaration: Option<(std::ops::Range<usize>, &'a str, &'static str)>,
        action: ScopeAction,
    ) -> Option<usize> {
        let (span, member, kind) = declaration?;
        let id = declare(&mut self.out, span, kind);
        if action != ScopeAction::Definition {
            return Some(id);
        }
        record_unique(&mut self.scopes[self.current].bindings, member, id);
        if self.current == 0 {
            record_unique(&mut self.defs, member, id);
        }
        Some(id)
    }

    fn open_scope(&mut self, name: &'a str, tag: Tag<'a>) {
        let scope = self.current;
        let action = tag.scope;
        let declared = self.declare_tag(tag.declaration, action);
        let parameter_scope = match action {
            ScopeAction::Definition | ScopeAction::Nested if !tag.self_closing => {
                let child = self.scopes.len();
                self.scopes.push(Scope {
                    parent: Some(scope),
                    tag: name,
                    owner: declared,
                    ..Default::default()
                });
                self.current = child;
                child
            }
            ScopeAction::Page => scope,
            _ => return,
        };
        if let Some(args) = tag.args {
            record_params(args, &mut self.scopes[parameter_scope]);
        }
    }
}

fn expression_call(body: &str, offset: usize) -> Option<(std::ops::Range<usize>, bool)> {
    let expression = body.trim_start();
    let local = expression.strip_prefix("local.");
    let member = local.unwrap_or(expression);
    let len = identifier(member);
    if len == 0 || !member[len..].trim_start().starts_with('(') {
        return None;
    }
    let start = offset + body.len() - member.len();
    Some((start..start + len, local.is_some()))
}

/// Python calls may be unqualified or use the template's local namespace.
/// A dot after any other receiver excludes the following name.
#[derive(Default)]
struct CallHead {
    local: bool,
    qualifier: Option<bool>,
}
impl CallHead {
    fn observe(
        &mut self,
        token: &PythonToken<'_>,
        tail: &str,
        end: usize,
    ) -> Option<(std::ops::Range<usize>, bool)> {
        match *token {
            PythonToken::Trivia => return None,
            PythonToken::Punct(b'.') => {
                self.qualifier = Some(self.local);
                self.local = false;
                return None;
            }
            PythonToken::Name(name) => {
                let receiver = self.qualifier.take();
                self.local = receiver.is_none() && name == "local";
                return (receiver != Some(false) && tail.trim_start().starts_with('('))
                    .then(|| (end - name.len()..end, receiver == Some(true)));
            }
            _ => *self = Self::default(),
        }
        None
    }
}

#[derive(Default)]
struct ExpressionCalls {
    head: CallHead,
    scoped: bool,
    filtered: bool,
}

impl ExpressionCalls {
    fn observe(
        &mut self,
        token: &PythonToken<'_>,
        tail: &str,
        after: usize,
        depth: usize,
    ) -> Option<(std::ops::Range<usize>, bool)> {
        match token {
            PythonToken::Name("lambda" | "for") | PythonToken::Walrus => self.scoped = true,
            PythonToken::Punct(b'|') if depth == 0 => self.filtered = true,
            _ => {}
        }
        if self.filtered {
            return None;
        }
        self.head.observe(token, tail, after)
    }
}
/// Scan the host boundary and embedded uses together. Binding-introducing
/// expressions retain only the historically supported outer call.
fn expression(
    code: &str,
    offset: usize,
    scope: usize,
    pending: &mut Vec<(std::ops::Range<usize>, usize, bool)>,
) -> usize {
    let mark = pending.len();
    let mut calls = ExpressionCalls::default();
    let width = python_region(
        code,
        Ending::Expression { braces: 0 },
        |token, tail, span, depth| {
            if let Some((name, local)) = calls.observe(&token, tail, offset + span.end, depth) {
                pending.push((name, scope, local));
            }
        },
    );
    if calls.scoped {
        pending.truncate(mark);
        pending.extend(
            expression_call(&code[..width], offset).map(|(name, local)| (name, scope, local)),
        );
    }
    width
}

enum Part<'a> {
    Comment,
    Code(&'a str, Ending),
    Expression,
    Open(&'a str, Tag<'a>),
    Close(&'a str),
    Text,
}

// HTML comments and both named Mako tag forms all start with '<'. An
// unrecognised introducer stays in template text, including a lone '<'.
fn tag_part(source: &str, at: usize) -> Option<(usize, Part<'_>)> {
    let rest = &source[at..];
    if rest.starts_with("<!--") {
        return Some((rest.find("-->").map_or(rest.len(), |n| n + 3), Part::Text));
    }
    if let Some(tail) = rest.strip_prefix("</%") {
        return Some(closing_part(tail));
    }
    let tail = rest.strip_prefix("<%")?;
    if !tail.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Some((2, Part::Code(tail, Ending::Block)));
    }
    let name = &tail[..identifier(tail)];
    let parsed = tag(source, at, name);
    Some((parsed.consumed, Part::Open(name, parsed)))
}

fn closing_part(tail: &str) -> (usize, Part<'_>) {
    let (name, terminal) = tail.split_at(identifier(tail));
    let part = if terminal.starts_with('>') {
        Part::Close(name)
    } else {
        Part::Text
    };
    (3 + name.len(), part)
}

fn part(source: &str, at: usize) -> (usize, Part<'_>) {
    let rest = &source[at..];
    if rest.starts_with("##") {
        return (rest.find('\n').unwrap_or(rest.len()), Part::Comment);
    }
    if rest.starts_with('%')
        && !rest.starts_with("%%")
        && source[..at]
            .trim_end_matches([' ', '\t'])
            .as_bytes()
            .last()
            .is_none_or(|&b| b == b'\n')
    {
        let end = rest.find('\n').unwrap_or(rest.len());
        return (1, Part::Code(&rest[1..end], Ending::Line));
    }
    if rest.starts_with('<')
        && let Some(part) = tag_part(source, at)
    {
        return part;
    }
    if rest.starts_with("${") {
        return (2, Part::Expression);
    }
    let first = rest.chars().next().unwrap().len_utf8();
    let end = rest[first..]
        .find(['#', '%', '<', '$'])
        .map_or(rest.len(), |n| first + n);
    (end, Part::Text)
}

pub(in crate::domain::code) fn analyze(source: &str, mut comment: impl FnMut(usize)) -> Analysis {
    let mut template = Template {
        out: Analysis::default(),
        scopes: vec![Scope::default()],
        current: 0,
        defs: Default::default(),
    };
    let mut pending = Vec::new();
    let mut at = 0;
    while at < source.len() {
        let (mut width, part) = part(source, at);
        let scope = template.current;
        match part {
            Part::Comment => comment(at),
            Part::Code(code, ending) => {
                width += record_writes(code, &mut template.scopes[scope], ending);
            }
            Part::Expression => {
                let body = at + width;
                width += expression(&source[body..], body, scope, &mut pending);
            }
            Part::Open(name, tag) => template.open_scope(name, tag),
            Part::Close(name) => template.close_scope(name),
            Part::Text => {}
        }
        at += width;
    }
    template.bind(source, pending);
    template.out
}
