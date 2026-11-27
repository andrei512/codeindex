//! Grammar implementations that recognize raw string prefixes.
use super::super::{
    Kind,
    literals::{cpp_raw_end, csharp_raw_end, rust_literal_end, swift_raw_end},
};
use super::{Grammar, Lexeme, Prefix, literal};

pub(in super::super) struct Cpp;
impl Grammar for Cpp {
    const C_LINE_ENDINGS: bool = true;
    const DIGIT_SEPARATOR: bool = true;
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        if matches!(c, 'R' | 'u' | 'U' | 'L') {
            cpp_raw_end(tail).map(literal)
        } else {
            None
        }
    }
}
pub(in super::super) struct CppLegacy;
impl Grammar for CppLegacy {
    const C_LINE_ENDINGS: bool = true;
    fn prefix(tail: &str, c: char, previous: Option<&str>) -> Prefix {
        Cpp::prefix(tail, c, previous)
    }
}
pub(in super::super) struct Csharp;
impl Grammar for Csharp {
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        if c == '"' {
            csharp_raw_end(tail).map(literal)
        } else {
            None
        }
    }
}
pub(in super::super) struct Swift;
impl Grammar for Swift {
    const NESTED_COMMENTS: bool = true;
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        if matches!(c, '#' | '"') {
            swift_raw_end(tail).map(literal)
        } else {
            None
        }
    }
}
pub(in super::super) struct Rust;
impl Grammar for Rust {
    const NESTED_COMMENTS: bool = true;
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        if !matches!(c, 'b' | 'c' | 'r' | '\'') {
            return None;
        }
        if let Some(end) = rust_literal_end(tail) {
            return Some(literal(end));
        }
        let raw = tail
            .strip_prefix("r#")?
            .strip_prefix(|c: char| c == '_' || c.is_alphabetic())?;
        let end = raw
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map_or(tail.len(), |i| tail.len() - raw.len() + i);
        Some((end, Lexeme::SingleLine(Kind::Word)))
    }
    fn quote_start(c: char, tail: &str) -> Option<usize> {
        let prefix = usize::from(matches!(c, 'b' | 'c'));
        let quote = &tail.as_bytes()[prefix..];
        if quote.starts_with(b"\"") || quote.starts_with(b"'\\") {
            return Some(prefix);
        }
        (c == '`').then_some(0)
    }
}
