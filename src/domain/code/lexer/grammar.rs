//! Literal and identifier rules selected once for a source view.
use super::{Kind, literals::regex_end, sql_dollar_string_len};

mod delimited_comments;
mod raw_strings;
pub(super) use delimited_comments::{CMake, Html, Powershell};
pub(super) use raw_strings::{Cpp, CppLegacy, Csharp, Rust, Swift};

pub(super) enum Lexeme {
    Comment,
    Token(Kind),
    SingleLine(Kind),
}
impl Lexeme {
    pub(super) fn observe(
        self,
        text: &str,
        start: usize,
        comment: &mut impl FnMut(usize),
        mut newline: impl FnMut(usize),
    ) -> Option<Kind> {
        if let Self::SingleLine(kind) = self {
            return Some(kind);
        }
        for (offset, _) in text.match_indices('\n') {
            newline(start + offset + 1);
        }
        match self {
            Self::Comment => {
                comment(start);
                None
            }
            Self::Token(kind) | Self::SingleLine(kind) => Some(kind),
        }
    }
}
type Prefix = Option<(usize, Lexeme)>;
fn literal(size: usize) -> (usize, Lexeme) {
    (size, Lexeme::Token(Kind::String))
}

pub(super) trait Grammar {
    const NESTED_COMMENTS: bool = false;
    const C_LINE_ENDINGS: bool = false;
    fn line_comment_width(tail: &str) -> usize {
        let end = if Self::C_LINE_ENDINGS {
            tail.find(['\n', '\r'])
        } else {
            tail.find('\n')
        };
        end.unwrap_or(tail.len())
    }
    const DASH_IN_WORD: bool = false;
    const DIGIT_SEPARATOR: bool = false;
    const TRIPLE_QUOTES: bool = false;
    fn prefix(_tail: &str, _c: char, _previous: Option<&str>) -> Prefix {
        None
    }
    fn quote_start(c: char, _tail: &str) -> Option<usize> {
        matches!(c, '\'' | '"' | '`').then_some(0)
    }
}

pub(super) struct Ordinary;
impl Grammar for Ordinary {}
pub(super) struct Css;
impl Grammar for Css {
    const DASH_IN_WORD: bool = true;
}
pub(super) struct Python;
impl Grammar for Python {
    const TRIPLE_QUOTES: bool = true;
}
pub(super) struct Sql;
impl Grammar for Sql {
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        if c == '$' {
            sql_dollar_string_len(tail).map(literal)
        } else {
            None
        }
    }
}
pub(super) struct C;
impl Grammar for C {
    const C_LINE_ENDINGS: bool = true;
}
pub(super) struct C23;
impl Grammar for C23 {
    const C_LINE_ENDINGS: bool = true;
    const DIGIT_SEPARATOR: bool = true;
}
pub(super) struct Javascript;
impl Grammar for Javascript {
    fn prefix(tail: &str, c: char, previous: Option<&str>) -> Prefix {
        if c != '/' {
            return None;
        }
        if matches!(tail.as_bytes().get(1), Some(b'/' | b'*')) {
            return None;
        }
        if previous.is_none_or(|p| {
            matches!(
                p,
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
        }) {
            regex_end(tail.as_bytes(), 1).map(|end| (end, Lexeme::SingleLine(Kind::String)))
        } else {
            None
        }
    }
}
