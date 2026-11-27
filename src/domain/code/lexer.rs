//! Language-configured lexical scanner. Token text remains in the captured source.
use super::Language;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Word,
    String,
    Punct,
}

#[derive(Clone)]
pub(crate) struct Token<'a> {
    pub text: &'a str,
    pub span: Range<usize>,
    pub line: usize,
    pub kind: Kind,
}

mod grammar;
mod literals;
pub(super) use literals::{bracket_literal, cpp_raw_body, dollar_content, sql_dollar_string_len};

#[derive(Clone, Copy)]
pub(super) enum LineComments {
    None,
    Slash,
    Hash,
    Dash,
    SlashOrHash,
}
impl LineComments {
    pub(super) fn starts(self, tail: &str) -> bool {
        match self {
            Self::None => false,
            Self::Slash => tail.starts_with("//"),
            Self::Hash => tail.starts_with('#'),
            Self::Dash => tail.starts_with("--"),
            Self::SlashOrHash => tail.starts_with("//") || tail.starts_with('#'),
        }
    }
}
#[derive(Clone, Copy)]
struct Comments {
    line: LineComments,
    block: bool,
}

impl Comments {
    fn for_language(lang: Language) -> Self {
        Self {
            line: lang.comment_style(),
            block: lang.block_comments(),
        }
    }

    fn consume<G: grammar::Grammar>(
        self,
        source: &str,
        start: usize,
        comment: &mut impl FnMut(usize),
        newline: impl FnMut(usize),
    ) -> Option<usize> {
        let tail = &source[start..];
        if self.line.starts(tail) {
            comment(start);
            return Some(start + G::line_comment_width(tail));
        }
        if self.block && tail.starts_with("/*") {
            comment(start);
            return Some(block_comment_end(
                source.as_bytes(),
                start,
                G::NESTED_COMMENTS,
                newline,
            ));
        }
        None
    }
}
/// Compile-command-dependent token boundaries. Header buffers can be shared
/// exactly when both their language and preprocessing number grammar agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Mode {
    pub(super) language: Language,
    pub(super) digit_separators: bool,
}
impl Mode {
    pub(super) fn written(language: Language) -> Self {
        Self {
            language,
            digit_separators: language == Language::Cpp,
        }
    }
    pub(super) fn tokens(self, source: &str) -> Vec<Token<'_>> {
        self.scan(source, |_| {})
    }
    fn scan(self, source: &str, mut comment: impl FnMut(usize)) -> Vec<Token<'_>> {
        let mut state = ScanState::new(self);
        state.advance(source, source.len(), &mut comment, &mut |_| {});
        state.tokens
    }
}
pub(super) fn tokens(source: &str, lang: Language) -> Vec<Token<'_>> {
    Mode::written(lang).tokens(source)
}
pub(super) fn tokens_with_comments(
    source: &str,
    lang: Language,
    comment: impl FnMut(usize),
) -> Vec<Token<'_>> {
    Mode::written(lang).scan(source, comment)
}
impl From<Language> for Mode {
    fn from(lang: Language) -> Self {
        Self::written(lang)
    }
}
impl<'a> ScanState<'a> {
    pub(crate) fn advance(
        &mut self,
        source: &'a str,
        bytes: usize,
        comment: &mut impl FnMut(usize),
        newline: &mut impl FnMut(usize),
    ) -> bool {
        let mode = self.mode;
        let end = self.offset.saturating_add(bytes);
        use Language::*;
        match (mode.language, mode.digit_separators) {
            (C, true) => self.resume::<grammar::C23>(source, end, comment, newline),
            (C, false) => self.resume::<grammar::C>(source, end, comment, newline),
            (Cpp, true) => self.resume::<grammar::Cpp>(source, end, comment, newline),
            (Cpp, false) => self.resume::<grammar::CppLegacy>(source, end, comment, newline),
            (Rust, _) => self.resume::<grammar::Rust>(source, end, comment, newline),
            (Swift, _) => self.resume::<grammar::Swift>(source, end, comment, newline),
            _ => self.resume_other(source, end, comment, newline),
        }
    }

