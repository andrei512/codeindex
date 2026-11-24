//! These languages differ in raw strings, multiline strings and embedded markup.
//! Keep literals opaque, including interpolation, to avoid synthesizing code.
use super::{Kind, Language, Token};

fn heredoc_end(text: &str) -> Option<usize> {
    let header = text.strip_prefix("<<<")?;
    let newline = header.find('\n')?;
    let label = header[..newline].trim().trim_matches(['\'', '"']);
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b == b'_' || b.is_ascii_alphanumeric())
    {
        return None;
    }
    let mut offset = 3 + newline + 1;
    for line in text[offset..].split_inclusive('\n') {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if let Some(rest) = trimmed.strip_prefix(label)
            && !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            return Some(offset + line.len() - trimmed.len() + label.len());
        }
        offset += line.len();
    }
    Some(text.len())
}

pub(super) fn scan(source: &str, lang: Language) -> Vec<Token<'_>> {
    let php = matches!(lang, Language::Php | Language::Hack);
    let mut in_code = lang != Language::Php;
    let mut tokens = Vec::new();
    let (mut i, mut line) = (
        if lang == Language::Hack {
            super::hack_header_len(source).unwrap_or(0)
        } else {
            0
        },
        1,
    );
    let b = source.as_bytes();
    while i < b.len() {
        let start = i;
        let tail = &source[start..];
        let mut kind = None;
        if !in_code {
            let Some(skip) = tail.find("<?") else { break };
            i += skip + 2;
            if source[i..].starts_with("php") {
                i += 3;
            } else if source[i..].starts_with("hh") {
                i += 2;
            }
            in_code = true;
        } else if lang == Language::Php && tail.starts_with("?>") {
            i += 2;
            in_code = false;
        } else if tail.starts_with("//") || php && tail.starts_with('#') && !tail.starts_with("#[")
        {
            let end = tail.find('\n').unwrap_or(tail.len());
            i += if lang == Language::Php {
                tail[..end].find("?>").unwrap_or(end)
            } else {
                end
            };
        } else if tail.starts_with("/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else if matches!(lang, Language::Kotlin | Language::Dart)
                    && b[i..].starts_with(b"/*")
                {
                    depth += 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if php
            && tail.starts_with("<<<")
            && let Some(n) = heredoc_end(tail)
        {
            i += n;
            kind = Some(Kind::String);
        } else {
            let c = tail.chars().next().unwrap();
            i += c.len_utf8();
            if matches!(c, '\'' | '"' | '`') {
                let q = c as u8;
                let three = matches!(lang, Language::Kotlin | Language::Dart | Language::Java)
                    && b[start..].starts_with(&[q; 3]);
                let delim = if three { 3 } else { 1 };
                i = start + delim;
                let raw = (q == b'`' && lang == Language::Go)
                    || (three && lang == Language::Kotlin)
                    || (lang == Language::Dart
                        && start > 0
                        && b[start - 1] == b'r'
                        && (start < 2
                            || !(b[start - 2].is_ascii_alphanumeric() || b[start - 2] == b'_')));
                while i < b.len() {
                    if b[i..].starts_with(&[q; 3][..delim]) {
                        i += delim;
                        break;
                    }
                    if b[i] == b'\\' && !raw && i + 1 < b.len() {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                kind = Some(Kind::String);
            } else if c.is_alphanumeric() || c == '_' || c == '$' {
                while i < b.len() {
                    let n = source[i..].chars().next().unwrap();
                    if !(n.is_alphanumeric() || n == '_' || n == '$') {
                        break;
                    }
                    i += n.len_utf8();
                }
                kind = Some(Kind::Word);
            } else if !c.is_whitespace() {
                kind = Some(Kind::Punct);
            }
        }
        if let Some(kind) = kind {
            tokens.push(Token {
                kind,
                text: &source[start..i],
                span: start..i,
                line,
            });
        }
        line += b[start..i].iter().filter(|&&c| c == b'\n').count();
    }
    tokens
}
