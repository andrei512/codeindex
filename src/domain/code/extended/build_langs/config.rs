use super::{Facts, UseMode, span};
use crate::domain::code::Analysis;
use std::borrow::Cow;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Name,
    String,
    Heredoc,
    Punct,
    Opaque,
}

#[derive(Clone, Copy)]
pub(super) struct Token<'a> {
    pub text: &'a str,
    pub kind: Kind,
}

fn escaped_interpolation(prefix: &[u8]) -> bool {
    prefix.last() == Some(&b'$')
}

fn interpolation_open(bytes: &[u8], at: usize) -> bool {
    bytes.get(at + 1) == Some(&b'{') && !escaped_interpolation(&bytes[..at])
}

fn quote_end(s: &str, mut i: usize, depth: usize) -> usize {
    if depth >= 64 {
        return s.len();
    }
    let b = s.as_bytes();
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i = (i + 2).min(b.len()),
            b'"' => return i + 1,
            b'$' if interpolation_open(b, i) => {
                i = interpolation_end(s, i + 2, depth).unwrap_or(s.len());
            }
            _ => i += 1,
        }
    }
    i
}

fn interpolation_end(s: &str, mut i: usize, depth: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut braces = 1usize;
    while i < b.len() && braces > 0 {
        match b[i] {
            b'"' => {
                i = quote_end(s, i, depth + 1);
                continue;
            }
            b'{' => braces += 1,
            b'}' => braces -= 1,
            _ => (),
        }
        i += 1;
    }
    (braces == 0).then_some(i)
}

fn indented_end(s: &str) -> usize {
    let mut i = 2;
    let b = s.as_bytes();
    while let Some(next) = s.as_bytes()[i..].windows(2).position(|v| v == b"''") {
        i += next + 2;
        match b.get(i) {
            Some(b'\'' | b'$') => i += 1,
            Some(b'\\') => i = (i + 2).min(b.len()),
            _ => return i,
        }
    }
    s.len()
}

fn heredoc_end(s: &str) -> Option<usize> {
    let (header, body) = s.split_once('\n')?;
    let line = header.trim_end();
    let delimiter = line.strip_prefix("<<")?;
    let indented = delimiter.starts_with('-');
    let name = delimiter.strip_prefix('-').unwrap_or(delimiter);
    if !heredoc_name(name) {
        return None;
    }
    let close = body.split_inclusive('\n').find(|line| {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let written = if indented {
            trimmed.trim_start()
        } else {
            trimmed
        };
        written == name
    });
    Some(close.map_or(s.len(), |line| span(s, line).end))
}

fn heredoc_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A failed filesystem-path probe still establishes how much of a dotted
/// spelling was examined. The lexer can skip repeating that probe at each dot.
fn nix_path(s: &str) -> (bool, usize) {
    if let Some(tail) = s.strip_prefix('<') {
        return search_path_end(tail).map_or((false, 0), |end| (true, end));
    }
    let n = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || "_+-./~".contains(c)))
        .unwrap_or(s.len());
    let word = &s[..n];
    (
        word.len() > 1 && word.contains('/') && !word.contains("//"),
        n,
    )
}

fn search_path_end(tail: &str) -> Option<usize> {
    let end = tail.find(|c: char| c == '>' || c.is_whitespace())?;
    (end > 0 && tail.as_bytes()[end] == b'>').then_some(end + 2)
}

pub(super) fn tokens(s: &str, nix: bool) -> Vec<Token<'_>> {
    tokens_with_comments(s, nix, |_| {})
}

fn string_token(s: &str, nix: bool) -> Option<(Token<'_>, usize)> {
    if s.starts_with('"') {
        let end = quote_end(s, 0, 0);
        return Some((quoted_token(s, end), end));
    }
    let (end, kind) = if nix && s.starts_with("''") {
        (indented_end(s), Kind::Opaque)
    } else if !nix && s.starts_with("<<") {
        (heredoc_end(s)?, Kind::Heredoc)
    } else {
        return None;
    };
    Some((
        Token {
            text: &s[..end],
            kind,
        },
        end,
    ))
}