    #[inline(always)]
    fn resume_other(
        &mut self,
        source: &'a str,
        end: usize,
        comment: &mut impl FnMut(usize),
        newline: &mut impl FnMut(usize),
    ) -> bool {
        use Language::*;
        match self.mode.language {
            Powershell => self.resume::<grammar::Powershell>(source, end, comment, newline),
            CMake => self.resume::<grammar::CMake>(source, end, comment, newline),
            Csharp => self.resume::<grammar::Csharp>(source, end, comment, newline),
            Python => self.resume::<grammar::Python>(source, end, comment, newline),
            Js => self.resume::<grammar::Javascript>(source, end, comment, newline),
            _ => self.resume_document(source, end, comment, newline),
        }
    }

    #[inline(always)]
    fn resume_document(
        &mut self,
        source: &'a str,
        end: usize,
        comment: &mut impl FnMut(usize),
        newline: &mut impl FnMut(usize),
    ) -> bool {
        use Language::*;
        match self.mode.language {
            Html => self.resume::<grammar::Html>(source, end, comment, newline),
            Css => self.resume::<grammar::Css>(source, end, comment, newline),
            Sql => self.resume::<grammar::Sql>(source, end, comment, newline),
            _ => self.resume::<grammar::Ordinary>(source, end, comment, newline),
        }
    }
}

pub(crate) struct ScanState<'a> {
    mode: Mode,
    comments: Comments,
    offset: usize,
    line: usize,
    pub(super) tokens: Vec<Token<'a>>,
}

impl ScanState<'_> {
    pub(crate) fn new(mode: impl Into<Mode>) -> Self {
        let mode = mode.into();
        Self {
            mode,
            comments: Comments::for_language(mode.language),
            offset: 0,
            line: 1,
            tokens: Vec::new(),
        }
    }
}

impl<'a> ScanState<'a> {
    /// Stops only between lexemes. A literal, escaped line, or nested block
    /// comment is consumed atomically even if it crosses the soft byte limit.
    fn resume<G: grammar::Grammar>(
        &mut self,
        source: &'a str,
        limit: usize,
        comment: &mut impl FnMut(usize),
        newline: &mut impl FnMut(usize),
    ) -> bool {
        let bytes = source.as_bytes();
        let mut i = self.offset;
        let mut line = self.line;
        let result = &mut self.tokens;
        let stop = limit.min(bytes.len());
        while i < stop {
            let start = i;
            let start_line = line;
            let mut next_line = |at| {
                line += 1;
                newline(at);
            };
            let tail = &source[start..];
            let c = tail.chars().next().unwrap();
            i += c.len_utf8();
            if whitespace(c, i, &mut next_line) {
                continue;
            }
            let lexeme = LexemeStart {
                source,
                start,
                first: c,
                previous: result.last().map(|token| token.text),
            };
            let (end, kind) = self.comments.lexeme::<G>(lexeme, comment, next_line);
            i = end;
            push_token(result, source, start..i, (start_line, kind));
        }
        self.offset = i;
        self.line = line;
        i == source.len()
    }
}

#[inline]
fn push_token<'a>(
    out: &mut Vec<Token<'a>>,
    source: &'a str,
    span: Range<usize>,
    (line, kind): (usize, Option<Kind>),
) {
    if let Some(kind) = kind {
        out.push(Token {
            text: &source[span.clone()],
            span,
            line,
            kind,
        });
    }
}

struct LexemeStart<'a> {
    source: &'a str,
    start: usize,
    first: char,
    previous: Option<&'a str>,
}

