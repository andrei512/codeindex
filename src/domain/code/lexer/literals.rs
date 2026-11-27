//! Source widths for literals whose closing delimiter depends on the language.
use std::ops::Range;

// At the start of an expression ECMAScript reads a regexp literal. Keep its
// pattern opaque so words and parentheses inside it never become declarations.
pub(super) fn regex_end(bytes: &[u8], pos: usize) -> Option<usize> {
    let mut class = false;
    let mut pattern = bytes[pos..].iter().copied().enumerate();
    while let Some((at, byte)) = pattern.next() {
        match byte {
            b'\n' | b'\r' => return None,
            b'\\' => {
                pattern
                    .next()
                    .filter(|&(_, escaped)| !matches!(escaped, b'\n' | b'\r'))?;
            }
            b'[' | b']' => class = byte == b'[',
            b'/' if !class => {
                let flags = pos + at + 1;
                let count = bytes[flags..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
                return Some(flags + count);
            }
            _ => {}
        }
    }
    None
}

// PostgreSQL dollar quotes use matching case-sensitive tags. An unfinished
// literal consumes the rest of the file, including during an editor update.
pub(in crate::domain::code) fn sql_dollar_string_len(text: &str) -> Option<usize> {
    let delimiter = dollar_delimiter(text)?;
    let tag = &delimiter[1..delimiter.len() - 1];
    if !tag.is_empty()
        && (!tag.starts_with(|c: char| c == '_' || c.is_alphabetic())
            || !tag.chars().all(|c| c == '_' || c.is_alphanumeric()))
    {
        return None;
    }
    Some(
        text[delimiter.len()..]
            .find(delimiter)
            .map_or(text.len(), |n| n + delimiter.len() * 2),
    )
}

fn dollar_delimiter(text: &str) -> Option<&str> {
    let width = text.strip_prefix('$')?.find('$')? + 2;
    Some(&text[..width])
}
pub(in crate::domain::code) fn dollar_content(text: &str) -> Option<Range<usize>> {
    let width = dollar_delimiter(text)?.len();
    (text.len() >= 2 * width && text.ends_with(&text[..width])).then(|| width..text.len() - width)
}

pub(super) fn csharp_raw_end(tail: &str) -> Option<usize> {
    let quotes = tail.bytes().take_while(|b| *b == b'"').count();
    if quotes < 3 {
        return None;
    }
    Some(
        tail[quotes..]
            .find(&tail[..quotes])
            .map_or(tail.len(), |at| at + 2 * quotes),
    )
}

pub(in crate::domain::code) fn cpp_raw_body(tail: &str) -> Option<&str> {
    tail.strip_prefix("u8")
        .or_else(|| tail.strip_prefix(['u', 'U', 'L']))
        .unwrap_or(tail)
        .strip_prefix("R\"")
}

pub(super) fn cpp_raw_end(tail: &str) -> Option<usize> {
    let raw = cpp_raw_body(tail)?;
    let end = raw.bytes().take(17).position(|b| b == b'(')?;
    let delimiter = &raw[..end];
    let body = &raw[end + 1..];
    if delimiter
        .bytes()
        .any(|b| b <= b' ' || matches!(b, b')' | b'\\'))
    {
        return None;
    }
    let offset = tail.len() - body.len();
    Some(RawClose { first: ')', marker: delimiter, trailer: "\"" }.end(tail, offset))
}

pub(super) fn swift_raw_end(tail: &str) -> Option<usize> {
    let hashes = tail.bytes().take_while(|b| *b == b'#').count();
    let raw = &tail[hashes..];
    let quotes = if raw.starts_with("\"\"\"") {
        3
    } else if hashes > 0 && raw.starts_with('"') {
        1
    } else {
        return None;
    };
    let closing = &raw[..quotes];
    let marker = &tail[..hashes];
    let mut at = hashes + quotes;
    while let Some(offset) = tail[at..].find(['\\', '"']) {
        at += offset;
        let rest = &tail[at..];
        match swift_step(rest, closing, marker) {
            Some(width) => at += width,
            None => return Some(at + quotes + hashes),
        }
    }
    return Some(tail.len());
}

fn swift_step(rest: &str, closing: &str, marker: &str) -> Option<usize> {
    if let Some(width) = swift_escape(rest, marker) {
        return Some(width);
    }
    let end = rest
        .strip_prefix(closing)
        .is_some_and(|tail| tail.starts_with(marker));
    (!end).then_some(1)
}

fn swift_escape(rest: &str, marker: &str) -> Option<usize> {
    let escaped = rest.strip_prefix('\\')?.strip_prefix(marker)?;
    let character = escaped.chars().next()?;
    Some(1 + marker.len() + character.len_utf8())
}

/// CMake and Lua use the same bracket delimiter. Return the opener width and
/// exclusive end, including an unterminated body through end of input.
pub(in crate::domain::code) fn bracket_literal(s: &str) -> Option<(usize, usize)> {
    let r = s.strip_prefix('[')?;
    let n = r.bytes().take_while(|&c| c == b'=').count();
    let body = r.get(n..)?.strip_prefix('[')?;
    if n == 0 {
        return Some((2, body.find("]]").map_or(s.len(), |i| i + 4)));
    }
    let Some(first) = body.find(']') else {
        return Some((n + 2, s.len()));
    };
    let equals = &r[..n];
    let after = &body[first + 1..];
    if after
        .strip_prefix(equals)
        .is_some_and(|tail| tail.starts_with(']'))
    {
        return Some((n + 2, first + 2 * (n + 2)));
    }
    // A false closing bracket can repeat many times in literal text. Use
    // the full delimiter search for the remaining bytes in that case.
    let marker = format!("]{equals}]");
    let end = after
        .find(&marker)
        .map_or(s.len(), |i| first + 1 + i + 2 * marker.len());
    Some((n + 2, end))
}

// C++ raw delimiters and Rust hash runs are compared directly against
// their source spelling. No closing string is allocated, even on a miss.
struct RawClose<'a> {
    first: char,
    marker: &'a str,
    trailer: &'a str,
}

impl RawClose<'_> {
    fn end(&self, text: &str, start: usize) -> usize {
        for (offset, _) in text[start..].match_indices(self.first) {
            let mark = start + offset + self.first.len_utf8();
            if text[mark..].strip_prefix(self.marker).is_some_and(|tail| tail.starts_with(self.trailer)) {
                return mark + self.marker.len() + self.trailer.len();
            }
        }
        text.len()
    }
}

pub(super) fn rust_literal_end(text: &str) -> Option<usize> {
    let raw = text
        .strip_prefix("br")
        .or_else(|| text.strip_prefix("cr"))
        .or_else(|| text.strip_prefix('r'));
    if let Some(raw) = raw {
        let body = raw.trim_start_matches('#');
        if body.starts_with('"') {
            let marker = &raw[..raw.len() - body.len()];
            let delimiter = RawClose { first: '"', marker, trailer: "" };
            return Some(delimiter.end(text, text.len() - body.len() + 1));
        }
    }
    let quoted = text
        .strip_prefix('b')
        .or_else(|| text.strip_prefix('c'))
        .unwrap_or(text);
    // Lifetimes are not strings. A plain character literal is one scalar
    // followed by a quote; escapes use the regular quote scanner.
    let body = quoted.strip_prefix('\'')?;
    let ch = body.chars().next()?;
    let end = text.len() - body.len() + ch.len_utf8();
    (ch != '\\' && text.as_bytes().get(end) == Some(&b'\'')).then_some(end + 1)
}
