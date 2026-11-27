//! Recognize written function definitions even in inactive translation-unit branches.
use super::{Kind, Token, ident};

fn at<'a>(tokens: &[Token<'a>], i: usize) -> &'a str {
    tokens.get(i).map_or("", |t| t.text)
}

/// Parameter opener, body opener, and exclusive end of a definition.
pub(in crate::domain::code) fn written_body(
    t: &[Token],
    pairs: &[usize],
    i: usize,
    cpp: bool,
) -> Option<(usize, usize, usize)> {
    let open = parameter_opener(t, pairs, i, cpp)?;
    let close = pairs[open];
    if close >= t.len() {
        return None;
    }
    let mut body = close + 1;
    while matches!(at(t, body), "const" | "noexcept" | "override" | "final") {
        body += 1;
    }
    if at(t, body) != "{" || pairs[body] >= t.len() {
        return None;
    }
    let end = pairs[body] + 1;
    Some((open, body, end))
}

fn parameter_opener(t: &[Token], pairs: &[usize], name: usize, cpp: bool) -> Option<usize> {
    let [token, next] = t.get(name..name + 2)? else {
        return None;
    };
    if next.text != "(" || !definition_name(token) {
        return None;
    }
    if !declarator_prefix(t, name) {
        return None;
    }
    Some(if cpp && operator_call(t, pairs, name) {
        name + 3
    } else {
        name + 1
    })
}

fn definition_name(token: &Token) -> bool {
    ident(token)
        && !matches!(
            token.text,
            "if" | "for"
                | "while"
                | "switch"
                | "catch"
                | "sizeof"
                | "alignof"
                | "decltype"
                | "noexcept"
                | "requires"
                | "constexpr"
                | "static_assert"
        )
}

fn declarator_prefix(t: &[Token], i: usize) -> bool {
    let previous = &t[i - 1];
    match previous.text {
        "return" | "else" | "do" | "case" | "new" | "define" => false,
        "*" | "&" | ">" => true,
        ":" if i >= 2 && t[i - 2].text == ":" => true,
        _ => previous.kind == Kind::Word,
    }
}

fn operator_call(t: &[Token], pairs: &[usize], i: usize) -> bool {
    t[i].text == "operator" && pairs[i + 1] == i + 2 && at(t, i + 3) == "("
}
