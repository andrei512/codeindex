//! End positions shared by unparenthesized JS arrows in the same expression.
use super::Token;
use std::collections::HashMap;

fn expression_boundary(
    tokens: &[Token],
    start: usize,
    i: usize,
    begins: &impl Fn(&str) -> bool,
) -> bool {
    let token = &tokens[i];
    let on_new_line = i > start && token.line > tokens[i - 1].line;
    let boundary = matches!(token.text, "," | ";" | ")" | "]" | "}");
    boundary || (on_new_line && begins(token.text))
}

fn arrow_body(tokens: &[Token], i: usize, operator: &str) -> Option<usize> {
    let arrow = operator == "=" && tokens.get(i + 1).is_some_and(|t| t.text == ">");
    let expression = tokens.get(i + 2).is_some_and(|t| t.text != "{");
    (arrow && expression).then_some(i + 2)
}

#[derive(Default)]
pub(in crate::domain::code) struct ExpressionBodies {
    ends: HashMap<usize, usize>,
    nested: Vec<usize>,
}

impl ExpressionBodies {
    pub fn end(
        &mut self,
        tokens: &[Token],
        pairs: &[usize],
        start: usize,
        begins_statement: impl Fn(&str) -> bool,
    ) -> usize {
        self.end_with(tokens, start, begins_statement, |i| {
            let close = pairs[i];
            if close > i && close < tokens.len() {
                close + 1
            } else {
                i + 1
            }
        })
    }

    pub(super) fn end_with(
        &mut self,
        tokens: &[Token],
        start: usize,
        begins_statement: impl Fn(&str) -> bool,
        mut next: impl FnMut(usize) -> usize,
    ) -> usize {
        if let Some(end) = self.ends.remove(&start) {
            return end;
        }
        let mut i = start;
        while let Some(token) = tokens.get(i) {
            if expression_boundary(tokens, start, i, &begins_statement) {
                break;
            }
            self.nested.extend(arrow_body(tokens, i, token.text));
            i = next(i);
        }
        // An arrow starting on the boundary itself can extend beyond it.
        for body in self.nested.drain(..).take_while(|&body| body < i) {
            self.ends.insert(body, i);
        }
        i
    }
}
