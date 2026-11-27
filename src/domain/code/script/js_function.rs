//! Anonymous functions and object or class methods introduce a receiver scope
//! at the same point in their signatures. Named declarations bind separately.
use super::{
    CallableSignature, Language, NameKind, SignatureEnd, Token, angle_ends::AngleEnds, tok, word,
};
use std::ops::Range;

pub(super) struct JsFunction {
    pub(super) name: Option<Range<usize>>,
    pub(super) signature: CallableSignature,
}

impl JsFunction {
    pub(super) fn parse(
        site: super::parser::ParserSite,
        pairs: &[usize],
        angles: &mut AngleEnds<'_, '_>,
    ) -> Option<Self> {
        let t = angles.tokens;
        let (token, at, previous) = (site.token, site.index, site.previous);
        let (head, name) = if token.text == "function" && previous != "." {
            (at + usize::from(tok(t, at + 1) == "*"), None)
        } else {
            let (head, span) = method_name(t, pairs, site)?;
            (head, Some(span))
        };
        let paren = angles.arguments(head + 1)?;
        let close = pairs[paren];
        let body = SignatureEnd::parse(angles, pairs, close).body()?;
        Some(Self {
            name,
            signature: CallableSignature {
                head,
                parentheses: (paren, close),
                body,
            },
        })
    }
}

fn method_name(
    t: &[Token],
    pairs: &[usize],
    site: super::parser::ParserSite,
) -> Option<(usize, Range<usize>)> {
    let (at, previous) = (site.index, site.previous);
    if !method_prefix(t, at, previous) {
        return None;
    }
    let head = method_name_end(t, pairs, at)?;
    let start = t[at - usize::from(previous == "#")].span.start;
    Some((head, start..t.get(head)?.span.end))
}

fn method_name_end(t: &[Token], pairs: &[usize], at: usize) -> Option<usize> {
    let token = t.get(at)?;
    if token.text == "[" {
        return pairs.get(at).copied().filter(|&end| end < t.len());
    }
    (word(token)
        && (NameKind::of(Language::Js, token.text).identifier() || token.text == "constructor"))
        .then_some(at)
}

// Contextual modifiers precede a method name only at a declaration head.
// Elsewhere get, set and async are ordinary expression operands.
pub(super) fn method_modifier(t: &[Token], pairs: &[usize], at: usize) -> bool {
    if !method_prefix(t, at, tok(t, at.wrapping_sub(1))) {
        return false;
    }
    let first = at + 1;
    let name = first + usize::from(matches!(tok(t, first), "*" | "#"));
    let Some(head) = method_name_end(t, pairs, name) else {
        return false;
    };
    let paren = match tok(t, head + 1) {
        "(" => head + 1,
        "<" => super::angle_end(t, head + 1).map_or(t.len(), |close| close + 1),
        _ => return false,
    };
    tok(t, paren) == "("
        && pairs
            .get(paren)
            .is_some_and(|&close| matches!(tok(t, close.saturating_add(1)), "{" | ":"))
}

fn method_prefix(t: &[Token], at: usize, previous: &str) -> bool {
    match previous {
        "*" => at < 2 || tok(t, at - 2) != "function",
        "{" | "}" | "," | ";" | "async" | "static" | "get" | "set" | "public" | "private"
        | "protected" | "override" | "#" => true,
        _ => false,
    }
}
