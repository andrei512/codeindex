//! Visit executable interpolation ranges. Escaped delimiters remain literal.
use super::{Analysis, Evidence, Language, reference_chain::References, syntax::paired};
use crate::domain::code::lexer;
fn expression(bytes: &[u8], start: usize, python: bool) -> Option<(std::ops::Range<usize>, usize)> {
    let mut cursor = Expression {
        bytes,
        python,
        depth: 0,
        suffix: None,
    };
    let mut p = start;
    while let Some(&c) = bytes.get(p) {
        if c == b'}' && cursor.depth == 0 {
            return Some((start..cursor.suffix.unwrap_or(p), p + 1));
        }
        p = cursor.advance(p, c);
    }
    None
}

struct Expression<'a> {
    bytes: &'a [u8],
    python: bool,
    depth: usize,
    suffix: Option<usize>,
}

impl Expression<'_> {
    fn advance(&mut self, at: usize, byte: u8) -> usize {
        match byte {
            b'\'' | b'"' | b'`' => {
                return at + lexer::quoted_len(&self.bytes[at..], false, false, |_| {});
            }
            b'(' | b'[' | b'{' => self.depth += 1,
            b')' | b']' | b'}' => self.depth = self.depth.saturating_sub(1),
            b':' | b'!' => self.observe_suffix(at),
            _ => {}
        }
        at + 1
    }

    fn observe_suffix(&mut self, at: usize) {
        let at_root = self.python && self.depth == 0;
        if at_root && self.bytes.get(at + 1) != Some(&b'=') {
            self.suffix.get_or_insert(at);
        }
    }
}

pub(super) fn visit(
    lang: Language,
    site: super::parser::ParserSite,
    prefix: &str,
    (analysis, evidence): (&mut Analysis, &mut Evidence),
) {
    let python = lang == Language::Python;
    let text = site.token.text;
    if !interpolated(text, prefix, python) {
        return;
    }
    for range in interpolations(text, python) {
        expression_uses(lang, site, range, (analysis, evidence));
    }
}

fn interpolated(text: &str, prefix: &str, python: bool) -> bool {
    if python {
        ["f", "rf", "fr"]
            .iter()
            .any(|form| prefix.eq_ignore_ascii_case(form))
    } else {
        text.starts_with('`')
    }
}

fn interpolations(text: &str, python: bool) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let bytes = text.as_bytes();
    let mut p = 0;
    std::iter::from_fn(move || {
        while p < bytes.len() {
            if !interpolation_start(bytes, &mut p, python) {
                continue;
            }
            let (range, next) = expression(bytes, p, python)?;
            p = next;
            return Some(range);
        }
        None
    })
}

fn interpolation_start(bytes: &[u8], p: &mut usize, python: bool) -> bool {
    let at = *p;
    *p += 1;
    let next = bytes.get(*p).copied();
    match (python, bytes[at], next) {
        (true, b'{', Some(b'{')) | (false, b'\\', _) => {
            *p += 1;
            false
        }
        (true, b'{', _) => true,
        (false, b'$', Some(b'{')) => {
            *p += 1;
            true
        }
        _ => false,
    }
}

fn expression_uses(
    lang: Language,
    site: super::parser::ParserSite,
    range: std::ops::Range<usize>,
    (analysis, evidence): (&mut Analysis, &mut Evidence),
) {
    let offset = site.token.span.start + range.start;
    let mut tokens = lexer::tokens(&site.token.text[range], lang);
    for token in &mut tokens {
        token.span.start += offset;
        token.span.end += offset;
    }
    let pairs = paired(&tokens);
    References::new(&tokens, &pairs, lang).expression(
        0..tokens.len(),
        site.scope,
        analysis,
        evidence,
    );
}
