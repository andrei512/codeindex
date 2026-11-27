//! Per-file facts selected by the consumers of this build.
use super::*;

#[derive(Default)]
pub(crate) struct Local {
    pub(crate) code: Analysis,
    pub(crate) score: code::score::Saved,
    pub(crate) test_score: code::score::Saved,
    pub(crate) markdown: Option<Box<Parsed>>,
    pub(crate) line_ends: Vec<usize>,
    pub(crate) byte_len: usize,
}

#[derive(Clone, Copy)]
pub(super) enum Facts {
    All,
    Code,
    Headings,
    Lines,
}

impl Facts {
    pub(super) fn read(
        self,
        path: &str,
        body: Option<&str>,
        project: Option<&code::CProject>,
    ) -> Local {
        let Some(text) = body else {
            return Local::default();
        };
        let mut local = Local::default();
        local
            .line_ends
            .extend(text.match_indices('\n').map(|(i, _)| i + 1));
        local.byte_len = text.len();
        match self {
            Self::All => local.read_all(path, text, project),
            Self::Code => local.code = read_code(path, text, project),
            Self::Headings if markdown::is_markdown(path) => {
                local.markdown = Some(Box::new(Parsed::headings(path, text)));
            }
            _ => {}
        }
        local
    }
}

fn read_code(path: &str, text: &str, project: Option<&code::CProject>) -> Analysis {
    let Some(language) = Language::for_path(path) else {
        return Analysis::default();
    };
    language.with_analyzer(path, project, |provider| provider.analyze(&code::Document { path, text }))
}

impl Local {
    pub(super) fn record_measurement(&mut self, measured: code::Measured) {
        self.code = measured.facts;
        self.score = measured.base;
        self.test_score = measured.adjusted;
    }

    pub(super) fn read_all(&mut self, path: &str, text: &str, project: Option<&code::CProject>) {
        if let Some(language) = Language::for_path(path) {
            self.record_measurement(code::score_file(language, &code::Document { path, text }, project, true));
        }
        if markdown::is_markdown(path) {
            let parsed = Parsed::parse(path, text);
            self.score = code::score::markdown(text, &parsed);
            self.markdown = Some(Box::new(parsed));
        }
        if self.score.is_none()
            && path.ends_with(".t")
            && code::score::suite_nesting::perl_source(text)
        {
            self.test_score = code::score_file(Language::Perl, &code::Document { path, text }, None, true).selected();
        }
    }
}
