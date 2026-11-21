use super::unicode::RANGES;
use std::ops::Range;

const MAX_KEY: usize = 32768;

/// Emits a borrowed, normalized key and the complete span of its original spelling.
/// SQLite caps key *bytes*, including when the cap falls inside a scalar.
pub(super) fn tokenize(text: &str, emit: impl FnMut(&[u8], Range<usize>)) {
    tokenize_with(text, &mut Vec::new(), emit);
}

pub(super) fn tokenize_with(
    text: &str,
    key: &mut Vec<u8>,
    mut emit: impl FnMut(&[u8], Range<usize>),
) {
    key.clear();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let (kind, folded) = if c.is_ascii() {
            (
                u8::from(c.is_ascii_alphanumeric() || c == '_'),
                Some(c.to_ascii_lowercase()),
            )
        } else {
            let scalar = match c as u32 {
                0xfffe | 0xffff => 0xfffd,
                n => n,
            };
            let index = RANGES.partition_point(|&(lower, _, _)| lower <= scalar) - 1;
            let (_, kind, delta) = RANGES[index];
            (kind, char::from_u32((scalar as i32 + delta) as u32))
        };
        if kind == 1 || (kind == 2 && start.is_some()) {
            start.get_or_insert(i);
            if let Some(folded) = folded.filter(|_| key.len() < MAX_KEY) {
                let mut buffer = [0; 4];
                let bytes = folded.encode_utf8(&mut buffer).as_bytes();
                key.extend_from_slice(&bytes[..bytes.len().min(MAX_KEY - key.len())]);
            }
        } else if let Some(begin) = start.take() {
            emit(key, begin..i);
            key.clear();
        }
    }
    if let Some(begin) = start {
        emit(key, begin..text.len());
    }
}

/// Identifier splitting follows Rust's character predicates, independently of
/// unicode61's frozen token character set. Each emitted part borrows the source.
pub(super) fn identifier_parts(text: &str, mut emit: impl FnMut(usize, &str)) {
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let base = word.as_ptr() as usize - text.as_ptr() as usize;
        let mut start = 0;
        let mut previous = ' ';
        let mut chars = word.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c == '_'
                || (c.is_uppercase()
                    && (previous.is_lowercase()
                        || (previous.is_uppercase()
                            && chars.peek().is_some_and(|(_, next)| next.is_lowercase()))))
            {
                if at > start {
                    emit(base + start, &word[start..at]);
                }
                start = at + usize::from(c == '_');
            }
            previous = c;
        }
        if start > 0 && start < word.len() {
            emit(base + start, &word[start..]);
        }
    }
}
