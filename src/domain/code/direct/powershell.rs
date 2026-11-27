//! Literal command invocations and explicitly imported local modules.
use super::super::{
    Analysis, Call, ForwardNames, Site, Unit,
    lexer::{Kind, Token},
};
use crate::domain::code::record_unique;
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::Path,
};

#[derive(Default)]
pub(in crate::domain::code) struct Evidence {
    imports: Vec<Range<usize>>,
    exports: HashMap<String, usize>,
    qualified: Vec<(usize, Range<usize>)>,
}
crate::domain::code::cache::record!(Evidence {
    imports,
    qualified,
    exports
});

struct Function {
    id: usize,
    module_level: bool,
}

#[derive(Default)]
struct Functions {
    names: HashMap<Option<usize>, HashMap<String, Option<Function>>>,
    scopes: Vec<Option<usize>>,
    owner: Option<usize>,
    pending_owner: Option<usize>,
    allowed: Option<HashSet<String>>,
    nested_export: bool,
}

impl Functions {
    fn open(&mut self) {
        self.scopes.push(self.owner);
        self.owner = self.pending_owner.take().or(self.owner);
    }
    fn close(&mut self) {
        self.owner = self.scopes.pop().flatten();
    }
    fn export_names(&mut self, names: &[Token]) {
        if self.scopes.is_empty() {
            let allowed = self.allowed.get_or_insert_with(HashSet::new);
            allowed.extend(
                names
                    .iter()
                    .filter(|t| t.text != ",")
                    .map(|t| t.text.trim_matches(['\'', '"']).to_ascii_lowercase()),
            );
        } else {
            self.nested_export = true;
        }
    }
    fn declare(&mut self, name: &str, id: usize) {
        self.pending_owner = Some(id);
        let module_level = self.scopes.is_empty();
        record_unique(
            self.names.entry(self.owner).or_default(),
            name.to_ascii_lowercase(),
            Function { id, module_level },
        );
    }
    fn local(&self, owner: Option<usize>, name: &str) -> Option<usize> {
        let found = owner
            .and_then(|id| self.names.get(&Some(id))?.get(name))
            .or_else(|| self.names.get(&None)?.get(name))?;
        found.as_ref().map(|f| f.id)
    }
    fn bind_locals(
        &self,
        text: &str,
        analysis: &mut Analysis,
        qualified: &[(usize, Range<usize>)],
    ) {
        // Qualified calls are already recorded in source order. Bind the runs
        // of ordinary calls between them after every declaration is known.
        let mut start = 0;
        for end in qualified
            .iter()
            .map(|(call, _)| *call)
            .chain([analysis.calls.len()])
        {
            let ordinary = (start..end).filter_map(|row| {
                let call = &analysis.calls[row];
                let key = text[call.span.clone()].to_ascii_lowercase();
                Some((row, vec![self.local(call.owner, &key)?]))
            });
            analysis.direct.extend(ordinary);
            start = end + 1;
        }
    }

    fn exports(mut self, declarations_only: bool) -> HashMap<String, usize> {
        // A declaration-only module never runs the export commands in its
        // function bodies. With executable top-level code their effect is unknown.
        let allowed = self.allowed.as_ref();
        let all = allowed.map_or(declarations_only || !self.nested_export, |names| {
            names.contains("*")
        });
        self.names
            .remove(&None)
            .into_iter()
            .flatten()
            .filter_map(|(name, function)| {
                let f = function?;
                (f.module_level && (all || allowed.is_some_and(|names| names.contains(&name))))
                    .then_some((name, f.id))
            })
            .collect()
    }
}

fn command_start(tokens: &[Token], i: usize) -> bool {
    let token = &tokens[i];
    if token.kind != Kind::Word || token.text.starts_with('$') {
        return false;
    }
    let prev = i.checked_sub(1).map(|j| &tokens[j]);
    prev.is_none_or(|p| {
        p.line < token.line
            || (p.span.start == 0 && p.text == "\u{feff}")
            || matches!(p.text, ";" | "|" | "&" | "{" | "(" | "=")
            || p.text.eq_ignore_ascii_case("return")
    })
}

// Consume the complete operand even when it is dynamic or names a non-module file.
fn imported_module(
    text: &str,
    tokens: &[Token],
    mut i: usize,
    line: usize,
) -> (usize, Option<Range<usize>>) {
    let Some(arg) = tokens.get(i).filter(|n| n.line == line) else {
        return (i, None);
    };
    let start = arg.span.start;
    let mut end = arg.span.end;
    i += 1;
    while let Some(next) = tokens
        .get(i)
        .filter(|n| n.span.start == end && n.text != ";")
    {
        end = next.span.end;
        i += 1;
    }
    let path = text[start..end].trim_matches(['\'', '"']);
    let range = (!path.contains('$') && path.to_ascii_lowercase().ends_with(".psm1")).then(|| {
        let offset = path.as_ptr() as usize - text.as_ptr() as usize;
        offset..offset + path.len()
    });
    (i, range)
}

