//! Written, file-local names in build scripts and declarative configuration.
use crate::domain::code::{Analysis, Call, Declaration, Language};
use std::{collections::HashMap, ops::Range};

mod cmake;
mod config;
mod lines;
mod nix;
#[cfg(test)]
mod tests;

pub(super) fn analyze(lang: Language, source: &str) -> Analysis {
    match lang {
        Language::Dockerfile => lines::docker(source),
        Language::Makefile => lines::make(source),
        Language::Batchfile => lines::batch(source),
        Language::Procfile => lines::procfile(source),
        Language::CMake => cmake::analyze(source),
        Language::Hcl => config::hcl(source),
        Language::Nix => nix::analyze(source),
        _ => unreachable!("not a build/configuration language"),
    }
}

type Span = Range<usize>;

#[derive(Default)]
struct Facts {
    analysis: Analysis,
    names: HashMap<String, Vec<usize>>,
    uses: Vec<(String, Span, Option<usize>, bool, bool)>,
}

impl Facts {
    fn declare(&mut self, key: String, span: Span, kind: &'static str) -> usize {
        let id = self.analysis.declarations.len();
        self.analysis.declarations.push(Declaration { span, kind });
        self.names.entry(key).or_default().push(id);
        id
    }

    fn usage(&mut self, key: String, span: Span, owner: Option<usize>, call: bool, earlier: bool) {
        self.uses.push((key, span, owner, call, earlier));
    }

    fn edge(&mut self, target: usize, span: Span, owner: Option<usize>, is_call: bool) {
        self.analysis
            .direct
            .push((self.analysis.calls.len(), vec![target]));
        self.analysis.calls.push(Call {
            span,
            owner,
            is_call,
        });
    }

    fn finish(mut self) -> Analysis {
        for (key, span, owner, call, earlier) in std::mem::take(&mut self.uses) {
            let Some(ids) = self.names.get(&key) else {
                continue;
            };
            if let [id] = ids[..]
                && (!earlier || self.analysis.declarations[id].span.start < span.start)
            {
                self.edge(id, span, owner, call);
            }
        }
        self.analysis
    }
}

fn span(source: &str, substring: &str) -> Span {
    let start = substring.as_ptr() as usize - source.as_ptr() as usize;
    start..start + substring.len()
}

fn simple(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

fn target(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_+-./".contains(&b))
}
