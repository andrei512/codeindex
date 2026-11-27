//! Written parameter declarations share token bounds with overload resolution.
use super::{Analyzer, Token, ident};

#[derive(Default)]
pub(super) struct Parameter<'a> {
    pub(super) name: Option<usize>,
    pub(super) ty: Option<&'a str>,
    pub(super) label: &'a str,
    pub(super) is_const: bool,
}

/// A cursor owns only token ordinals. Adding a parameter binding to the
/// analyzer does not invalidate the next span in the same declaration.
pub(super) struct ParameterList {
    next: usize,
    close: usize,
}

impl ParameterList {
    pub fn next(&mut self, analyzer: &Analyzer) -> Option<(usize, usize)> {
        while self.next <= self.close && self.close < analyzer.t.len() {
            let start = self.next;
            let end = self.boundary(analyzer, start);
            self.next = end + 1;
            match &analyzer.t[start..end] {
                [] => {}
                [token] if token.text == "void" => {}
                _ => return Some((start, end)),
            }
        }
        None
    }

    fn boundary(&self, analyzer: &Analyzer, mut at: usize) -> usize {
        while at < self.close {
            at = match analyzer.word(at) {
                "," => return at,
                "(" | "[" | "{" if analyzer.pairs[at] < self.close => analyzer.pairs[at] + 1,
                _ => at + 1,
            };
        }
        at
    }
}

pub(super) enum FunctionParameters<'a> {
    Written(ParameterList),
    Checked(std::vec::IntoIter<Parameter<'a>>),
}
impl<'a> FunctionParameters<'a> {
    pub(super) fn next<G: super::functions::FunctionGrammar>(
        &mut self,
        a: &Analyzer<'a, '_>,
    ) -> Option<Parameter<'a>> {
        match self {
            Self::Written(list) => list.next(a).map(|(lo, hi)| G::parameter(a, lo, hi)),
            Self::Checked(parameters) => parameters.next(),
        }
    }
}

impl<'a> Analyzer<'a, '_> {
    pub(super) fn function_parameters<G: super::functions::FunctionGrammar>(
        &self,
        open: usize,
        scope: usize,
        ambiguous: bool,
    ) -> Option<FunctionParameters<'a>> {
        let mut list = self.parameter_list(open);
        if !ambiguous {
            return Some(FunctionParameters::Written(list));
        }
        // A local C++ prototype may be an initialized value. Validate all
        // operands before introducing any parameter or function binding.
        let mut parameters = Vec::new();
        while let Some((lo, hi)) = list.next(self) {
            if self.arg_type(scope, lo, hi).is_some()
                || (lo..hi).any(|j| matches!(self.word(j), "!" | "+" | "." | "/"))
            {
                return None;
            }
            parameters.push(G::parameter(self, lo, hi));
        }
        Some(FunctionParameters::Checked(parameters.into_iter()))
    }

    pub(super) fn parameter_list(&self, open: usize) -> ParameterList {
        ParameterList {
            next: open + 1,
            close: self.pairs[open],
        }
    }

    pub(super) fn parameter_spans(&self, open: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let mut list = self.parameter_list(open);
        std::iter::from_fn(move || list.next(self))
    }

    pub(super) fn swift_parameter(&self, lo: usize, hi: usize) -> Parameter<'a> {
        let mut parameter = Parameter {
            label: self.word(lo),
            ..Parameter::default()
        };
        for (offset, token) in self.t[lo..hi].iter().enumerate() {
            match token.text {
                "=" => break,
                "const" => parameter.is_const = true,
                ":" if parameter.ty.is_none() => {
                    let colon = lo + offset;
                    parameter.name = (offset > 0).then(|| colon - 1);
                    parameter.ty = Some(self.word(colon + 1));
                }
                _ => {}
            }
        }
        parameter
    }

    pub(super) fn type_first_parameter(&self, lo: usize, hi: usize) -> Parameter<'a> {
        let mut parameter = Parameter::default();
        let mut pointer = false;
        let tokens = self.t[lo..hi].iter().take_while(|token| token.text != "=");
        for (i, token) in tokens.enumerate() {
            parameter.is_const |= token.text == "const";
            pointer |= token.text == "*";
            self.parameter_word(&mut parameter, token, (i, lo));
        }
        if pointer && parameter.ty == Some("char") {
            parameter.ty = Some("char*");
        }
        parameter
    }

    fn parameter_word(
        &self,
        parameter: &mut Parameter<'a>,
        token: &Token<'a>,
        (i, lo): (usize, usize),
    ) {
        if !ident(token) {
            return;
        }
        let word = token.text;
        if i != 0 && !self.grammar.reserved(word) {
            parameter.name = Some(lo + i);
        }
        if !matches!(
            word,
            "const" | "volatile" | "ref" | "out" | "in" | "this" | "struct" | "class"
        ) {
            parameter.ty.get_or_insert(word);
        }
    }
}
