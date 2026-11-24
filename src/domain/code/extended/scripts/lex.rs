//! Literals and comments are opaque. Offsets always refer to the original UTF-8.
use super::Language;
use std::{collections::VecDeque, ops::Range};

#[derive(Clone, Copy)]
pub(super) struct Token<'a> {
    pub text: &'a str,
    pub at: usize,
}

impl Token<'_> {
    pub fn span(self) -> Range<usize> {
        self.at..self.at + self.text.len()
    }
    pub fn name(self) -> bool {
        self.text
            .starts_with(|c: char| c == '_' || c.is_alphabetic())
    }
}

pub(super) fn identifier(s: &str) -> usize {
    if !s.starts_with(|c: char| c == '_' || c.is_alphabetic()) {
        return 0;
    }
    s.find(|c: char| c != '_' && !c.is_alphanumeric())
        .unwrap_or(s.len())
}

fn line_end(s: &str) -> usize {
    s.find('\n').unwrap_or(s.len())
}

pub(super) fn quoted(s: &str, q: u8) -> usize {
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() {
        if b[i] == q {
            return i + 1;
        }
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    b.len()
}

// Lua's long brackets must close with exactly as many '=' characters as opened.
fn long_bracket(s: &str) -> Option<usize> {
    let r = s.strip_prefix('[')?;
    let n = r.bytes().take_while(|&c| c == b'=').count();
    let body = r.get(n..)?.strip_prefix('[')?;
    let close = format!("]{}]", &r[..n]);
    Some(
        body.find(&close)
            .map_or(s.len(), |i| n + 2 + i + close.len()),
    )
}

fn block_comment(s: &str, nested: bool) -> usize {
    let b = s.as_bytes();
    let (mut pos, mut depth) = (2, 1usize);
    while pos + 1 < b.len() {
        match &b[pos..pos + 2] {
            b"*/" => {
                depth -= 1;
                pos += 2;
                if depth == 0 {
                    return pos;
                }
            }
            b"/*" if nested => {
                depth += 1;
                pos += 2;
            }
            _ => pos += 1,
        }
    }
    b.len()
}

// Ruby percent literals and Perl quote-like operators allow paired, nested delimiters.
fn delimited(s: &str, open: char, close: char) -> usize {
    let mut depth = 1usize;
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
            continue;
        }
        if c == close {
            depth -= 1;
            if depth == 0 {
                return i + c.len_utf8();
            }
        } else if c == open {
            depth += 1;
        }
    }
    s.len()
}
fn pair(c: char) -> char {
    match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => c,
    }
}
fn quote_operator(s: &str, lang: Language) -> Option<usize> {
    let (prefix, parts) = if lang == Language::Ruby {
        let tail = s.strip_prefix('%')?;
        let has_kind = tail.starts_with(['q', 'Q', 'w', 'W', 'i', 'I', 'r', 'x', 's']);
        (1 + usize::from(has_kind), 1)
    } else {
        let n = identifier(s);
        let parts = match &s[..n] {
            "q" | "qq" | "qw" | "qr" | "qx" | "m" => 1,
            "s" | "tr" | "y" => 2,
            _ => return None,
        };
        (
            s.len() - s[n..].trim_start_matches([' ', '\t']).len(),
            parts,
        )
    };
    let open = s[prefix..].chars().next()?;
    if open.is_alphanumeric() || open == '_' || open.is_whitespace() {
        return None;
    }
    let close = pair(open);
    let mut end = prefix + open.len_utf8();
    end += delimited(&s[end..], open, close);
    if parts == 2 && end < s.len() {
        if open == close {
            end += delimited(&s[end..], open, close);
        } else {
            let rest = s[end..].trim_start();
            let next = rest.chars().next()?;
            end = s.len() - rest.len() + next.len_utf8();
            end += delimited(&s[end..], next, pair(next));
        }
    }
    Some(end)
}

fn heredoc(s: &str) -> Option<(&str, bool, usize)> {
    let body = s.strip_prefix("<<")?;
    let indent = body.starts_with(['~', '-']);
    let rest = &body[usize::from(indent)..];
    let q = rest
        .as_bytes()
        .first()
        .copied()
        .filter(|c| matches!(c, b'\'' | b'"' | b'`'));
    let value = &rest[usize::from(q.is_some())..];
    let len = identifier(value);
    if len == 0 || q.is_some_and(|c| value.as_bytes().get(len) != Some(&c)) {
        return None;
    }
    let used = s.len() - value.len() + len + usize::from(q.is_some());
    Some((&value[..len], indent, used))
}

