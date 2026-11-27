use super::parser::{
    Parser,
    dialect::{Grammar, Javascript, Python},
};
use crate::domain::code::analyzer::{Document, FileAnalyzer, Measured};
use crate::domain::code::{
    Analysis, Token,
    edit_proof::{EditProof, GraphComparison},
    lexer, script,
};

pub(in crate::domain::code) const JAVASCRIPT: &dyn FileAnalyzer =
    &Script::<Javascript>(std::marker::PhantomData);
pub(in crate::domain::code) const PYTHON: &dyn FileAnalyzer =
    &Script::<Python>(std::marker::PhantomData);

struct Script<G>(std::marker::PhantomData<fn() -> G>);
impl<G: Grammar> Script<G> {
    fn parse(&self, text: &str, tokens: &[Token]) -> (Analysis, Vec<usize>) {
        let (grammar, pairs) = G::prepare(tokens);
        let facts = Parser::<G>::new(text, tokens, &pairs).analyze(grammar);
        (facts, pairs)
    }
}
pub(in crate::domain::code) fn javascript_from_tokens(
    text: &str,
    tokens: &[Token],
) -> (Analysis, Vec<usize>) {
    Script::<Javascript>(std::marker::PhantomData).parse(text, tokens)
}
impl<G: Grammar> FileAnalyzer for Script<G> {
    fn lexed(&self) -> Option<&dyn crate::domain::code::analyzer::LexedAnalyzer> {
        Some(self)
    }
    fn edit_proof(&self) -> Option<&dyn EditProof> {
        Some(self)
    }
    fn analyze(&self, doc: &Document) -> Analysis {
        self.parse(doc.text, &lexer::tokens(doc.text, G::LANGUAGE))
            .0
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        doc.tokenize_and_measure(G::LANGUAGE, self, tests)
    }
}
impl<G: Grammar> crate::domain::code::analyzer::LexedAnalyzer for Script<G> {
    fn from_tokens(
        &self,
        doc: &Document,
        tokens: &[crate::domain::code::Token],
        tests: bool,
    ) -> Measured {
        let (facts, pairs) = self.parse(doc.text, tokens);
        crate::domain::code::score::PairedSyntax {
            tokens,
            pairs: &pairs,
        }
        .measure(G::LANGUAGE, doc, facts, tests)
    }
}
impl<G: Grammar> EditProof for Script<G> {
    fn compare(&self, analysis: &Analysis, before: &str, after: &str) -> GraphComparison {
        if script::same_binding_text(analysis, G::LANGUAGE, before, after) {
            GraphComparison::CompareEvidence
        } else {
            GraphComparison::Recompute
        }
    }
}

pub(in crate::domain::code) struct ScriptResolver;
impl crate::domain::code::project_resolver::ProjectResolver for ScriptResolver {
    fn refine(
        &self,
        project: &crate::domain::code::ProjectView,
        prior: &[Option<usize>],
        keep: &mut [bool],
        _: &mut dyn FnMut(usize) -> std::io::Result<Analysis>,
    ) -> std::io::Result<()> {
        project.modules().refine(project, prior, keep);
        Ok(())
    }
    fn reads(&self, path: &str, lang: Option<crate::domain::code::Language>) -> bool {
        use crate::domain::code::Language;
        matches!(lang, Some(Language::Python | Language::Js))
            || path == "pyproject.toml"
            || matches!(
                path.rsplit('/').next(),
                Some("package.json" | "tsconfig.json")
            )
    }
    fn bind(
        &self,
        files: &[usize],
        project: &crate::domain::code::project_resolver::ProjectView,
        output: &mut crate::domain::code::ForwardNames,
    ) {
        project.modules().bind(files, project, output);
        script::public_labels(files, project.units, &mut output.public_labels);
    }
}