/// A comma continues an export argument list onto the following source line.
fn export_operands<'t, 's>(tokens: &'t [Token<'s>], mut line: usize) -> &'t [Token<'s>] {
    let mut continued = false;
    let end = tokens.iter().position(|token| {
        let boundary = token.line != line && !continued;
        continued = token.text == ",";
        line = token.line;
        boundary || matches!(token.text, ";" | "|" | "}")
    });
    &tokens[..end.unwrap_or(tokens.len())]
}

fn function_names<'t, 's>(operands: &'t [Token<'s>]) -> Option<&'t [Token<'s>]> {
    let names = match operands {
        [dash, option, tail @ ..]
            if dash.text == "-" && option.text.eq_ignore_ascii_case("Function") =>
        {
            tail
        }
        [dash, ..] if dash.text == "-" => return None,
        names => names,
    };
    names
        .iter()
        .all(|t| t.text == "," || literal_name(t.text).is_some())
        .then_some(names)
}

enum Command<'t, 's> {
    Open,
    Close,
    Function(&'t Token<'s>),
    Export(&'t [Token<'s>]),
    Import(Option<Range<usize>>),
    Call {
        name: &'t Token<'s>,
        qualifier: Option<Range<usize>>,
    },
}

enum Head {
    Open,
    Close,
    Separator,
    Function,
    Command,
    Operand,
}

impl Head {
    fn boundary(text: &str) -> Option<Self> {
        match text {
            "{" => Some(Self::Open),
            "}" => Some(Self::Close),
            ";" | "\u{feff}" => Some(Self::Separator),
            _ => None,
        }
    }

    fn at(tokens: &[Token], i: usize) -> Self {
        let text = tokens[i].text;
        if let Some(boundary) = Self::boundary(text) {
            return boundary;
        }
        if !command_start(tokens, i) {
            return Self::Operand;
        }
        if text.eq_ignore_ascii_case("function") {
            return Self::Function;
        }
        Self::Command
    }
}

// Each command consumes exactly its syntactic operands. The scope owner
// supplies whether an Import-Module head has module-level meaning.
struct Commands<'t, 's> {
    text: &'s str,
    tokens: &'t [Token<'s>],
    next: usize,
    declarations_only: bool,
}
impl<'t, 's> Commands<'t, 's> {
    fn next(&mut self, module_level: bool) -> Option<Command<'t, 's>> {
        loop {
            let i = self.next;
            let head = self.tokens.get(i)?;
            self.next += 1;
            let event = self.read_head(i, head, module_level);
            if event.is_some() {
                return event;
            }
        }
    }

    fn read_head(
        &mut self,
        i: usize,
        token: &'t Token<'s>,
        module_level: bool,
    ) -> Option<Command<'t, 's>> {
        let head = Head::at(self.tokens, i);
        match head {
            Head::Open => Some(Command::Open),
            Head::Close => Some(Command::Close),
            Head::Separator => None,
            Head::Function => self.function_name().map(Command::Function),
            Head::Command | Head::Operand => {
                self.declarations_only &= !module_level;
                matches!(head, Head::Command).then(|| self.command_head(token, module_level))
            }
        }
    }

    fn function_name(&mut self) -> Option<&'t Token<'s>> {
        let name = self
            .tokens
            .get(self.next)
            .filter(|t| t.kind == Kind::Word)?;
        self.next += 1;
        Some(name)
    }

    fn command_head(&mut self, head: &'t Token<'s>, module_level: bool) -> Command<'t, 's> {
        if head.text.eq_ignore_ascii_case("Export-ModuleMember") {
            return self.export(head.line);
        }
        if module_level && head.text.eq_ignore_ascii_case("Import-Module") {
            let (next, path) = imported_module(self.text, self.tokens, self.next, head.line);
            self.next = next;
            return Command::Import(path);
        }
        self.call(head)
    }

    fn export(&mut self, line: usize) -> Command<'t, 's> {
        let tail = &self.tokens[self.next..];
        let operands = export_operands(tail, line);
        let Some(names) = function_names(operands) else {
            // Complex operands may contain commands or braces that the walker still needs.
            return Command::Export(&[]);
        };
        self.next += operands.len();
        Command::Export(names)
    }

    fn call(&mut self, head: &'t Token<'s>) -> Command<'t, 's> {
        if let [slash, name, ..] = &self.tokens[self.next..]
            && slash.text == "\\"
            && slash.span.start == head.span.end
            && name.span.start == slash.span.end
            && name.kind == Kind::Word
        {
            self.next += 2;
            return Command::Call {
                name,
                qualifier: Some(head.span.clone()),
            };
        }
        Command::Call {
            name: head,
            qualifier: None,
        }
    }
}

#[derive(Default)]
struct CommandScope {
    analysis: Analysis,
    evidence: Evidence,
    functions: Functions,
}

