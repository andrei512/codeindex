//! Written source, tokens and delimiter pairs can have different owners. Some
//! language parsers use their own recovery rules for malformed delimiters.
use super::{Analysis, Language, Token, scan};
use std::cell::OnceCell;

/// Delimiter mates belong to the exact token stream used to build them.
#[derive(Clone, Copy)]
pub(in crate::domain::code) struct PairedSyntax<'s, 'text> {
    pub tokens: &'s [Token<'text>],
    pub pairs: &'s [usize],
}

impl PairedSyntax<'_, '_> {
    /// Advance past a balanced group without crossing the caller's scan boundary.
    /// An unmatched opener is consumed as one token, preserving recovery.
    pub(super) fn next_top_level(self, i: usize, limit: usize, openers: &[&str]) -> usize {
        let close = self.pairs[i];
        if (i + 1..limit).contains(&close) && openers.contains(&self.tokens[i].text) {
            close + 1
        } else {
            i + 1
        }
    }

    /// Return the mate of a complete parenthesized group in this exact stream.
    pub(super) fn close_paren(self, open: usize) -> Option<usize> {
        if super::scan::at(self.tokens, open) != "(" {
            return None;
        }
        self.pairs
            .get(open)
            .copied()
            .filter(|&end| end < self.tokens.len())
    }
}

#[derive(Clone, Copy)]
pub(in crate::domain::code) enum Input<'s, 'text> {
    Source,
    Lexed(Language, &'s [Token<'text>]),
    Paired(PairedSyntax<'s, 'text>),
}

pub(super) struct TestSyntax<'s, 'text> {
    pub(super) text: &'text str,
    pub(super) lang: Language,
    facts: Option<&'s Analysis>,
    regions: OnceCell<Vec<scan::Region<'text>>>,
    input: Input<'s, 'text>,
    pub(super) ruby_blocks: OnceCell<crate::domain::code::roles::RubyBlocks>,
    tokens: OnceCell<Vec<Token<'text>>>,
    pairs: OnceCell<Vec<usize>>,
}

impl<'s, 'text> TestSyntax<'s, 'text> {
    pub(super) fn new(
        lang: Language,
        text: &'text str,
        facts: Option<&'s Analysis>,
        input: Input<'s, 'text>,
    ) -> Self {
        Self {
            lang,
            text,
            facts,
            input,
            ruby_blocks: OnceCell::new(),
            regions: OnceCell::new(),
            tokens: OnceCell::new(),
            pairs: OnceCell::new(),
        }
    }

    pub(super) fn paired(&self) -> PairedSyntax<'_, 'text> {
        let tokens = match self.input {
            Input::Paired(syntax) => return syntax,
            Input::Lexed(lang, tokens) if lang == self.lang => tokens,
            Input::Source | Input::Lexed(..) => self
                .tokens
                .get_or_init(|| crate::domain::code::roles::role_tokens(self.text, self.lang)),
        };
        PairedSyntax {
            tokens,
            pairs: self
                .pairs
                .get_or_init(|| crate::domain::code::script::paired_for_score(tokens)),
        }
    }

    pub(super) fn regions(&self) -> &[scan::Region<'text>] {
        self.regions.get_or_init(|| {
            let written;
            let facts = match self.facts {
                Some(facts) => facts,
                None => {
                    // Project-configured C/C++ scoring uses the written source across build arms.
                    written = crate::domain::code::analyze(self.lang, self.text);
                    &written
                }
            };
            scan::RegionSource {
                lang: self.lang,
                text: self.text,
                syntax: self.paired(),
                facts,
            }
            .regions()
        })
    }
}
