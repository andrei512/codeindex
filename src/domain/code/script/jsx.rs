//! JSX text and attribute spellings are outside lexical JavaScript lookup.
//! Expression containers retain their normal tokens.
use super::{Token, tok, word};
use std::{collections::HashMap, ops::Range};
#[derive(Default)]
pub(super) struct Markup {
    ignored: Vec<u64>,
    pub props: HashMap<usize, Range<usize>>,
}
impl Markup {
    pub(super) fn ignored(&self, token: usize) -> bool {
        self.ignored
            .get(token / 64)
            .is_some_and(|word| word & (1u64 << (token % 64)) != 0)
    }
    fn ignore(&mut self, token: usize) {
        let word = token / 64;
        if self.ignored.len() <= word {
            self.ignored.resize(word + 1, 0);
        }
        self.ignored[word] |= 1u64 << (token % 64);
    }
}
pub(super) fn classify(t: &[Token], end: &[usize]) -> Markup {
    let mut out = Markup::default();
    let mut depth = 0usize;
    let mut expressions = Vec::new();
    let mut attributes = Vec::new();
    let mut code = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if expressions.last().is_some_and(|&(stop, _)| i == stop) {
            depth = expressions.pop().unwrap().1;
            i += 1;
            continue;
        }
        if depth > 0 && tok(t, i) == "{" && end[i] < t.len() {
            expressions.push((end[i], depth));
            depth = 0;
            i += 1;
            continue;
        }
        if tok(t, i) != "<" {
            if depth > 0 {
                out.ignore(i);
            }
            i += 1;
            continue;
        }
        let closing = tok(t, i + 1) == "/";
        if depth == 0
            && (closing
                || i > 0
                    && !matches!(
                        tok(t, i - 1),
                        "(" | "[" | "," | "=" | "return" | ":" | ">" | "&" | "|" | "{" | "?" | ";"
                    ))
        {
            i += 1;
            continue;
        }
        let name = i + 1 + usize::from(closing);
        let fragment = tok(t, name) == ">";
        if !fragment && !t.get(name).is_some_and(word) {
            i += 1;
            continue;
        }
        let mut p = name + usize::from(!fragment);
        while matches!(tok(t, p), "." | "-" | ":") && t.get(p + 1).is_some_and(word) {
            p += 2;
        }
        let component = !fragment && tok(t, name).starts_with(char::is_uppercase) && p == name + 1;
        attributes.clear();
        code.clear();
        while p < t.len() && !matches!(tok(t, p), ">" | "/") {
            if tok(t, p) == "{" && end[p] < t.len() {
                code.push(p..end[p] + 1);
                p = end[p] + 1;
                continue;
            }
            if !word(&t[p]) {
                break;
            }
            let left = p;
            p += 1;
            while matches!(tok(t, p), "-" | ":") && t.get(p + 1).is_some_and(word) {
                p += 2;
            }
            if p == left + 1 {
                attributes.push(left);
            }
            if tok(t, p) == "=" {
                p += 1;
                if tok(t, p) == "{" && end[p] < t.len() {
                    code.push(p..end[p] + 1);
                    p = end[p] + 1;
                } else if t.get(p).is_some_and(|x| x.kind == super::Kind::String) {
                    p += 1;
                } else {
                    break;
                }
            }
        }
        let self_closed = tok(t, p) == "/";
        p += usize::from(self_closed);
        if tok(t, p) != ">"
            || (!closing && !fragment && !self_closed && depth == 0 && tok(t, p + 1) == "(")
        {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut cursor = 0;
        while j <= p {
            if cursor < code.len() && code[cursor].start == j {
                j = code[cursor].end;
                cursor += 1;
                continue;
            }
            if !(component && j == name) {
                out.ignore(j);
            }
            j += 1;
        }
        if component && !closing {
            for &attr in &attributes {
                out.props.insert(attr, t[name].span.clone());
            }
        }
        if closing {
            depth = depth.saturating_sub(1);
        } else if !self_closed {
            depth += 1;
        }
        i = p + 1;
    }
    out
}
