use super::{Facts, span};
use crate::domain::code::Analysis;

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
            b'$' if b.get(i + 1) == Some(&b'{') && !(i > 0 && b[i - 1] == b'$') => {
                i += 2;
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
            }
            _ => i += 1,
        }
    }
    i
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
    let line = s.split_once('\n')?.0.trim_end();
    let delimiter = line.strip_prefix("<<")?;
    let indented = delimiter.starts_with('-');
    let name = delimiter.strip_prefix('-').unwrap_or(delimiter);
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    let mut size = s.find('\n')? + 1;
    for line in s[size..].split_inclusive('\n') {
        size += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        if (if indented { line.trim_start() } else { line }) == name {
            break;
        }
    }
    Some(size)
}

fn nix_path(s: &str) -> Option<usize> {
    if let Some(tail) = s.strip_prefix('<') {
        let n = tail.find(|c: char| c == '>' || c.is_whitespace())?;
        if n > 0 && tail.as_bytes()[n] == b'>' {
            return Some(n + 2);
        }
        return None;
    }
    let n = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || "_+-./~".contains(c)))
        .unwrap_or(s.len());
    let word = &s[..n];
    if word.len() > 1 && word.contains('/') && !word.contains("//") {
        return Some(n);
    }
    None
}

pub(super) fn tokens(mut s: &str, nix: bool) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut scanned_path_end = 0;
    while !s.is_empty() {
        s = s.trim_start_matches(char::is_whitespace);
        let Some(c) = s.chars().next() else { break };
        if c == '#' || (!nix && s.starts_with("//")) {
            s = s.split_once('\n').map_or("", |(_, r)| r);
            continue;
        }
        if let Some(t) = s.strip_prefix("/*") {
            s = t.split_once("*/").map_or("", |(_, r)| r);
            continue;
        }
        if c == '"' {
            let end = quote_end(s, 0, 0);
            let closed = end > 1 && s.as_bytes()[end - 1] == b'"';
            let text = if closed { &s[1..end - 1] } else { &s[..end] };
            out.push(Token {
                text,
                kind: if closed { Kind::String } else { Kind::Opaque },
            });
            s = &s[end..];
            continue;
        }
        if nix && s.starts_with("''") {
            let end = indented_end(s);
            out.push(Token {
                text: &s[..end],
                kind: Kind::Opaque,
            });
            s = &s[end..];
            continue;
        }
        if !nix
            && s.starts_with("<<")
            && let Some(end) = heredoc_end(s)
        {
            out.push(Token {
                text: &s[..end],
                kind: Kind::Heredoc,
            });
            s = &s[end..];
            continue;
        }
        // Scan each contiguous Nix path/selection spelling once. A filesystem
        // path or a lookup path has no lexical variable references in its name.
        if nix && s.as_ptr() as usize >= scanned_path_end {
            if let Some(n) = nix_path(s) {
                out.push(Token {
                    text: &s[..n],
                    kind: Kind::Opaque,
                });
                s = &s[n..];
                continue;
            }
            let n = s
                .find(|c: char| !(c.is_ascii_alphanumeric() || "_+-./~".contains(c)))
                .unwrap_or(s.len());
            scanned_path_end = s.as_ptr() as usize + n;
        }
        let name = |v: char| v.is_alphanumeric() || matches!(v, '_' | '-') || (nix && v == '\'');
        if name(c) {
            let end = s.find(|v: char| !name(v)).unwrap_or(s.len());
            out.push(Token {
                text: &s[..end],
                kind: Kind::Name,
            });
            s = &s[end..];
            continue;
        }
        let end = ["...", "==", "!=", "=>", "//", "||", "&&", "++", "->", "${"]
            .iter()
            .find_map(|op| s.starts_with(op).then_some(op.len()))
            .unwrap_or(c.len_utf8());
        out.push(Token {
            text: &s[..end],
            kind: Kind::Punct,
        });
        s = &s[end..];
    }
    out
}

fn label(t: Token) -> bool {
    matches!(t.kind, Kind::Name | Kind::String)
        && !t.text.is_empty()
        && !t.text.contains(['$', '%', '\\'])
}

#[derive(Clone, Copy)]
struct Body {
    owner: Option<usize>,
    locals: bool,
    config: bool,
}

