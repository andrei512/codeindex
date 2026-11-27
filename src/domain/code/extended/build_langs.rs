//! Written, file-local names in build scripts and declarative configuration.
use crate::domain::code::Analysis;
use crate::domain::code::record_unique;
use std::{collections::HashMap, ops::Range};

pub(in crate::domain::code) mod cmake;
mod config;
mod lines;
pub(in crate::domain::code) use lines::{docker_comments, make_comments};
mod nix;
#[cfg(test)]
mod tests;

pub(in crate::domain::code) use cmake::analyze as cmake;
pub(in crate::domain::code) use config::hcl;
pub(in crate::domain::code) use lines::{
    batch as batchfile, docker as dockerfile, make as makefile, procfile,
};
pub(in crate::domain::code) use nix::analyze as nix;

type Span = Range<usize>;

#[derive(Default)]
struct Facts<K = String> {
    analysis: Analysis,
    names: HashMap<String, Option<usize>>,
    uses: Vec<(K, Span, Option<usize>, UseMode)>,
}

#[derive(Clone, Copy)]
enum UseMode {
    Reference,
    Call,
    EarlierReference,
    EarlierCall,
}

impl UseMode {
    fn call(self) -> bool {
        matches!(self, Self::Call | Self::EarlierCall)
    }

    fn admits(self, declaration: usize, usage: usize) -> bool {
        let earlier = matches!(self, Self::EarlierReference | Self::EarlierCall);
        !earlier || declaration < usage
    }
}

impl<K: AsRef<str>> Facts<K> {
    fn declare(&mut self, key: String, span: Span, kind: &'static str) -> usize {
        let id = self.analysis.declare_at(span, kind);
        record_unique(&mut self.names, key, id);
        id
    }

    fn unique(&self, key: &str) -> Option<usize> {
        self.names.get(key).copied().flatten()
    }

    fn usage(&mut self, key: K, span: Span, owner: Option<usize>, mode: UseMode) {
        self.uses.push((key, span, owner, mode));
    }

    fn finish(mut self) -> Analysis {
        for (key, span, owner, mode) in self.uses {
            let id = self.names.get(key.as_ref()).copied().flatten();
            Self::record_use(&mut self.analysis, id, (span, owner, mode));
        }
        self.analysis
    }

    fn record_use(
        analysis: &mut Analysis,
        id: Option<usize>,
        (span, owner, mode): (Span, Option<usize>, UseMode),
    ) {
        let Some(id) = id else { return };
        if mode.admits(analysis.declarations[id].span.start, span.start) {
            analysis.direct_edge(id, span, owner, mode.call());
        }
    }
}

fn span(source: &str, substring: &str) -> Span {
    let start = substring.as_ptr() as usize - source.as_ptr() as usize;
    start..start + substring.len()
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

fn simple(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(name_byte)
}

fn target(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_+-./".contains(&b))
}

pub(in crate::domain::code) fn config_comments(
    source: &str,
    nix: bool,
    comment: impl FnMut(usize),
) {
    config::tokens_with_comments(source, nix, comment);
}
