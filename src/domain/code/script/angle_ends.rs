//! A speculative type argument can instead be a comparison. A failed outer
//! probe still proves which inner brackets matched in the inspected interval.
use super::{Token, tok};
use std::{collections::HashMap, ops::Range};

pub(super) struct AngleEnds<'t, 's> {
    pub(super) tokens: &'t [Token<'s>],
    inspected: Range<usize>,
    outer: Option<usize>,
    pending: Vec<usize>,
    nested: HashMap<usize, usize>,
}

impl<'t, 's> AngleEnds<'t, 's> {
    pub fn new(tokens: &'t [Token<'s>]) -> Self {
        Self {
            tokens,
            inspected: 0..0,
            outer: None,
            pending: Vec::new(),
            nested: HashMap::new(),
        }
    }

    pub fn arguments(&mut self, after_name: usize) -> Option<usize> {
        let open = match tok(self.tokens, after_name) {
            "(" => return Some(after_name),
            "<" => self.following(after_name)? + 1,
            _ => return None,
        };
        (tok(self.tokens, open) == "(").then_some(open)
    }

    /// Callers start on a literal '<'. A fat arrow never closes a type.
    pub fn following(&mut self, start: usize) -> Option<usize> {
        if self.inspected.contains(&start) {
            return self.cached(start);
        }
        self.pending.clear();
        self.nested.clear();
        (self.inspected, self.outer) = scan(self.tokens, start, |event| match event {
            Nested::Open(index) => self.pending.push(index),
            Nested::Close(index) => {
                let open = self
                    .pending
                    .pop()
                    .expect("nested closing bracket has an opening");
                self.nested.insert(open, index);
            }
        });
        self.outer
    }

    fn cached(&self, start: usize) -> Option<usize> {
        if self.inspected.start == start {
            self.outer
        } else {
            self.nested.get(&start).copied()
        }
    }
}

pub(in crate::domain::code) fn angle_end(tokens: &[Token], start: usize) -> Option<usize> {
    if tok(tokens, start) != "<" {
        return None;
    }
    scan(tokens, start, |_| {}).1
}

enum Nested {
    Open(usize),
    Close(usize),
}

fn scan(
    tokens: &[Token],
    start: usize,
    mut visit: impl FnMut(Nested),
) -> (Range<usize>, Option<usize>) {
    let mut depth = 0;
    for i in start + 1..tokens.len() {
        let token = &tokens[i];
        let previous = &tokens[i - 1];
        match bracket(&mut depth, (previous, token), i, &mut visit) {
            Bracket::Close => return (start..i + 1, Some(i)),
            Bracket::Gap => return (start..i, None),
            Bracket::Continue => {}
        }
    }
    (start..tokens.len(), None)
}

enum Bracket {
    Close,
    Gap,
    Continue,
}

fn bracket(
    depth: &mut usize,
    tokens: (&Token, &Token),
    i: usize,
    visit: &mut impl FnMut(Nested),
) -> Bracket {
    let (previous, token) = tokens;
    let closes = token.text == ">" && previous.text != "=";
    // Across a blank line only an innermost bracket may close.
    match (closes, *depth, token.text) {
        (true, 0, _) => return Bracket::Close,
        (true, _, _) => {
            *depth -= 1;
            visit(Nested::Close(i));
        }
        _ => {}
    }
    if token.line > previous.line + 1 {
        return Bracket::Gap;
    }
    if token.text == "<" {
        *depth += 1;
        visit(Nested::Open(i));
    }
    Bracket::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::code::{Language, lexer};
    #[test]
    fn cached_inner_brackets_match_individual_probes() {
        for first in ["a < b", "a < B <", "f<T>", "=>", ""] {
            for join in [" ", "\n", "\n\n"] {
                for last in ["<t", ">", "<X> >", "a => b > >", "x < y < z"] {
                    check_bracket_cache(first, join, last);
                }
            }
        }
    }
    fn check_bracket_cache(first: &str, join: &str, last: &str) {
        let source = format!("< {first}{join}{last}");
        let tokens = lexer::tokens(&source, Language::Js);
        let mut cache = AngleEnds::new(&tokens);
        let openings: Vec<_> = (0..tokens.len())
            .filter(|&i| tokens[i].text == "<")
            .collect();
        for &i in openings.iter().chain(openings.iter().rev()) {
            assert_eq!(
                cache.following(i),
                super::super::angle_end(&tokens, i),
                "{source:?} at {i}"
            );
        }
    }
}
