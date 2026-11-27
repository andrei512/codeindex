//! Coordinate parser facts, lexical input and file scoring for each language profile.
use super::{Analysis, CProject, Document, Language, lexer, score, script};
use score::Input;

pub(crate) fn scored(
    lang: Language,
    text: &str,
    path: &str,
    project: Option<&CProject>,
) -> (Analysis, score::Saved) {
    let super::Measured { facts, base, .. } =
        score_file(lang, &Document { path, text }, project, false);
    (facts, base)
}

pub(crate) fn score_file(
    lang: Language,
    doc: &Document,
    project: Option<&CProject>,
    test_rules: bool,
) -> super::Measured {
    lang.with_analyzer(doc.path, project, |provider| {
        provider.measure(doc, test_rules)
    })
}

#[derive(Clone, Copy)]
pub(super) struct ScoreSource<'a, 'text> {
    pub lang: Language,
    pub doc: &'a Document<'text>,
}

/// Score an already resolved translation unit against a query-specific policy.
pub(super) fn score_c_facts(
    source: ScoreSource,
    facts: &Analysis,
    configured: bool,
    test_rules: bool,
) -> score::Saved {
    let (base, adjusted) = written_c_scores(source, facts, configured, test_rules);
    adjusted.or(base)
}

pub(super) fn written_c_scores(
    source: ScoreSource,
    facts: &Analysis,
    configured: bool,
    test_rules: bool,
) -> (score::Saved, score::Saved) {
    let tokens = lexer::tokens(source.doc.text, source.lang);
    let pairs = script::paired_for_score(&tokens);
    let syntax = score::PairedSyntax {
        tokens: &tokens,
        pairs: &pairs,
    };
    if !configured {
        return score::paired_scores(source, syntax, facts, test_rules);
    }
    configured_scores(source, syntax, facts, test_rules)
}

/// Project facts describe a configured translation unit. Test regions are
/// reconstructed from the written source when an adjustment needs them.
pub(super) fn configured_scores(
    ScoreSource { lang, doc }: ScoreSource,
    syntax: score::PairedSyntax,
    facts: &Analysis,
    tests: bool,
) -> (score::Saved, score::Saved) {
    let base = score::with_pairs(lang, doc.text, syntax, facts);
    Input::Paired(syntax).policies(doc, base, None, tests)
}
