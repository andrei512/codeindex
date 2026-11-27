//! Index a use once together with the lookup that will resolve it. A shorthand
//! field has both a member use and a lexical use at the same source position.
use super::*;
use std::num::NonZeroUsize;

fn dotted_member(t: &[Token], at: usize) -> bool {
    at > 0 && t[at - 1].text == "." && (at < 2 || t[at - 2].text != ".")
}

fn field_colon(t: &[Token], at: usize) -> bool {
    word(t, at + 1) == ":" && word(t, at + 2) != ":"
}

pub(super) struct TokenUses {
    sites: Vec<Option<NonZeroUsize>>,
    previous_path: Option<(usize, usize)>,
}

impl TokenUses {
    pub fn new(tokens: usize) -> Self {
        Self {
            sites: vec![None; tokens],
            previous_path: None,
        }
    }

    pub fn at(&self, token: usize) -> Option<usize> {
        self.sites[token].map(|n| n.get() - 1)
    }

    pub fn remember(&mut self, token: usize, id: usize) {
        self.sites[token] = NonZeroUsize::new(id + 1);
    }

    pub fn receiver(
        &self,
        tokens: &[Token],
        pairs: &[usize],
        i: usize,
        literal: &[String],
    ) -> Option<(members::Receiver, bool)> {
        if !ident(tokens[i].text) {
            return None;
        }
        let prev = i.checked_sub(1).map_or("", |j| word(tokens, j));
        if i > 1 && dotted_member(tokens, i) {
            let operand = members::operand(tokens, pairs, i - 2)?;
            return Some((
                members::Receiver::Previous(self.at(operand)?),
                call_follows(tokens, pairs, i),
            ));
        }
        if literal.is_empty() || !matches!(prev, "{" | ",") {
            return None;
        }
        let next = word(tokens, i + 1);
        let field = matches!(next, "," | "}") || field_colon(tokens, i);
        field.then(|| (members::Receiver::Literal(literal.to_vec()), false))
    }
}

pub(super) struct LexicalUse<'a, 's> {
    pub tokens: &'a [Token<'s>],
    pub at: usize,
    pub scope: usize,
    pub enclosing: &'a Scope,
    pub delimiter: Option<usize>,
}

impl LexicalUse<'_, '_> {
    fn admitted(&self) -> bool {
        let t = self.tokens;
        let i = self.at;
        let prior = i.checked_sub(1).map_or("", |j| word(t, j));
        let next = word(t, i + 1);
        path_segment(t[i].text)
            && !matches!(prior, "'" | "$" | "#")
            && !matches!(next, "!" | "#")
            && !dotted_member(t, i)
            && !field_colon(t, i)
    }

    fn field_value(&self, first: usize, before: &str) -> bool {
        before == ":"
            && first >= 3
            && matches!(word(self.tokens, first - 3), "{" | ",")
            && !self.enclosing.literal.is_empty()
            && self
                .delimiter
                .is_some_and(|op| word(self.tokens, op) == "{")
    }

    fn namespace(&self, first: usize) -> Namespace {
        let t = self.tokens;
        let i = self.at;
        let spelling = t[i].text;
        let next = word(t, i + 1);
        let before = first.checked_sub(1).map_or("", |j| word(t, j));
        let left = first.checked_sub(2).map_or("", |j| word(t, j));
        if self.value_position(first, before, left) {
            return Namespace::Value;
        }
        let type_context = matches!((before, left), (":" | "as", _) | (">", "-"));
        if type_context || self.qualified_module(next) {
            return Namespace::Type;
        }
        if name(spelling).starts_with(char::is_uppercase) && next != "(" {
            return Namespace::Either;
        }
        Namespace::Value
    }

    fn value_position(&self, first: usize, before: &str, left: &str) -> bool {
        let spelling = word(self.tokens, self.at);
        let next = word(self.tokens, self.at + 1);
        let value = spelling == "self" || self.field_value(first, before);
        let after_colon = annotation_operand(first, before, left) && operand_start(next, spelling);
        let explicit_value = value && next != ":";
        explicit_value || after_colon
    }

    fn qualified_module(&self, next: &str) -> bool {
        next == ":"
            && word(self.tokens, self.at + 2) == ":"
            && word(self.tokens, self.at + 3) != "<"
    }
}

fn annotation_operand(first: usize, before: &str, left: &str) -> bool {
    let standalone_colon = before == ":" && left != ":";
    first > 1 && standalone_colon
}

fn operand_start(next: &str, spelling: &str) -> bool {
    matches!(next, "." | "(") && spelling.starts_with(char::is_lowercase)
}

impl TokenUses {
    pub(super) fn lexical(&mut self, request: LexicalUse<'_, '_>) -> Option<Lookup> {
        if !request.admitted() {
            return None;
        }
        let t = request.tokens;
        let i = request.at;
        let first = self.path_start(t, i);
        let path = if first == i {
            Vec::new()
        } else {
            (first..=i)
                .step_by(3)
                .map(|j| name(t[j].text).to_owned())
                .collect()
        };
        Some(Lookup {
            scope: request.scope,
            ns: request.namespace(first),
            path,
        })
    }

    fn path_start(&mut self, t: &[Token], i: usize) -> usize {
        let mut first = i;
        while let Some(previous) = preceding_segment(t, first).filter(|&p| path_segment(word(t, p)))
        {
            first = match self.previous_path {
                Some((last, start)) if last == previous => {
                    first = start;
                    break;
                }
                _ => previous,
            };
        }
        self.previous_path = Some((i, first));
        first
    }
}