fn quoted_token(s: &str, end: usize) -> Token<'_> {
    if end > 1 && s.as_bytes()[end - 1] == b'"' {
        return Token {
            text: &s[1..end - 1],
            kind: Kind::String,
        };
    }
    Token {
        text: &s[..end],
        kind: Kind::Opaque,
    }
}

fn after_comment(s: &str, nix: bool) -> Option<&str> {
    if let Some(body) = s.strip_prefix("/*") {
        Some(body.split_once("*/").map_or("", |(_, tail)| tail))
    } else if starts_line_comment(s, nix) {
        Some(s.split_once('\n').map_or("", |(_, tail)| tail))
    } else {
        None
    }
}

fn starts_line_comment(s: &str, nix: bool) -> bool {
    s.starts_with('#') || (!nix && s.starts_with("//"))
}

pub(super) fn tokens_with_comments(
    mut s: &str,
    nix: bool,
    mut comment: impl FnMut(usize),
) -> Vec<Token<'_>> {
    let length = s.len();
    let mut out = Vec::new();
    let mut scanner = ConfigTokens { nix, path_end: 0 };
    while let Some(c) = {
        s = s.trim_start_matches(char::is_whitespace);
        s.chars().next()
    } {
        let (tail, token) = scanner.item(s, c, || comment(length - s.len()));
        s = tail;
        out.extend(token);
    }
    out
}

impl ConfigTokens {
    fn item<'a>(
        &mut self,
        s: &'a str,
        c: char,
        comment: impl FnOnce(),
    ) -> (&'a str, Option<Token<'a>>) {
        if let Some(tail) = after_comment(s, self.nix) {
            comment();
            return (tail, None);
        }
        let (token, size) = self.next(s, c);
        (&s[size..], Some(token))
    }
}

struct ConfigTokens {
    nix: bool,
    path_end: usize,
}
impl ConfigTokens {
    fn next<'a>(&mut self, s: &'a str, first: char) -> (Token<'a>, usize) {
        if let Some(token) = string_token(s, self.nix) {
            return token;
        }
        // A failed path probe covers all subsequent token starts in that spelling.
        if self.nix && s.as_ptr() as usize >= self.path_end {
            let (is_path, len) = nix_path(s);
            if is_path {
                return (
                    Token {
                        text: &s[..len],
                        kind: Kind::Opaque,
                    },
                    len,
                );
            }
            self.path_end = s.as_ptr() as usize + len;
        }
        let word = unquoted(s, first, self.nix);
        (word, word.text.len())
    }
}

fn unquoted(s: &str, first: char, nix: bool) -> Token<'_> {
    let name = |v: char| v.is_alphanumeric() || matches!(v, '_' | '-') || (nix && v == '\'');
    if name(first) {
        let end = s.find(|v: char| !name(v)).unwrap_or(s.len());
        Token {
            text: &s[..end],
            kind: Kind::Name,
        }
    } else {
        let end = ["...", "==", "!=", "=>", "//", "||", "&&", "++", "->", "${"]
            .iter()
            .find_map(|op| s.starts_with(op).then_some(op.len()))
            .unwrap_or(first.len_utf8());
        Token {
            text: &s[..end],
            kind: Kind::Punct,
        }
    }
}

fn label(t: Token) -> bool {
    matches!(t.kind, Kind::Name | Kind::String)
        && !t.text.is_empty()
        && !t.text.contains(['$', '%', '\\'])
}

#[derive(Clone, Copy, PartialEq)]
enum BodyKind {
    Block,
    Locals,
    Expression,
}

#[derive(Clone, Copy)]
struct Body {
    owner: Option<usize>,
    kind: BodyKind,
}

