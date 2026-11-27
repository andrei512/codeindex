use crate::domain::code::analyzer::{Document, FileAnalyzer, Measured};
use crate::domain::code::{
    Analysis, Language,
    edit_proof::{EditProof, GraphComparison},
    lexer, rust, score,
};
use score::Input;
pub(in crate::domain::code) struct Rust;
impl FileAnalyzer for Rust {
    fn lexed(&self) -> Option<&dyn crate::domain::code::analyzer::LexedAnalyzer> {
        Some(self)
    }
    fn edit_proof(&self) -> Option<&dyn EditProof> {
        Some(self)
    }
    fn analyze(&self, doc: &Document) -> Analysis {
        rust::analyze(&lexer::tokens(doc.text, Language::Rust), doc.text.len())
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        doc.tokenize_and_measure(Language::Rust, self, tests)
    }
}
impl crate::domain::code::analyzer::LexedAnalyzer for Rust {
    fn from_tokens(
        &self,
        doc: &Document,
        tokens: &[crate::domain::code::Token],
        tests: bool,
    ) -> Measured {
        let lang = Language::Rust;
        let delimiters = rust::delimiters(tokens);
        let pairs = delimiters.mates;
        let facts = rust::analyze_paired(tokens, doc.text.len(), &pairs);
        if delimiters.balanced {
            return score::PairedSyntax {
                tokens,
                pairs: &pairs,
            }
            .measure(lang, doc, facts, tests);
        }
        let base = score::with_pairs(lang, doc.text, score::PairedSyntax { tokens, pairs: &pairs }, &facts);
        // Malformed Rust uses different delimiter recovery for test measurements.
        Input::Lexed(lang, tokens).measure(doc, facts, base, tests)
    }
}

impl EditProof for Rust {
    fn compare(&self, analysis: &Analysis, before: &str, after: &str) -> GraphComparison {
        let Some((declaration, last)) = analysis.declarations.iter().enumerate().next_back() else {
            return GraphComparison::Recompute;
        };
        if before.len() != after.len()
            && (before.starts_with(after) || after.starts_with(before))
            && last.kind == "source_file"
            && last.span == (0..after.len())
        {
            GraphComparison::CompareWithOldEnd {
                declaration,
                old_end: before.len(),
                new_end: after.len(),
            }
        } else {
            GraphComparison::Recompute
        }
    }
}

pub(in crate::domain::code) struct RustResolver;
impl crate::domain::code::project_resolver::ProjectResolver for RustResolver {
    fn refine(
        &self,
        project: &crate::domain::code::ProjectView,
        prior: &[Option<usize>],
        keep: &mut [bool],
        old: &mut dyn FnMut(usize) -> std::io::Result<Analysis>,
    ) -> std::io::Result<()> {
        let mut former = Vec::new();
        let changed_rust = project.units.iter().enumerate().filter(|&(id, u)| {
            prior[id].is_none() && Language::for_path(u.path) == Some(Language::Rust)
        });
        for (id, u) in changed_rust {
            let prior = old(id)?;
            let (Some(before), Some(_)) = (&prior.rust, &u.analysis.rust) else {
                return Ok(());
            };
            rust::binding::previous_neighbors(id, project, before, &mut former);
        }
        project.rust(true).refine(prior, keep, &former);
        Ok(())
    }
    fn reads(&self, path: &str, lang: Option<crate::domain::code::Language>) -> bool {
        use crate::domain::code::Language;
        lang == Some(Language::Rust)
            || std::path::Path::new(path)
                .file_name()
                .is_some_and(|n| n == "Cargo.toml")
            || matches!(path.rsplit_once('/'), Some((dir, "config" | "config.toml")) if dir.rsplit('/').next() == Some(".cargo"))
    }
    fn bind(
        &self,
        files: &[usize],
        project: &crate::domain::code::project_resolver::ProjectView,
        output: &mut crate::domain::code::ForwardNames,
    ) {
        project.rust(false).bind(files, output);
    }
}