impl CommandScope {
    fn observe(&mut self, command: Command<'_, '_>) {
        match command {
            Command::Open => self.functions.open(),
            Command::Close => self.functions.close(),
            Command::Function(name) => {
                let id = self.analysis.declare_at(name.span.clone(), "function_name");
                self.functions.declare(name.text, id);
            }
            Command::Export(names) => self.functions.export_names(names),
            Command::Import(path) => self.evidence.imports.extend(path),
            Command::Call { name, qualifier } => {
                let id = self.analysis.calls.len();
                self.analysis.calls.push(Call {
                    span: name.span.clone(),
                    owner: self.functions.owner,
                    is_call: true,
                });
                self.evidence
                    .qualified
                    .extend(qualifier.map(|span| (id, span)));
            }
        }
    }

    fn finish(mut self, text: &str, declarations_only: bool) -> Analysis {
        self.functions
            .bind_locals(text, &mut self.analysis, &self.evidence.qualified);
        self.evidence.exports = self.functions.exports(declarations_only);
        if !self.evidence.qualified.is_empty() || !self.evidence.exports.is_empty() {
            self.analysis.direct_evidence =
                Some(Box::new(super::Evidence::Powershell(self.evidence)));
        }
        self.analysis
    }
}

pub(in crate::domain::code) fn analyze(text: &str, tokens: &[Token]) -> Analysis {
    let mut scope = CommandScope::default();
    let mut commands = Commands {
        text,
        tokens,
        next: 0,
        declarations_only: true,
    };
    while let Some(command) = commands.next(scope.functions.scopes.is_empty()) {
        scope.observe(command);
    }
    scope.finish(text, commands.declarations_only)
}

fn evidence<'a>(unit: &Unit<'a>) -> Option<&'a Evidence> {
    match unit.analysis.direct_evidence.as_deref()? {
        super::Evidence::Powershell(facts) => Some(facts),
        _ => None,
    }
}

#[derive(Default)]
struct ImportedModules {
    // An unresolved import still shadows a module with the same written stem.
    by_stem: HashMap<String, Option<usize>>,
}

impl ImportedModules {
    fn import(&mut self, unit: &Unit, span: &Range<usize>, paths: &HashMap<&str, usize>) {
        let path = &unit.text[span.clone()];
        if let Some(stem) = Path::new(path).file_stem().and_then(|p| p.to_str()) {
            self.by_stem.insert(
                stem.to_ascii_lowercase(),
                super::shell::path_for(unit.path, path, paths),
            );
        }
    }

    fn target(
        &self,
        unit: &Unit,
        qualifier: &Range<usize>,
        call: &Call,
        units: &[Unit],
    ) -> Option<Site> {
        let module = unit.text[qualifier.clone()].to_ascii_lowercase();
        let file = self.by_stem.get(&module).copied().flatten()?;
        let key = unit.text[call.span.clone()].to_ascii_lowercase();
        let local = *evidence(&units[file])?.exports.get(&key)?;
        Some(Site { file, local })
    }
}

fn resolve(file: usize, project: &crate::domain::code::ProjectView, names: &mut ForwardNames) {
    let unit = &project.units[file];
    let Some(e) = evidence(unit) else { return };
    if e.qualified.is_empty() || e.imports.is_empty() {
        return;
    }
    // Written imports and qualified calls are ordered by extraction.
    let mut imports = e.imports.iter().peekable();
    let mut modules = ImportedModules::default();
    for &(local, ref qualifier) in &e.qualified {
        let call = &unit.analysis.calls[local];
        let available = std::iter::from_fn(|| imports.next_if(|span| span.end <= call.span.start));
        available.for_each(|span| modules.import(unit, span, project.paths()));
        if let Some(target) = modules.target(unit, qualifier, call, project.units) {
            names.bind_one(Site { file, local }, target);
        }
    }
}

fn literal_name(token: &str) -> Option<&str> {
    let name = token.trim_matches(['\'', '"']);
    (!name.is_empty()
        && (name == "*"
            || name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))))
    .then_some(name)
}

pub(in crate::domain::code) struct PowershellResolver;
impl crate::domain::code::project_resolver::ProjectResolver for PowershellResolver {
    fn refine(
        &self,
        project: &crate::domain::code::ProjectView,
        prior: &[Option<usize>],
        keep: &mut [bool],
        _: &mut dyn FnMut(usize) -> std::io::Result<crate::domain::code::Analysis>,
    ) -> std::io::Result<()> {
        let reusable = project
            .units
            .iter()
            .enumerate()
            .filter(|(id, _)| prior[*id].is_some())
            .filter_map(|(id, u)| evidence(u).map(|e| (id, u, e)));
        for (id, u, e) in reusable {
            // A qualified command consults the named module's own export table.
            keep[id] = e
                .imports
                .iter()
                .filter_map(|s| super::shell::path_for(u.path, &u.text[s.clone()], project.paths()))
                .all(|m| prior[m].is_some());
        }
        Ok(())
    }
    fn reads(&self, _: &str, lang: Option<crate::domain::code::Language>) -> bool {
        use crate::domain::code::Language;
        lang == Some(Language::Powershell)
    }
    fn bind(
        &self,
        files: &[usize],
        project: &crate::domain::code::project_resolver::ProjectView,
        output: &mut crate::domain::code::ForwardNames,
    ) {
        for &id in files {
            resolve(id, project, output);
        }
    }
}
