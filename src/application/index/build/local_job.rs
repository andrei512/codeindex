//! Captured source is immutable for the lifetime of the job. Only completion
//! publishes the local row. A suspended job owns its line cursor and lexer.
use super::{Facts, Language, Local, code};
pub(super) const SLICE: usize = 512 * 1024;

pub(super) struct Job<'a, const QUANTUM: usize = SLICE> {
    slot: &'a mut Option<Local>,
    path: &'a str,
    text: Option<&'a str>,
    local: Local,
    scanned: usize,
    lexical: Option<(
        &'static dyn code::LexedAnalyzer,
        code::ScanState<'a>,
    )>,
}

impl<'a> Job<'a> {
    pub fn new(
        slot: &'a mut Option<Local>,
        path: &'a str,
        text: Option<&'a str>,
        project: Option<&code::CProject>,
    ) -> Self {
        Self::with_quantum(slot, path, text, project)
    }
}
impl<'a, const QUANTUM: usize> Job<'a, QUANTUM> {
    fn with_quantum(
        slot: &'a mut Option<Local>,
        path: &'a str,
        text: Option<&'a str>,
        project: Option<&code::CProject>,
    ) -> Self {
        let lexical = text.filter(|s| s.len() > QUANTUM).and_then(|_| {
            let lang = Language::for_path(path)?;
            if project.is_some() && code::CProject::supports(lang) {
                return None;
            }
            Some((lang.analyzer().lexed()?, code::ScanState::new(lang)))
        });
        Self {
            slot,
            path,
            text,
            local: Local::default(),
            scanned: 0,
            lexical,
        }
    }

    pub fn advance(&mut self, project: Option<&code::CProject>) -> bool {
        let Some(text) = self.text.filter(|s| s.len() > QUANTUM) else {
            *self.slot = Some(Facts::All.read(self.path, self.text, project));
            return true;
        };
        let complete = if let Some((_, state)) = self.lexical.as_mut() {
            state.advance(text, QUANTUM, &mut |_| {}, &mut |end| {
                self.local.line_ends.push(end)
            })
        } else {
            let end = text.ceil_char_boundary((self.scanned + QUANTUM).min(text.len()));
            self.local.line_ends.extend(
                text[self.scanned..end]
                    .match_indices('\n')
                    .map(|(i, _)| self.scanned + i + 1),
            );
            self.scanned = end;
            end == text.len()
        };
        if !complete {
            return false;
        }
        self.local.byte_len = text.len();
        if let Some((parser, state)) = self.lexical.take() {
            let doc = code::Document {
                path: self.path,
                text,
            };
            self.local.record_measurement(parser.resume_measure(&doc, &state, true));
        } else {
            self.local.read_all(self.path, text, project);
        }
        *self.slot = Some(std::mem::take(&mut self.local));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check<const N: usize>(path: &str, text: &str, expected: &Local) {
        let mut slot = None;
        let mut job = Job::<N>::with_quantum(&mut slot, path, Some(text), None);
        while !job.advance(None) {}
        let actual = slot.unwrap();
        assert_eq!(
            (actual.byte_len, &actual.line_ends),
            (expected.byte_len, &expected.line_ends),
            "{path}"
        );
        assert_eq!(actual.score, expected.score, "{path}");
        assert_eq!(actual.test_score, expected.test_score, "{path}");
        let mut before = Vec::new();
        expected.code.encode(&mut before);
        let mut after = Vec::new();
        actual.code.encode(&mut after);
        assert_eq!(before, after, "{path}");
        assert_eq!(
            expected.markdown.is_some(),
            actual.markdown.is_some(),
            "{path}"
        );
    }

    #[test]
    fn every_profile_publishes_the_same_facts_with_small_slices() {
        let paths = [
            "sample.rs",
            "sample.ts",
            "sample.py",
            "BUILD",
            "sample.sh",
            "sample.ps1",
            "sample.cs",
            "sample.swift",
            "sample.c",
            "sample.cpp",
            "sample.proto",
            "sample.css",
            "sample.html",
            "sample.sql",
            "sample.go",
            "sample.java",
            "sample.kt",
            "sample.m",
            "sample.dart",
            "sample.php",
            "sample.hack",
            "Dockerfile",
            "Makefile",
            "sample.bat",
            "CMakeLists.txt",
            "Procfile",
            "sample.tf",
            "sample.nix",
            "sample.rb",
            "sample.lua",
            "sample.pl",
            "sample.s",
            "sample.mako",
            "sample.plpgsql",
        ];
        for path in paths {
            assert!(Language::for_path(path).is_some(), "{path}");
            let text = "// comment π\nfunction sample(x) { if (x) return '🙂'; }\n# next\n";
            let expected = Facts::All.read(path, Some(text), None);
            check::<1>(path, text, &expected);
            check::<7>(path, text, &expected);
        }
    }
}
