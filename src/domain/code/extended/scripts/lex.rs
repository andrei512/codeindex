//! Literals and comments are opaque. Offsets always refer to the original UTF-8.
use super::Language;
use std::{collections::VecDeque, ops::Range};

#[derive(Clone, Copy)]
pub(in crate::domain::code) struct Token<'a> {
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
    debug_assert_eq!(s.as_bytes()[0], q);
    crate::domain::code::lexer::quoted_len(s.as_bytes(), false, false, |_| {})
}

mod literal;

fn skip_heredocs(source: &str, mut at: usize, pending: &mut VecDeque<(&str, bool)>) -> usize {
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
    at
}

pub(super) fn tokens(source: &str, lang: Language) -> Vec<Token<'_>> {
    tokens_with_comments(source, lang, |_| {})
}

pub(in crate::domain::code) fn tokens_with_comments(
    source: &str,
    lang: Language,
    mut comment: impl FnMut(usize),
) -> Vec<Token<'_>> {
    let mut scanner = Scanner {
        source,
        lang,
        at: 0,
        pending: VecDeque::new(),
        previous: None,
    };
    let mut out = Vec::new();
    while let Some(token) = scanner.next(&mut comment) {
        out.push(token);
    }
    out
}

struct Scanner<'s> {
    source: &'s str,
    lang: Language,
    at: usize,
    pending: VecDeque<(&'s str, bool)>,
    previous: Option<Token<'s>>,
}

impl<'s> Scanner<'s> {
    fn opaque(&self, s: &str, c: char, comment: &mut impl FnMut(usize)) -> Option<usize> {
        let bol = self.at == 0 || self.source.as_bytes()[self.at - 1] == b'\n';
        if bol
            && matches!(self.lang, Language::Perl | Language::Ruby)
            && let Some(n) = document_marker(s, self.lang)
        {
            return Some(n);
        }
        if let Some(n) = comment_width(s, self.lang, c) {
            comment(self.at);
            return Some(n);
        }
        None
    }

    fn skip_whitespace(&mut self, c: char) -> bool {
        if !c.is_whitespace() {
            return false;
        }
        self.at += c.len_utf8();
        if c == '\n' && !self.pending.is_empty() {
            self.at = skip_heredocs(self.source, self.at, &mut self.pending);
        }
        true
    }

    fn skip(&mut self, s: &'s str, c: char, comment: &mut impl FnMut(usize)) -> bool {
        if self.skip_whitespace(c) {
            return true;
        }
        let Some(n) = self.opaque(s, c, comment) else {
            return false;
        };
        self.at += n;
        true
    }

    fn next(&mut self, comment: &mut impl FnMut(usize)) -> Option<Token<'s>> {
        while self.at < self.source.len() {
            let at = self.at;
            let s = &self.source[at..];
            let c = s.chars().next().unwrap();
            if self.skip(s, c, comment) {
                continue;
            }
            let n = self.token_width(s, c);
            let token = Token { at, text: &s[..n] };
            self.previous = Some(token);
            self.at += n;
            return Some(token);
        }
        None
    }
}

/// Interpreter data markers consume the remaining input. Embedded documentation
/// consumes through its closing marker and does not produce comment directives.
fn final_marker(s: &str, lang: Language) -> bool {
    if !s.starts_with("__") {
        return false;
    }
    match s[..line_end(s)].trim_end() {
        "__END__" => true,
        "__DATA__" => lang == Language::Perl,
        _ => false,
    }
}

fn document_close(s: &str, lang: Language) -> Option<&'static str> {
    match lang {
        Language::Perl => s
            .strip_prefix('=')
            .filter(|rest| rest.starts_with(|c: char| c.is_ascii_alphabetic()))
            .map(|_| "\n=cut"),
        Language::Ruby => s
            .strip_prefix("=begin")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(|_| "\n=end"),
        _ => None,
    }
}

fn document_marker(s: &str, lang: Language) -> Option<usize> {
    if final_marker(s, lang) {
        return Some(s.len());
    }
    let closing = document_close(s, lang)?;
    Some(
        s.find(closing)
            .map_or(s.len(), |n| n + 1 + line_end(&s[n + 1..])),
    )
}

fn lua_comment(s: &str) -> Option<usize> {
    let tail = s.strip_prefix("--")?;
    Some(
        crate::domain::code::lexer::bracket_literal(tail)
            .map_or_else(|| line_end(s), |(_, end)| end + 2),
    )
}

fn line_comment(s: &str, lang: Language, c: char) -> bool {
    match lang {
        Language::Perl | Language::Ruby => c == '#',
        Language::PlPgSql => s.starts_with("--"),
        Language::Assembly => matches!(c, ';' | '#' | '@') || s.starts_with("//"),
        _ => false,
    }
}

fn comment_width(s: &str, lang: Language, c: char) -> Option<usize> {
    if lang == Language::Lua {
        return lua_comment(s);
    }
    if line_comment(s, lang, c) {
        return Some(line_end(s));
    }
    if matches!(lang, Language::PlPgSql | Language::Assembly) && s.starts_with("/*") {
        return Some(crate::domain::code::lexer::block_comment_end(
            s.as_bytes(),
            0,
            lang == Language::PlPgSql,
            |_| {},
        ));
    }
    None
}
