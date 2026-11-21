//! Visit executable interpolation ranges. Escaped delimiters remain literal.
use super::{Analysis, Evidence, Kind, Language, Lookup, Token, add_use, keyword, word};
use crate::domain::code::lexer;
fn closing(bytes: &[u8], mut p: usize, python: bool) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut stop = None;
    let mut quote = 0;
    while p < bytes.len() {
        let c = bytes[p];
        if quote != 0 {
            if c == b'\\' {
                p += 2;
                continue;
            }
            if c == quote {
                quote = 0;
            }
        } else {
            match c {
                b'\'' | b'"' | b'`' => quote = c,
                b'(' | b'[' | b'{' => depth += 1,
                b'}' if depth == 0 => return Some((stop.unwrap_or(p), p + 1)),
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b':' | b'!' if python && depth == 0 && bytes.get(p + 1) != Some(&b'=') => {
                    stop.get_or_insert(p);
                }
                _ => {}
            }
        }
        p += 1;
    }
    None
}
pub(super) fn visit(
    lang: Language,
    token: &Token,
    prefix: &str,
    scope: usize,
    a: &mut Analysis,
    e: &mut Evidence,
) {
    let python = lang == Language::Python;
    if python {
        if !matches!(prefix.to_ascii_lowercase().as_str(), "f" | "rf" | "fr") {
            return;
        }
    } else if !token.text.starts_with('`') {
        return;
    }
    let b = token.text.as_bytes();
    let mut p = 0;
    while p < b.len() {
        let open = if python {
            if b[p] == b'{' && b.get(p + 1) == Some(&b'{') {
                p += 2;
                continue;
            }
            b[p] == b'{'
        } else {
            if b[p] == b'\\' {
                p += 2;
                continue;
            }
            b[p] == b'$' && b.get(p + 1) == Some(&b'{')
        };
        if !open {
            p += 1;
            continue;
        }
        let start = p + if python { 1 } else { 2 };
        let Some((end, next)) = closing(b, start, python) else {
            break;
        };
        let offset = token.span.start + start;
        let mut tokens = lexer::tokens(&token.text[start..end], lang);
        for t in &mut tokens {
            t.span.start += offset;
            t.span.end += offset;
        }
        for (i, t) in tokens.iter().enumerate() {
            if t.kind != Kind::Word || !word(t) || keyword(lang, t.text) {
                continue;
            }
            let member = i > 0 && tokens[i - 1].text == ".";
            let receiver = if member
                && i >= 2
                && word(&tokens[i - 2])
                && !(i > 2 && tokens[i - 3].text == ".")
            {
                Some(tokens[i - 2].span.clone())
            } else {
                None
            };
            if !member || receiver.is_some() {
                add_use(
                    a,
                    e,
                    t,
                    scope,
                    tokens.get(i + 1).is_some_and(|n| n.text == "("),
                    Lookup::Name { scope, receiver },
                );
            }
        }
        p = next;
    }
}
