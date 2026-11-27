use crate::domain::code::analyzer::{ConfiguredScore, Document, FileAnalyzer, Measured};
use crate::domain::code::{Analysis, Language, cfamily, lexer, score, scoring};
pub(in crate::domain::code) struct CFamily(pub(in crate::domain::code) Language);
impl FileAnalyzer for CFamily {
    fn lexed(&self) -> Option<&dyn crate::domain::code::analyzer::LexedAnalyzer> {
        Some(self)
    }
    fn analyze(&self, doc: &Document) -> Analysis {
        cfamily::analyze(self.0, doc.text, &lexer::tokens(doc.text, self.0))
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        doc.tokenize_and_measure(self.0, self, tests)
    }
}
impl crate::domain::code::analyzer::LexedAnalyzer for CFamily {
    fn from_tokens(
        &self,
        doc: &Document,
        tokens: &[crate::domain::code::Token],
        tests: bool,
    ) -> Measured {
        let (facts, pairs) = cfamily::analyze_with_pairs(self.0, doc.text, tokens);
        score::PairedSyntax {
            tokens,
            pairs: &pairs,
        }
        .measure(self.0, doc, facts, tests)
    }
}
pub(in crate::domain::code) struct ConfiguredC(pub(in crate::domain::code) Language);
impl ConfiguredScore for ConfiguredC {
    fn measure_saved(
        &self,
        doc: &Document,
        facts: &Analysis,
        configured: bool,
        tests: bool,
    ) -> score::Saved {
        scoring::score_c_facts(
            scoring::ScoreSource { lang: self.0, doc },
            facts,
            configured,
            tests,
        )
    }
}
impl crate::domain::code::analyzer::LexedAnalyzer for ConfiguredC {
    fn from_tokens(
        &self,
        doc: &Document,
        tokens: &[crate::domain::code::Token],
        tests: bool,
    ) -> Measured {
        CFamily(self.0).from_tokens(doc, tokens, tests)
    }
}
impl FileAnalyzer for ConfiguredC {
    fn lexed(&self) -> Option<&dyn crate::domain::code::analyzer::LexedAnalyzer> {
        Some(self)
    }
    fn configured_score(&self) -> Option<&dyn ConfiguredScore> {
        Some(self)
    }
    fn analyze(&self, doc: &Document) -> Analysis {
        CFamily(self.0).analyze(doc)
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        CFamily(self.0).measure(doc, tests)
    }
}

impl FileAnalyzer for cfamily::project::TranslationUnit<'_, '_> {
    fn analyze(&self, doc: &Document) -> Analysis {
        self.extract(doc.path, doc.text, true, |_, _, facts, _| facts)
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        self.measure_products(doc, tests, true)
    }
    fn score(&self, doc: &Document, tests: bool) -> score::Saved {
        self.measure_products(doc, tests, false).selected()
    }
}

impl cfamily::project::TranslationUnit<'_, '_> {
    fn measure_products(&self, doc: &Document, tests: bool, graph: bool) -> Measured {
        let lang = self.written;
        self.extract(
            doc.path,
            doc.text,
            graph,
            |configured, tokens, facts, pairs| {
                if configured != lang {
                    let (base, adjusted) = scoring::written_c_scores(
                        scoring::ScoreSource { lang, doc },
                        &facts,
                        true,
                        tests,
                    );
                    return Measured {
                        facts,
                        base,
                        adjusted,
                    };
                }
                let input = score::PairedSyntax { tokens, pairs };
                let (base, adjusted) = scoring::configured_scores(
                    scoring::ScoreSource { lang, doc },
                    input,
                    &facts,
                    tests,
                );
                Measured {
                    facts,
                    base,
                    adjusted,
                }
            },
        )
    }
}
