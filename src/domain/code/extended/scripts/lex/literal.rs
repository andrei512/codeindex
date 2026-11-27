//! Opaque literals and the token boundaries that distinguish them from operators.
use super::{Language, Scanner, Token, identifier, quoted};
// Ruby percent literals and Perl quote-like operators allow paired, nested delimiters.
fn delimited(s: &str, open: char, close: char) -> usize {
    let mut depth = 1usize;
    let closing = super::super::super::unescaped_chars(s).find(|&(_, c)| {
        if c == close {
            depth -= 1;
        } else if c == open {
            depth += 1;
        }
        depth == 0
    });
    closing.map_or(s.len(), |(i, c)| i + c.len_utf8())
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
fn quote_prefix(s: &str, lang: Language) -> Option<(usize, usize)> {
    let (prefix, parts) = if lang == Language::Ruby {
        let tail = s.strip_prefix('%')?;
        let has_kind = tail.starts_with(['q', 'Q', 'w', 'W', 'i', 'I', 'r', 'x', 's']);
        (1 + usize::from(has_kind), 1)
    } else {
        let (n, parts) = match s.as_bytes() {
            [b'q', b'q' | b'w' | b'r' | b'x', ..] => (2, 1),
            [b'q' | b'm', ..] => (1, 1),
            [b't', b'r', ..] => (2, 2),
            [b's' | b'y', ..] => (1, 2),
            _ => return None,
        };
        (
            s.len() - s[n..].trim_start_matches([' ', '\t']).len(),
            parts,
        )
    };
    Some((prefix, parts))
}

fn is_quote_delimiter(c: char) -> bool {
    c != '_' && !c.is_alphanumeric() && !c.is_whitespace()
}

fn quote_operator(s: &str, lang: Language) -> Option<usize> {
    let (prefix, parts) = quote_prefix(s, lang)?;
    let open = s[prefix..]
        .chars()
        .next()
        .filter(|&c| is_quote_delimiter(c))?;
    let close = pair(open);
    let inside = prefix + open.len_utf8();
    let end = inside + delimited(&s[inside..], open, close);
    if parts == 1 || end == s.len() {
        return Some(end);
    }
    // Paired replacement syntax supplies a new delimiter; /a/b/ reuses '/'.
    let (inside, open) = if open == close {
        (end, open)
    } else {
        let rest = s[end..].trim_start();
        let next = rest.chars().next()?;
        (s.len() - rest.len() + next.len_utf8(), next)
    };
    Some(inside + delimited(&s[inside..], open, pair(open)))
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

fn sql_quote(s: &str, quote: u8, escape: bool) -> usize {
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() {
        let Some(width) = sql_quote_step(&b[i..], quote, escape) else {
            return i + 1;
        };
        i = (i + width).min(b.len());
    }
    i
}

fn sql_quote_step(bytes: &[u8], quote: u8, escape: bool) -> Option<usize> {
    match bytes {
        [b'\\', ..] if escape => Some(2),
        [first, second, ..] if (*first, *second) == (quote, quote) => Some(2),
        [first, ..] if *first == quote => None,
        _ => Some(1),
    }
}

impl<'s> Scanner<'s> {
    #[inline]
    pub(super) fn token_width(&mut self, s: &'s str, c: char) -> usize {
        use Language::{Lua, Perl, PlPgSql, Ruby};
        let literal = match (self.lang, c) {
            (Lua, '[') => crate::domain::code::lexer::bracket_literal(s).map(|(_, end)| end),
            (PlPgSql, '$') => crate::domain::code::lexer::sql_dollar_string_len(s),
            (PlPgSql, '\'' | '"') => Some(self.sql_quote(s, c)),
            (_, '\'' | '"' | '`') => Some(quoted(s, c as u8)),
            (Ruby | Perl, _) => self.script_literal(s, c),
            _ => None,
        };
        literal.unwrap_or_else(|| identifier_width(s, c, self.lang))
    }

    fn sql_quote(&self, s: &str, quote: char) -> usize {
        sql_quote(s, quote as u8, quote == '\'' && self.escape_prefix())
    }

    fn script_literal(&mut self, s: &'s str, c: char) -> Option<usize> {
        use Language::{Perl, Ruby};
        match (self.lang, c) {
            (_, '<') => heredoc(s).map(|(name, indent, len)| {
                self.pending.push_back((name, indent));
                len
            }),
            (_, '/') if self.starts_slash_literal() => Some(quoted(s, b'/')),
            (Ruby, '%') | (Perl, 'q' | 'm' | 's' | 't' | 'y') if self.allows_quote_operator() => {
                quote_operator(s, self.lang)
            }
            _ => None,
        }
    }

    fn allows_quote_operator(&self) -> bool {
        self.previous
            .as_ref()
            .is_none_or(|p| quote_context(p, self.at))
    }

    fn escape_prefix(&self) -> bool {
        self.previous
            .as_ref()
            .is_some_and(|p| p.span().end == self.at && p.text.eq_ignore_ascii_case("e"))
    }

    fn starts_slash_literal(&self) -> bool {
        self.previous.as_ref().is_none_or(|p| {
            matches!(
                p.text,
                "=" | "~" | "(" | "," | "return" | "when" | "if" | "unless" | "puts" | "print"
            ) || self.source[p.span().end..self.at].contains('\n')
        })
    }
}

fn identifier_width(s: &str, c: char, lang: Language) -> usize {
    let extra = match lang {
        Language::Assembly => '.',
        Language::PlPgSql => '$',
        _ => return identifier(s).max(c.len_utf8()),
    };
    if c != '_' && !c.is_alphabetic() {
        return c.len_utf8();
    }
    s.find(|n: char| n != '_' && n != extra && !n.is_alphanumeric())
        .unwrap_or(s.len())
}

fn quote_context(p: &Token, at: usize) -> bool {
    !matches!(
        p.text,
        "sub" | "package" | ":" | ">" | "$" | "@" | "%" | "&"
    ) && (p.span().end != at || !p.name())
}