pub(super) fn hcl(source: &str) -> Analysis {
    let t = tokens(source, false);
    let mut f = Facts::default();
    let mut stack = vec![Body {
        owner: None,
        locals: false,
        config: true,
    }];
    let mut i = 0;
    while i < t.len() {
        let tok = t[i];
        let body = *stack.last().unwrap();
        if tok.kind == Kind::Punct && tok.text == "}" {
            if stack.len() > 1 {
                stack.pop();
            }
            i += 1;
            continue;
        }
        if tok.kind == Kind::Punct && matches!(tok.text, "{" | "${") {
            stack.push(Body {
                config: false,
                locals: false,
                ..body
            });
            i += 1;
            continue;
        }
        if body.config
            && tok.kind == Kind::Name
            && (i == 0 || {
                let prev = t[i - 1];
                let end = span(source, prev.text).end;
                matches!(prev.kind, Kind::Punct) && matches!(prev.text, "{" | "}")
                    || source[end..span(source, tok.text).start].contains('\n')
            })
        {
            let mut end = i + 1;
            while end < t.len()
                && label(t[end])
                && !source[span(source, t[end - 1].text).end..span(source, t[end].text).start]
                    .contains('\n')
            {
                end += 1;
            }
            if t.get(end)
                .is_some_and(|v| v.kind == Kind::Punct && v.text == "{")
            {
                let labels = &t[i + 1..end];
                let top = stack.len() == 1;
                let owner = labels
                    .last()
                    .map(|last| {
                        let key = match (top, tok.text, labels) {
                            (true, "variable", [n]) => format!("var.{}", n.text),
                            (true, "module", [n]) => format!("module.{}", n.text),
                            (true, "resource", [ty, n]) => format!("{}.{}", ty.text, n.text),
                            (true, "data", [ty, n]) => format!("data.{}.{}", ty.text, n.text),
                            _ => format!("block:{}", i),
                        };
                        f.declare(key, span(source, last.text), "block_label")
                    })
                    .or(body.owner);
                stack.push(Body {
                    owner,
                    locals: top && tok.text == "locals" && labels.is_empty(),
                    config: true,
                });
                i = end + 1;
                continue;
            }
            if let Some(next) = t.get(i + 1)
                && next.kind == Kind::Punct
                && next.text == "="
            {
                let key = if body.locals {
                    format!("local.{}", tok.text)
                } else {
                    format!("attr:{i}")
                };
                let attr = f.declare(key, span(source, tok.text), "attribute");
                // Expressions remain owned by their containing block, or by the
                // named local value in a locals block.
                if body.locals {
                    stack.last_mut().unwrap().owner = Some(attr);
                }
                i += 2;
                continue;
            }
        }
        if tok.kind == Kind::Name && (i == 0 || t[i - 1].text != ".") {
            let mut end = i + 1;
            while end + 1 < t.len()
                && t[end].text == "."
                && t[end].kind == Kind::Punct
                && t[end + 1].kind == Kind::Name
            {
                end += 2;
            }
            if end > i + 1 {
                let names = t[i..end]
                    .iter()
                    .step_by(2)
                    .map(|v| v.text)
                    .collect::<Vec<_>>();
                traversal(source, &names, body.owner, &mut f);
                i = end;
                continue;
            }
        }
        if matches!(tok.kind, Kind::String | Kind::Heredoc) {
            let mut tail = tok.text;
            while let Some((prefix, rest)) = tail.split_once("${") {
                let Some((expr, next)) = rest.split_once('}') else {
                    break;
                };
                tail = next;
                if prefix.ends_with('$') {
                    continue;
                }
                let names = expr.trim().split('.').collect::<Vec<_>>();
                if names.iter().all(|s| {
                    !s.is_empty()
                        && s.chars()
                            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
                }) {
                    traversal(source, &names, body.owner, &mut f);
                }
            }
        }
        i += 1;
    }
    f.finish()
}

fn traversal(source: &str, names: &[&str], owner: Option<usize>, f: &mut Facts) {
    let count = if names.first() == Some(&"data") { 3 } else { 2 };
    if names.len() >= count {
        f.usage(
            names[..count].join("."),
            span(source, names[count - 1]),
            owner,
            false,
            false,
        );
    }
}
