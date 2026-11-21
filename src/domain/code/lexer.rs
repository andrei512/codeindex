//! Language-configured lexical scanner. Token text remains in the captured source.
use super::Language;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Word,
    String,
    Punct,
}

pub(super) struct Token<'a> {
    pub text: &'a str,
    pub span: Range<usize>,
    pub line: usize,
    pub kind: Kind,
}

// At the start of an expression ECMAScript reads a regexp literal. Keep its
// pattern opaque so words and parentheses inside it never become declarations.
fn regex_end(bytes: &[u8], mut pos: usize) -> Option<usize> {
    let mut class = false;
    while let Some(&c) = bytes.get(pos) {
        match c {
            b'\n' | b'\r' => return None,
            b'\\' => {
                if matches!(bytes.get(pos + 1), None | Some(b'\n' | b'\r')) {
                    return None;
                }
                pos += 1;
            }
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => {
                pos += 1;
                while bytes.get(pos).is_some_and(u8::is_ascii_alphabetic) {
                    pos += 1;
                }
                return Some(pos);
            }
            _ => {}
        }
        pos += 1;
    }
    None
}

pub(super) fn tokens(source: &str, lang: Language) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut result: Vec<Token> = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < bytes.len() {
        let start = i;
        let start_line = line;
        let tail = &source[start..];
        let c = tail.chars().next().unwrap();
        i += c.len_utf8();
        if c.is_whitespace() {
            line += usize::from(c == '\n');
            continue;
        }
        if lang.line_comment(tail) {
            i = source[start..]
                .find('\n')
                .map_or(bytes.len(), |n| start + n);
            continue;
        }
        if lang.block_comments() && tail.starts_with("/*") {
            i = start + 2;
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else if lang == Language::Rust && bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else {
                    line += usize::from(bytes[i] == b'\n');
                    i += 1;
                }
            }
            continue;
        }
        if lang == Language::Html && tail.starts_with("<!--") {
            i = tail.find("-->").map_or(bytes.len(), |end| start + end + 3);
            line += bytes[start..i].iter().filter(|b| **b == b'\n').count();
            continue;
        }
        if lang == Language::Rust {
            if let Some(end) = rust_literal_end(tail) {
                i = start + end;
                line += bytes[start..i].iter().filter(|&&b| b == b'\n').count();
                result.push(Token {
                    text: &source[start..i],
                    span: start..i,
                    line: start_line,
                    kind: Kind::String,
                });
                continue;
            }
            if let Some(raw) = tail
                .strip_prefix("r#")
                .filter(|r| r.starts_with(|c: char| c == '_' || c.is_alphabetic()))
            {
                let length = raw
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(raw.len());
                i = start + 2 + length;
                result.push(Token {
                    text: &source[start..i],
                    span: start..i,
                    line: start_line,
                    kind: Kind::Word,
                });
                continue;
            }
        }
        let mut kind = Kind::Punct;
        if lang == Language::Js
            && c == '/'
            && result.last().is_none_or(|t| {
                matches!(
                    t.text,
                    "=" | "("
                        | "{"
                        | "["
                        | ","
                        | ";"
                        | ":"
                        | "!"
                        | "&"
                        | "|"
                        | "?"
                        | "return"
                        | "throw"
                        | "case"
                        | "yield"
                )
            })
            && let Some(stop) = regex_end(bytes, i)
        {
            kind = Kind::String;
            i = stop;
        } else if matches!(c, '\'' | '"' | '`')
            && !(lang == Language::Rust && c == '\'' && tail.chars().nth(2) != Some('\''))
        {
            kind = Kind::String;
            let q = c as u8;
            let triple = lang == Language::Python && bytes[start..].starts_with(&[q, q, q]);
            if triple {
                i += 2;
            }
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    line += usize::from(bytes[i + 1] == b'\n');
                    i += 2;
                    continue;
                }
                if bytes[i] == q && (!triple || bytes[i..].starts_with(&[q, q, q])) {
                    i += if triple { 3 } else { 1 };
                    break;
                }
                line += usize::from(bytes[i] == b'\n');
                i += 1;
            }
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            kind = Kind::Word;
            for n in source[i..].chars() {
                if n.is_alphanumeric()
                    || n == '_'
                    || n == '$'
                    || n == '-' && matches!(lang, Language::Powershell | Language::Css)
                {
                    i += n.len_utf8();
                } else {
                    break;
                }
            }
        }
        result.push(Token {
            text: &source[start..i],
            span: start..i,
            line: start_line,
            kind,
        });
    }
    result
}

fn rust_literal_end(text: &str) -> Option<usize> {
    let raw = text
        .strip_prefix("br")
        .or_else(|| text.strip_prefix("cr"))
        .or_else(|| text.strip_prefix('r'));
    if let Some(raw) = raw {
        let hashes = raw.bytes().take_while(|&c| c == b'#').count();
        if raw.as_bytes().get(hashes) == Some(&b'"') {
            let start = text.len() - raw.len() + hashes + 1;
            for (offset, _) in text[start..].match_indices('"') {
                let after = start + offset + 1;
                if text.as_bytes()[after..].starts_with(&raw.as_bytes()[..hashes]) {
                    return Some(after + hashes);
                }
            }
            return Some(text.len());
        }
    }
    let quoted = text
        .strip_prefix('b')
        .or_else(|| text.strip_prefix('c'))
        .unwrap_or(text);
    let prefix = text.len() - quoted.len();
    let bytes = quoted.as_bytes();
    let q = *bytes.first()?;
    if q != b'"' && q != b'\'' {
        return None;
    }
    // A lifetime consumes no quoted token. Its first scalar would have to be
    // immediately followed by a quote to be an ordinary character literal.
    if q == b'\'' && bytes.get(1) != Some(&b'\\') {
        let first = quoted[1..].chars().next()?;
        let end = 1 + first.len_utf8();
        return (bytes.get(end) == Some(&q)).then_some(prefix + end + 1);
    }
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            c if c == q => return Some(prefix + i + 1),
            _ => i += 1,
        }
    }
    Some(text.len())
}