impl Comments {
    #[inline]
    fn lexeme<G: grammar::Grammar>(
        self,
        token: LexemeStart<'_>,
        comment: &mut impl FnMut(usize),
        mut newline: impl FnMut(usize),
    ) -> (usize, Option<Kind>) {
        let LexemeStart {
            source,
            start,
            first,
            previous,
        } = token;
        let tail = &source[start..];
        if let Some((width, lexeme)) = G::prefix(tail, first, previous) {
            let kind = lexeme.observe(&tail[..width], start, comment, newline);
            return (start + width, kind);
        }
        if let Some(end) = self.consume::<G>(source, start, comment, &mut newline) {
            return (end, None);
        }
        let (end, kind) = ordinary_token::<G>(source, start, first, newline);
        (end, Some(kind))
    }
}

#[inline]
fn whitespace(c: char, end: usize, newline: &mut impl FnMut(usize)) -> bool {
    if c == '\n' {
        newline(end);
        return true;
    }
    c.is_whitespace()
}

fn observe_newline(is_newline: bool, pos: usize, mut newline: impl FnMut(usize)) {
    if is_newline {
        newline(pos);
    }
}

fn ordinary_token<G: grammar::Grammar>(
    source: &str,
    start: usize,
    first: char,
    newline: impl FnMut(usize),
) -> (usize, Kind) {
    let next = start + first.len_utf8();
    let tail = &source[start..];
    if let Some(prefix) = G::quote_start(first, tail) {
        let at = start + prefix;
        let bytes = source.as_bytes();
        (quoted_end::<G>(bytes, at, newline), Kind::String)
    } else if word_char(first) {
        (word_end::<G>(source, next, first), Kind::Word)
    } else {
        (next, Kind::Punct)
    }
}

#[inline]
fn quoted_end<G: grammar::Grammar>(
    bytes: &[u8],
    at: usize,
    mut newline: impl FnMut(usize),
) -> usize {
    if G::C_LINE_ENDINGS {
        return native_quoted_end(bytes, at, newline);
    }
    at + quoted_len(&bytes[at..], G::TRIPLE_QUOTES, false, |end| {
        newline(at + end)
    })
}

fn word_end<G: grammar::Grammar>(source: &str, start: usize, first: char) -> usize {
    let numeric = G::DIGIT_SEPARATOR && first.is_ascii_digit();
    let mut at = start;
    for ch in source[start..].chars() {
        if !word_continues::<G>(source, at, ch, numeric) {
            break;
        }
        at += ch.len_utf8();
    }
    at
}

#[inline]
fn word_continues<G: grammar::Grammar>(source: &str, at: usize, next: char, numeric: bool) -> bool {
    match next {
        // C-family preprocessing numbers admit an identifier character
        // after an apostrophe. The native integer reader checks its radix.
        '\'' if numeric => source[at + 1..].starts_with(word_char),
        '-' => G::DASH_IN_WORD,
        _ => word_char(next),
    }
}

pub(super) fn word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$')
}

pub(super) fn block_comment_end(
    bytes: &[u8],
    start: usize,
    nested: bool,
    mut newline: impl FnMut(usize),
) -> usize {
    let mut i = start + 2;
    let mut depth = 1;
    while depth > 0 && i < bytes.len() {
        match &bytes[i..] {
            [b'*', b'/', ..] => {
                depth -= 1;
                i += 2;
            }
            [b'/', b'*', ..] if nested => {
                depth += 1;
                i += 2;
            }
            [b'\n', ..] => {
                i += 1;
                newline(i);
            }
            _ => i += 1,
        }
    }
    i
}

pub(super) fn quoted_len(
    bytes: &[u8],
    allow_triple: bool,
    raw: bool,
    newline: impl FnMut(usize),
) -> usize {
    if allow_triple && bytes.starts_with(&[bytes[0]; 3]) {
        quote_body::<3>(bytes, raw, newline)
    } else {
        quote_body::<1>(bytes, raw, newline)
    }
}
fn quote_body<const WIDTH: usize>(
    bytes: &[u8],
    raw: bool,
    mut newline: impl FnMut(usize),
) -> usize {
    let delimiter = [bytes[0]; WIDTH];
    let mut pos = WIDTH;
    while let Some(&byte) = bytes.get(pos) {
        if bytes[pos..].starts_with(&delimiter) {
            return pos + WIDTH;
        }
        pos += usize::from(byte == b'\\' && !raw);
        observe_newline(bytes.get(pos) == Some(&b'\n'), pos + 1, &mut newline);
        pos += 1;
    }
    bytes.len()
}

