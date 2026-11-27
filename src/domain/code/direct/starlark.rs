//! Starlark definitions own direct calls in their indented bodies.
use super::{Analysis, Call, Declaration, Kind, Token};
use crate::domain::code::{ForwardNames, Site, Unit, indentation};

pub(in crate::domain::code) fn analyze(text: &str, tokens: &[Token]) -> Analysis {
    let mut facts = WrittenCalls::default();
    for (i, token) in tokens.iter().enumerate() {
        let previous = i.checked_sub(1).map(|i| &tokens[i]);
        facts.update_scope(text, token, previous);
        facts.observe(token, previous, tokens.get(i + 1));
    }
    facts.analysis
}

#[derive(Default)]
struct WrittenCalls {
    analysis: Analysis,
    scopes: Vec<(usize, usize)>,
    indent: usize,
}

impl WrittenCalls {
    fn update_scope(&mut self, text: &str, token: &Token, previous: Option<&Token>) {
        if previous.is_some_and(|prior| prior.line == token.line) {
            return;
        }
        self.indent = indentation(text, token.span.start);
        while self
            .scopes
            .last()
            .is_some_and(|&(start, _)| start >= self.indent)
        {
            self.scopes.pop();
        }
    }

    fn observe(&mut self, token: &Token, previous: Option<&Token>, next: Option<&Token>) {
        if token.kind != Kind::Word {
            return;
        }
        let previous = previous.map_or("", |token| token.text);
        if previous == "def" {
            self.scopes
                .push((self.indent, self.analysis.declarations.len()));
            self.analysis.declarations.push(Declaration {
                span: token.span.clone(),
                kind: "function_definition",
            });
        } else if call_name(token.text, previous) && next.is_some_and(|t| t.text == "(") {
            self.analysis.calls.push(Call {
                span: token.span.clone(),
                owner: self.scopes.last().map(|&(_, owner)| owner),
                is_call: true,
            });
        }
    }
}

fn call_name(spelling: &str, previous: &str) -> bool {
    let unqualified = !matches!(previous, "." | ":" | "#");
    unqualified
        && !matches!(
            spelling,
            "if" | "for"
                | "while"
                | "match"
                | "switch"
                | "catch"
                | "return"
                | "sizeof"
                | "function"
                | "fn"
        )
}

pub(in crate::domain::code) fn resolve(file: usize, unit: &Unit, names: &mut ForwardNames) {
    for (local, call) in unit.analysis.calls.iter().enumerate() {
        let Some(sites) = names.by_name.get(&unit.text[call.span.clone()]) else { continue };
        let targets = same_file_targets(sites, file);
        targets.bind(Site { file, local }, names);
    }
}

enum Targets {
    None,
    One(Site),
    Many(Vec<Site>),
}

impl Targets {
    fn bind(self, source: Site, names: &mut ForwardNames) {
        match self {
            Self::None => {}
            Self::One(target) => names.bind_one(source, target),
            Self::Many(targets) => names.bind(source, targets),
        }
    }
}

fn same_file_targets(sites: &[Site], file: usize) -> Targets {
    // Collection appends declarations in file order across languages.
    let start = sites.partition_point(|site| site.file < file);
    let rest = &sites[start..];
    let count = rest.partition_point(|site| site.file == file);
    match &rest[..count] {
        [] => Targets::None,
        &[target] => Targets::One(target),
        many => Targets::Many(many.to_vec()),
    }
}

pub(in crate::domain::code) struct StarlarkResolver;
impl crate::domain::code::project_resolver::ProjectResolver for StarlarkResolver {
    fn isolated(&self, _:&crate::domain::code::Unit)->bool { true }
    fn reads(&self, _: &str, lang: Option<crate::domain::code::Language>) -> bool {
        use crate::domain::code::Language;
        lang == Some(Language::Starlark)
    }
    fn bind(&self, files: &[usize], project: &crate::domain::code::project_resolver::ProjectView, output: &mut crate::domain::code::ForwardNames) {
        for &id in files { resolve(id, &project.units[id], output); }
    }
}

