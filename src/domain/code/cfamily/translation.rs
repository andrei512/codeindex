//! Preserve physical source locations when binding the active text of an inclusion.
use super::{
    Analyzer, Token,
    project::{CProject, Command, PreprocessingSource, TranslationInput},
};
use crate::domain::code::{Analysis, Call, Declaration, cache::record};
use std::{collections::HashMap, ops::Range, sync::Arc};

// Runs from the same source are coalesced until a directive or include splits them.
#[derive(Default)]
pub(super) struct Expansion {
    paths: Vec<String>,
    ids: HashMap<String, usize>,
    runs: Vec<(usize, Range<usize>)>,
    included: bool,
    length: usize,
    definitions: Vec<usize>,
    owners: HashMap<usize, String>,
}

pub(crate) struct Partition {
    pub key: String,
    pub paths: Vec<String>,
    pub declarations: Vec<(usize, Declaration)>,
    pub calls: Vec<(usize, Call)>,
    pub edges: Vec<(usize, Vec<usize>)>,
}
record!(Partition {
    key,
    paths,
    declarations,
    calls,
    edges
});

pub(super) enum PartitionPolicy {
    Included,
    Configured,
}

type CapturedHeader<'a> = Option<(&'a str, Arc<PreprocessingSource<'a>>)>;

#[derive(Default)]
struct FlatSource<'a> {
    tokens: Vec<Token<'a>>,
    origins: Vec<(usize, &'a Range<usize>)>,
}

impl<'a> FlatSource<'a> {
    fn append(&mut self, id: usize, tokens: &'a [Token<'a>]) {
        for token in tokens {
            let ordinal = self.origins.len();
            self.origins.push((id, &token.span));
            self.tokens.push(Token {
                span: ordinal..ordinal + 1,
                text: token.text,
                kind: token.kind,
                line: token.line,
            });
        }
    }
}

impl Expansion {
    pub fn enter(&mut self, path: &str, main: bool) -> usize {
        self.included |= !main;
        if let Some(&id) = self.ids.get(path) {
            return id;
        }
        let id = self.paths.len();
        self.ids.insert(path.to_owned(), id);
        self.paths.push(path.to_owned());
        id
    }

    pub fn definition(&mut self, file: usize, index: usize) {
        self.definitions.push(self.length);
        self.active(file, index..index + 1);
    }
    pub fn owner(&mut self, from_end: usize, name: &str) {
        self.owners.insert(self.length - from_end, name.to_owned());
    }
    pub fn active(&mut self, file: usize, range: Range<usize>) {
        self.length += range.len();
        if let Some((last, prior)) = self.runs.last_mut()
            && *last == file
            && prior.end == range.start
        {
            prior.end = range.end;
            return;
        }
        self.runs.push((file, range));
    }

    fn flatten<'a>(
        &self,
        main: TranslationInput<'a>,
        headers: &'a [CapturedHeader<'a>],
    ) -> Option<FlatSource<'a>> {
        let mut source = FlatSource::default();
        for (id, range) in &self.runs {
            let tokens = self.source_tokens(*id, &main, headers)?;
            source.append(*id, &tokens[range.clone()]);
        }
        Some(source)
    }

    fn source_tokens<'a>(
        &self,
        id: usize,
        main: &TranslationInput<'a>,
        headers: &'a [CapturedHeader<'a>],
    ) -> Option<&'a [super::Token<'a>]> {
        if self.paths[id] == main.path {
            return Some(main.source.tokens.as_slice());
        }
        Some(headers[id].as_ref()?.1.tokens.as_slice())
    }

    pub fn bind(
        self,
        (project, command): (&CProject, &Command),
        source: TranslationInput<'_>,
        policy: PartitionPolicy,
    ) -> Option<Partition> {
        if !self.included && matches!(policy, PartitionPolicy::Included) {
            return None;
        }
        let main = source.path;
        let lang = command.language();
        let headers: Vec<_> = self
            .paths
            .iter()
            .map(|p| {
                (p != main)
                    .then(|| project.header(p, command.mode()))
                    .flatten()
            })
            .collect();
        let FlatSource { tokens, origins } = self.flatten(source, &headers)?;
        let mut parser = Analyzer::new(lang, &tokens);
        parser.digit_separators = command.digit_separators();
        for index in self.definitions {
            parser.skip[index] = true;
            parser.add(index, 0, "local_binding", super::Role::Value);
        }
        parser.macro_owners = self.owners;
        parser.extract_names();
        let Analysis {
            declarations,
            calls,
            direct: edges,
            ..
        } = parser.out;
        let declarations = declarations
            .into_iter()
            .map(|mut d| {
                let (id, span) = origins[d.span.start];
                d.span = span.clone();
                (id, d)
            })
            .collect();
        let calls = calls
            .into_iter()
            .map(|mut c| {
                let (id, span) = origins[c.span.start];
                c.span = span.clone();
                (id, c)
            })
            .collect();
        Some(Partition {
            key: command.context_key(main),
            paths: self.paths,
            declarations,
            calls,
            edges,
        })
    }
}
