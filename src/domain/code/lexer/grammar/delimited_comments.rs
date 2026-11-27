//! Grammars with language-specific block comment delimiters.
use super::super::bracket_literal;
use super::{Grammar, Lexeme, Prefix, literal};

pub(in super::super) struct Html;
impl Grammar for Html {
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        (c == '<' && tail.starts_with("<!--")).then(|| {
            (
                tail.find("-->").map_or(tail.len(), |i| i + 3),
                Lexeme::Comment,
            )
        })
    }
}
pub(in super::super) struct CMake;
impl Grammar for CMake {
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        match c {
            '#' => bracket_literal(&tail[1..]).map(|(_, end)| (end + 1, Lexeme::Comment)),
            '[' => bracket_literal(tail).map(|(_, end)| literal(end)),
            _ => None,
        }
    }
}
pub(in super::super) struct Powershell;
impl Grammar for Powershell {
    const DASH_IN_WORD: bool = true;
    fn prefix(tail: &str, c: char, _: Option<&str>) -> Prefix {
        if c == '<' && tail.starts_with("<#") {
            return Some((
                tail.find("#>").map_or(tail.len(), |i| i + 2),
                Lexeme::Comment,
            ));
        }
        if c != '@' {
            return None;
        }
        let &q = tail
            .as_bytes()
            .get(1)
            .filter(|q| matches!(q, b'\'' | b'"'))?;
        let (header, body) = tail.split_once('\n')?;
        if !header[2..].trim().is_empty() {
            return None;
        }
        let mut end = header.len() + 1;
        for line in body.split_inclusive('\n') {
            if line.as_bytes().starts_with(&[q, b'@']) {
                return Some(literal(end + 2));
            }
            end += line.len();
        }
        Some(literal(end))
    }
}