struct Hcl<'a> {
    source: &'a str,
    facts: Facts<Cow<'a, str>>,
    bodies: Vec<Body>,
}

pub(in crate::domain::code) fn hcl(source: &str) -> Analysis {
    let tokens = tokens(source, false);
    let mut parser = Hcl {
        source,
        facts: Facts::default(),
        bodies: vec![Body {
            owner: None,
            kind: BodyKind::Block,
        }],
    };
    let mut i = 0;
    while i < tokens.len() {
        i = parser.consume(&tokens, i);
    }
    parser.facts.finish()
}

impl<'a> Hcl<'a> {
    fn consume(&mut self, tokens: &[Token<'a>], i: usize) -> usize {
        let token = tokens[i];
        let body = *self.bodies.last().unwrap();
        match (token.kind, token.text) {
            (Kind::Punct, "}") => {
                if self.bodies.len() > 1 {
                    self.bodies.pop();
                }
            }
            (Kind::Punct, "{" | "${") => {
                self.bodies.push(Body {
                    kind: BodyKind::Expression,
                    ..body
                });
            }
            (Kind::Name, _) => return self.name_item(tokens, i, body),
            (Kind::String | Kind::Heredoc, _) => self.interpolations(token.text, body.owner),
            _ => {}
        }
        i + 1
    }

    fn name_item(&mut self, tokens: &[Token<'a>], i: usize, body: Body) -> usize {
        if body.kind != BodyKind::Expression
            && self.at_item_start(tokens, i)
            && let Some(end) = self.declaration(tokens, i, body)
        {
            return end;
        }
        if i == 0 || tokens[i - 1].text != "." {
            return self.address(tokens, i, body.owner);
        }
        i + 1
    }

    fn newline_between(&self, left: Token, right: Token) -> bool {
        self.source[span(self.source, left.text).end..span(self.source, right.text).start]
            .contains('\n')
    }

    fn at_item_start(&self, tokens: &[Token], i: usize) -> bool {
        if i == 0 {
            return true;
        }
        let prev = tokens[i - 1];
        (prev.kind == Kind::Punct && matches!(prev.text, "{" | "}"))
            || self.newline_between(prev, tokens[i])
    }

    /// Only literal labels on the header line can introduce a static block.
    fn block_open(&self, tokens: &[Token], i: usize) -> Option<usize> {
        let mut end = i + 1;
        while let Some(&next) = tokens.get(end) {
            if !label(next) || self.newline_between(tokens[end - 1], next) {
                break;
            }
            end += 1;
        }
        let open = tokens.get(end)?;
        if open.kind != Kind::Punct || open.text != "{" {
            return None;
        }
        Some(end)
    }

    fn declaration(&mut self, tokens: &[Token<'a>], i: usize, parent: Body) -> Option<usize> {
        let name = tokens[i].text;
        if tokens
            .get(i + 1)
            .is_some_and(|t| t.kind == Kind::Punct && t.text == "=")
        {
            self.attribute(name, parent.kind);
            return Some(i + 2);
        }
        let end = self.block_open(tokens, i)?;
        let labels = &tokens[i + 1..end];
        let body = self.block_body(name, labels, parent.owner);
        self.bodies.push(body);
        Some(end + 1)
    }

    fn attribute(&mut self, name: &str, kind: BodyKind) {
        let at = span(self.source, name);
        if kind == BodyKind::Locals {
            let id = self.facts.declare(format!("local.{name}"), at, "attribute");
            self.bodies.last_mut().unwrap().owner = Some(id);
        } else {
            self.facts.analysis.declare_at(at, "attribute");
        }
    }

    /// Unlabelled locals at the root changes the meaning of attribute writes.
    /// A labelled block owns its own uses; other unlabelled blocks inherit.
    fn block_body(&mut self, name: &str, labels: &[Token], enclosing: Option<usize>) -> Body {
        let top = self.bodies.len() == 1;
        let Some(last) = labels.last() else {
            let kind = match (top, name) {
                (true, "locals") => BodyKind::Locals,
                _ => BodyKind::Block,
            };
            return Body {
                kind,
                owner: enclosing,
            };
        };
        let at = span(self.source, last.text);
        let key = top.then(|| root_block_key(name, labels)).flatten();
        let id = match key {
            Some(key) => self.facts.declare(key, at, "block_label"),
            None => self.facts.analysis.declare_at(at, "block_label"),
        };
        Body {
            kind: BodyKind::Block,
            owner: Some(id),
        }
    }

    fn address(&mut self, tokens: &[Token<'a>], i: usize, owner: Option<usize>) -> usize {
        let mut end = i + 1;
        while let [dot, part, ..] = &tokens[end..] {
            if dot.kind != Kind::Punct || dot.text != "." || part.kind != Kind::Name {
                break;
            }
            end += 2;
        }
        if end > i + 1 {
            let names = tokens[i..end].iter().step_by(2).map(|t| t.text);
            traversal(self.source, names, owner, &mut self.facts);
        }
        end
    }

    fn interpolations(&mut self, text: &'a str, owner: Option<usize>) {
        for expr in template_expressions(text) {
            let names = expr.trim().split('.');
            if names.clone().all(hcl_identifier) {
                traversal(self.source, names, owner, &mut self.facts);
            }
        }
    }
}

fn root_block_key(name: &str, labels: &[Token]) -> Option<String> {
    match (name, labels) {
        ("variable", [n]) => Some(format!("var.{}", n.text)),
        ("module", [n]) => Some(format!("module.{}", n.text)),
        ("resource", [ty, n]) => Some(format!("{}.{}", ty.text, n.text)),
        ("data", [ty, n]) => Some(format!("data.{}.{}", ty.text, n.text)),
        _ => None,
    }
}

// A literal $${ consumes the introducer alone. It does not own a closing
// brace or any subsequent interpolation. Real expressions use the lexer's
// balanced endpoint so braces inside quoted strings cannot end them.
fn template_expressions(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        loop {
            let (prefix, rest) = text.split_once("${")?;
            text = rest;
            if escaped_interpolation(prefix.as_bytes()) {
                continue;
            }
            let end = interpolation_end(rest, 0, 0)?;
            text = &rest[end..];
            return Some(&rest[..end - 1]);
        }
    })
}

fn hcl_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
}

struct Address<'a> {
    namespace: &'a str,
    name: &'a str,
    instance: Option<&'a str>,
}

