use crate::domain::code::comment_evidence::CommentEvidence;
use crate::domain::code::analyzer::{Document, FileAnalyzer, Measured};
use crate::domain::code::{Analysis, Language, Token, extended, lexer, markup, score};
pub(in crate::domain::code) struct Sql;
impl FileAnalyzer for Sql {
    fn comment_evidence(&self, _: Language, text: &str) -> CommentEvidence {
        CommentEvidence::sql(text)
    }

    fn lexed(&self) -> Option<&dyn crate::domain::code::analyzer::LexedAnalyzer> {
        Some(self)
    }
    fn analyze(&self, doc: &Document) -> Analysis {
        extended::sql(doc.text, &lexer::tokens(doc.text, Language::Sql))
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        doc.tokenize_and_measure(Language::Sql, self, tests)
    }
}
impl crate::domain::code::analyzer::LexedAnalyzer for Sql {
    fn from_tokens(
        &self,
        doc: &Document,
        tokens: &[crate::domain::code::Token],
        tests: bool,
    ) -> Measured {
        let facts = extended::sql(doc.text, tokens);
        let (base, adjusted) = score::sql_scores(doc, tokens, &facts, tests);
        Measured {
            facts,
            base,
            adjusted,
        }
    }
}
pub(in crate::domain::code) struct Html;
impl FileAnalyzer for Html {
    fn comment_evidence(&self, _: Language, text: &str) -> CommentEvidence {
        CommentEvidence::native(Language::Html, text, |text, emit| {
            markup::html_parts(text, |part| {
                if let markup::HtmlPart::Comment(at) = part {
                    emit(at);
                }
            });
        })
    }

    fn lexed(&self) -> Option<&dyn crate::domain::code::analyzer::LexedAnalyzer> {
        Some(self)
    }
    fn analyze(&self, doc: &Document) -> Analysis {
        markup::html(doc.text)
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        doc.tokenize_and_measure(Language::Html, self, tests)
    }
}
impl crate::domain::code::analyzer::LexedAnalyzer for Html {
    fn from_tokens(&self, doc: &Document, tokens: &[Token], tests: bool) -> Measured {
        let mut bodies = Vec::new();
        let collect = tests
            && doc.text.contains("testharness.js")
            && Language::for_path(doc.path) == Some(Language::Html);
        let facts = markup::analyze_html(doc.text, collect.then_some(&mut bodies));
        let base = score::with_tokens(Language::Html, doc.text, tokens, &facts);
        let adjusted = base
            .as_deref()
            .and_then(|original| {
                score::embedded_tests::score_bodies(
                    Language::Js,
                    doc,
                    original,
                    bodies,
                )
            })
            .map(std::sync::Arc::new);
        Measured {
            facts,
            base,
            adjusted,
        }
    }
}
