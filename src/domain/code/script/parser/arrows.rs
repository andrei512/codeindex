//! Arrow parameters introduce one lexical scope through the end of their block or expression.
use super::*;

pub(super) struct Arrow {
    parameters: std::ops::Range<usize>,
    body: usize,
    pub(super) block: bool,
}

impl Arrow {
    pub(super) fn parse(t: &[Token], end: &[usize], start: usize) -> Option<Self> {
        let token = t.get(start)?;
        let (parameters, close) = match token.text {
            "(" if end[start] < t.len() => (start + 1..end[start], end[start]),
            _ if word(token) => (start..start + 1, start),
            _ => return None,
        };
        if tok(t, close + 1) != "=" || tok(t, close + 2) != ">" {
            return None;
        }
        let body = close + 3;
        Some(Self {
            parameters,
            body,
            block: tok(t, body) == "{",
        })
    }
}
impl<G: Grammar> Parser<'_, '_, G> {
    pub(super) fn enter_arrow(
        &mut self,
        arrow: Arrow,
        scope: usize,
        owner: Option<usize>,
    ) -> usize {
        let child = self.evidence.child_scope(scope, owner);
        let body_scope = ScopeFacts::new(self.source, child, &mut self.analysis, &mut self.evidence).parameters(
            &mut self.angles,
            self.pairs,
            arrow.parameters,
            true,
        );
        let stop = if arrow.block {
            self.pairs[arrow.body]
        } else {
            self.expression_end(arrow.body)
        };
        self.active.enter(body_scope, stop);
        arrow.body + usize::from(arrow.block)
    }

    pub(super) fn expression_end(&mut self, start: usize) -> usize {
        let (t, pairs) = (self.tokens, self.pairs);
        self.expressions.end_with(
            t,
            start,
            |word| matches!(word, "const" | "let" | "function" | "return" | "export"),
            |p| {
                if tok(t, p) == "<"
                    && t.get(p + 1).is_some_and(word)
                    && let Some(last) = self.angles.following(p)
                    && matches!(tok(t, last + 1), "(" | "=" | ")" | "," | ";")
                {
                    last + 1
                } else if matches!(tok(t, p), "(" | "[" | "{") {
                    pairs[p] + 1
                } else {
                    p + 1
                }
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_arrow_ends_observe_generics_statement_breaks_and_unclosed_groups() {
        let source = "const f = a => b => call<A, B>(b, x => x);\nconst g = y => z =>\nfunction(){};\nconst h = a => b => (";
        let tokens = crate::domain::code::lexer::tokens(source, Language::Js);
        let pairs = paired(&tokens);
        let mut parser = Parser::<dialect::Javascript>::new(source, &tokens, &pairs);
        let boundaries: Vec<_> = tokens
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0].text == "=" && w[1].text == ">")
            .map(|(i, _)| parser.expression_end(i + 2))
            .collect();
        let words: Vec<_> = boundaries.iter().map(|&i| tok(&tokens, i)).collect();
        assert_eq!(words, [";", ";", ")", "function", ";", "", ""]);
        assert_eq!(boundaries[5..], [tokens.len() + 1; 2]);
    }
}