// Translation removes a physical continuation before the C grammar
// interprets escapes, including the last slash in an even-length run.
fn native_quoted_end(bytes: &[u8], start: usize, mut newline: impl FnMut(usize)) -> usize {
    let quote = bytes[start];
    let mut escaped = false;
    let mut pos = start + 1;
    while pos < bytes.len() {
        match native_quote_byte(bytes, pos, (quote, escaped), &mut newline) {
            QuoteByte::End(end) => return end,
            QuoteByte::Splice(end) => pos = end,
            QuoteByte::Ordinary(next_escaped) => {
                escaped = next_escaped;
                pos += 1;
            }
        }
    }
    pos
}
enum QuoteByte {
    End(usize),
    Splice(usize),
    Ordinary(bool),
}

#[inline]
fn native_quote_byte(
    bytes: &[u8],
    pos: usize,
    (quote, escaped): (u8, bool),
    newline: &mut impl FnMut(usize),
) -> QuoteByte {
    let byte = bytes[pos];
    match byte {
        b'\r' | b'\n' => QuoteByte::End(pos),
        byte if byte == quote && !escaped => QuoteByte::End(pos + 1),
        b'\\' => splice_end(bytes, pos + 1, newline)
            .map_or(QuoteByte::Ordinary(!escaped), QuoteByte::Splice),
        _ => QuoteByte::Ordinary(false),
    }
}

fn splice_end(bytes: &[u8], mut pos: usize, newline: &mut impl FnMut(usize)) -> Option<usize> {
    while matches!(bytes.get(pos), Some(b' ' | b'\t')) {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'\r') {
        pos += 1;
        if bytes.get(pos) != Some(&b'\n') {
            return Some(pos);
        }
    } else if bytes.get(pos) != Some(&b'\n') {
        return None;
    }
    newline(pos + 1);
    Some(pos + 1)
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    #[test]
    fn c_physical_splices_precede_escape_pairing_in_a_string() {
        for newline in ["\n", "\r\n", "\r"] {
            for count in 1..=4 {
                let literal = format!("\"prefix{}{}n\"", "\\".repeat(count), newline);
                let text = format!("{literal} next");
                let t = tokens(&text, Language::C);
                assert_eq!(
                    t.iter().map(|t| t.text).collect::<Vec<_>>(),
                    [literal.as_str(), "next"]
                );
                assert_eq!(t[1].line, 1 + usize::from(newline.ends_with('\n')));
            }
        }
    }

    #[test]
    fn resumption_preserves_offsets_comments_and_js_regex_context() {
        let text = "let π = /a\\//g; /* one\n two */ x / 2;\n// e\nconst y='🙂';";
        let mut comments = Vec::new();
        let all = tokens_with_comments(text, Language::Js, |at| comments.push(at));
        for chunk in 1..=text.len() {
            let mut state = ScanState::new(Language::Js);
            let mut actual = Vec::new();
            while !state.resume::<grammar::Javascript>(
                text,
                state.offset + chunk,
                &mut |at| actual.push(at),
                &mut |_| {},
            ) {}
            assert_eq!(actual, comments);
            assert_tokens_equal(&state.tokens, &all);
        }
    }

    fn assert_tokens_equal(actual: &[Token], expected: &[Token]) {
        assert_eq!(actual.len(), expected.len());
        for (a, b) in actual.iter().zip(expected) {
            assert!(a.kind == b.kind);
            assert_eq!((&a.span, a.line, a.text), (&b.span, b.line, b.text));
        }
    }

}