pub(super) fn tokens(source: &str, lang: Language) -> Vec<Token<'_>> {
    let mut out = Vec::<Token>::new();
    let mut pending = VecDeque::new();
    let mut at = 0;
    while at < source.len() {
        let s = &source[at..];
        let c = s.chars().next().unwrap();
        let bol = at == 0 || source.as_bytes()[at - 1] == b'\n';
        if c == '\n' && !pending.is_empty() {
            at += 1;
            while let Some((name, indent)) = pending.pop_front() {
                while at < source.len() {
                    let n = line_end(&source[at..]);
                    let line = source[at..at + n].trim_end_matches('\r');
                    at += (n + 1).min(source.len() - at);
                    if (if indent { line.trim_start() } else { line }) == name {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_whitespace() {
            at += c.len_utf8();
            continue;
        }
        if bol && matches!(lang, Language::Perl | Language::Ruby) {
            let line = s[..line_end(s)].trim_end();
            if line == "__END__" || (lang == Language::Perl && line == "__DATA__") {
                break;
            }
            let pod = lang == Language::Perl
                && s.strip_prefix('=')
                    .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()));
            if pod
                || (lang == Language::Ruby
                    && s.starts_with("=begin")
                    && s[6..].starts_with(char::is_whitespace))
            {
                let end = if pod { "\n=cut" } else { "\n=end" };
                at += s
                    .find(end)
                    .map_or(s.len(), |n| n + 1 + line_end(&s[n + 1..]));
                continue;
            }
        }
        if lang == Language::Lua && s.starts_with("--") {
            at += long_bracket(&s[2..]).map_or_else(|| line_end(s), |n| n + 2);
            continue;
        }
        if (matches!(lang, Language::Ruby | Language::Perl) && c == '#')
            || (lang == Language::PlPgSql && s.starts_with("--"))
            || (lang == Language::Assembly && (matches!(c, ';' | '#' | '@') || s.starts_with("//")))
        {
            at += line_end(s);
            continue;
        }
        if matches!(lang, Language::PlPgSql | Language::Assembly) && s.starts_with("/*") {
            at += block_comment(s, lang == Language::PlPgSql);
            continue;
        }
        let quote = match lang {
            Language::Lua => long_bracket(s),
            Language::PlPgSql => crate::domain::code::lexer::sql_dollar_string_len(s),
            Language::Ruby | Language::Perl => {
                if let Some((name, indent, len)) = heredoc(s) {
                    pending.push_back((name, indent));
                    Some(len)
                } else if out.last().is_none_or(|p| {
                    !matches!(
                        p.text,
                        "sub" | "package" | ":" | ">" | "$" | "@" | "%" | "&"
                    ) && (p.span().end != at || !p.name())
                }) {
                    quote_operator(s, lang)
                } else {
                    None
                }
            }
            _ => None,
        };
        let n = if let Some(n) = quote {
            n
        } else if matches!(c, '\'' | '"' | '`') {
            if lang == Language::PlPgSql && c != '`' {
                let escape = c == '\''
                    && out
                        .last()
                        .is_some_and(|p| p.text.eq_ignore_ascii_case("e") && p.span().end == at);
                let b = s.as_bytes();
                let mut i = 1;
                while i < b.len() {
                    if escape && b[i] == b'\\' {
                        i = (i + 2).min(b.len());
                    } else if b[i] == c as u8 {
                        i += 1;
                        if b.get(i) != Some(&(c as u8)) {
                            break;
                        }
                        i += 1;
                    } else {
                        i += 1;
                    }
                }
                i
            } else {
                quoted(s, c as u8)
            }
        } else if c == '/'
            && matches!(lang, Language::Perl | Language::Ruby)
            && out.last().is_none_or(|t| {
                matches!(
                    t.text,
                    "=" | "~" | "(" | "," | "return" | "when" | "if" | "unless" | "puts" | "print"
                ) || source[t.span().end..at].contains('\n')
            })
        {
            quoted(s, b'/')
        } else {
            if lang == Language::Assembly && (c == '_' || c.is_alphabetic()) {
                s.find(|n: char| n != '_' && n != '.' && !n.is_alphanumeric()).unwrap_or(s.len())
            } else if lang == Language::PlPgSql && (c == '_' || c.is_alphabetic()) {
                s.find(|n: char| n != '_' && n != '$' && !n.is_alphanumeric())
                    .unwrap_or(s.len())
            } else {
                identifier(s).max(c.len_utf8())
            }
        };
        out.push(Token { text: &s[..n], at });
        at += n;
    }
    out
}