impl<'a> Address<'a> {
    fn read(mut names: impl Iterator<Item = &'a str>) -> Option<Self> {
        let namespace = names.next()?;
        let name = names.next()?;
        let instance = if namespace == "data" {
            Some(names.next()?)
        } else {
            None
        };
        Some(Self {
            namespace,
            name,
            instance,
        })
    }

    fn key(&self, source: &'a str, end: usize) -> Cow<'a, str> {
        let address = &source[span(source, self.namespace).start..end];
        // Use the original spelling for contiguous addresses.
        let len = self.namespace.len()
            + 1
            + self.name.len()
            + self.instance.map_or(0, |name| name.len() + 1);
        if address.len() == len {
            return Cow::Borrowed(address);
        }
        let mut key = String::with_capacity(len);
        key.push_str(self.namespace);
        key.push('.');
        key.push_str(self.name);
        if let Some(instance) = self.instance {
            key.push('.');
            key.push_str(instance);
        }
        Cow::Owned(key)
    }
}

fn traversal<'a>(
    source: &'a str,
    names: impl Iterator<Item = &'a str>,
    owner: Option<usize>,
    f: &mut Facts<Cow<'a, str>>,
) {
    let Some(address) = Address::read(names) else {
        return;
    };
    let target = span(source, address.instance.unwrap_or(address.name));
    f.usage(
        address.key(source, target.end),
        target,
        owner,
        UseMode::Reference,
    );
}
