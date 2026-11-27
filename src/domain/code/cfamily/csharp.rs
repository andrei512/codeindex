//! Deferred overload selection when a .NET SDK project supplies the argument type alias.
//! C# string.AsSpan() is System.MemoryExtensions.AsSpan(string), returning
//! System.ReadOnlySpan<char>. Source methods of the same name can override this lookup.
use super::{Analyzer, Binding, Role};
use crate::domain::code::{Analysis, ForwardNames, Site, Unit, cache, direct};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

mod project;

pub(in crate::domain::code) struct Evidence {
    pending: Vec<Pending>,
}

pub(super) struct Pending {
    call: usize,
    // Declaration ordinal and as-written parameter type; needed only until binding.
    candidates: Vec<(usize, String)>,
}

impl Pending {
    fn target(&self, aliases: &HashSet<String>) -> Option<usize> {
        super::unique(
            self.candidates
                .iter()
                .filter(|(_, spelling)| aliases.contains(spelling)),
        )
        .map(|(local, _)| *local)
    }
}

cache::record!(Evidence { pending });
cache::record!(Pending { call, candidates });

/// An imported namespace can provide a competing extension. A source alias
/// shadows the project spelling only for candidates using that alias.
pub(super) enum Usings<'a> {
    ExternalNamespace,
    Aliases(HashSet<&'a str>),
}

impl<'a> Usings<'a> {
    fn read(tokens: &[super::Token<'a>]) -> Self {
        let mut aliases = HashSet::new();
        for words in tokens.windows(3).filter(|w| w[0].text == "using") {
            match words[2].text {
                "=" => {
                    aliases.insert(words[1].text);
                }
                "(" => {}
                _ if words[1].kind == super::Kind::Word && words[1].text != "System" => {
                    return Self::ExternalNamespace;
                }
                _ => {}
            }
        }
        Self::Aliases(aliases)
    }
}

impl Analyzer<'_, '_> {
    fn project_candidate(
        &self,
        b: &Binding<'_>,
        aliases: &HashSet<&str>,
        scope: usize,
        name: usize,
    ) -> Option<(usize, String)> {
        let [(_, Some(ty))] = b.signature.as_ref()?.as_slice() else {
            return None;
        };
        // The file-local overload classifier stores only the first type word.
        // Reject arrays, generic signatures and by-ref arguments before using it.
        let start = self.out.declarations[b.id].span.start;
        let declaration = self.t.partition_point(|t| t.span.start < start);
        let open = declaration + 1;
        let ty_token = open + 1 + usize::from(self.word(open + 1) == "this");
        if self.word(ty_token) != *ty || self.pairs[open] != ty_token + 2 {
            return None;
        }
        // A source-level alias or local type makes the project spelling insufficient.
        if aliases.contains(ty)
            || self
                .lookup(scope, ty, Some(Role::Type), name)
                .next()
                .is_some()
        {
            return None;
        }
        Some((b.id, ty.to_string()))
    }

    pub(super) fn csharp_deferred(
        &self,
        name: usize,
        scope: usize,
        call: usize,
        candidates: &[&Binding<'_>],
    ) -> Option<Pending> {
        let input = name + 2;
        if self.pairs[name + 1] != input + 5
            || !self
                .t
                .get(input + 1..input + 5)?
                .iter()
                .map(|token| token.text)
                .eq([".", "AsSpan", "(", ")"])
        {
            return None;
        }
        let receivers = self.lookup(scope, self.word(input), Some(Role::Value), input);
        let receiver = super::unique(receivers)?;
        if receiver.ty != Some("string") {
            return None;
        }
        let Usings::Aliases(aliases) = self.csharp_usings.get_or_init(|| Usings::read(self.t))
        else {
            return None;
        };
        let candidates = candidates
            .iter()
            .map(|b| self.project_candidate(b, aliases, scope, name))
            .collect::<Option<Vec<_>>>()?;
        Some(Pending { call, candidates })
    }
}

pub(super) fn record(out: &mut Analysis, pending: Pending) {
    let ev = out.direct_evidence.get_or_insert_with(|| {
        Box::new(direct::Evidence::Csharp(Evidence {
            pending: Vec::new(),
        }))
    });
    if let direct::Evidence::Csharp(ev) = ev.as_mut() {
        ev.pending.push(pending);
    }
}

fn evidence<'a>(unit: &Unit<'a>) -> Option<&'a Evidence> {
    match unit.analysis.direct_evidence.as_deref()? {
        direct::Evidence::Csharp(e) => Some(e),
        _ => None,
    }
}

pub(in crate::domain::code) fn resolve(
    files: &[usize],
    units: &[Unit<'_>],
    paths: &HashMap<&str, usize>,
    names: &mut ForwardNames,
) {
    let pending = files.iter().filter_map(|&file| {
        let unit = &units[file];
        Some((file, unit, evidence(unit)?))
    });
    let mut pending = pending.peekable();
    if pending.peek().is_none() {
        return;
    }
    // Prefer missing an edge to inventing a framework method when source declares
    // another AsSpan. Include dependencies in the checkout, across project roots.
    if names
        .by_name
        .get("AsSpan")
        .is_some_and(|sites| sites.iter().any(|s| units[s.file].path.ends_with(".cs")))
    {
        return;
    }
    let mut projects = project::Projects::new(units, paths);
    for (file, unit, evidence) in pending {
        let Some(config) = projects.for_source(unit.path) else {
            continue;
        };

        evidence.bind_project(file, config, names);
    }
}

impl Evidence {
    fn bind_project(&self, file: usize, config: &HashSet<String>, names: &mut ForwardNames) {
        let targets = self
            .pending
            .iter()
            .filter_map(|site| site.target(config).map(|local| (site.call, local)));
        for (call, local) in targets {
            names.bind_one(Site { file, local: call }, Site { file, local });
        }
    }
}

enum ChangedInput {
    Source,
    Project,
    Other,
}

impl ChangedInput {
    fn classify(
        path: &str,
        resolver: &impl crate::domain::code::project_resolver::ProjectResolver,
    ) -> Self {
        if path.ends_with(".cs") {
            return Self::Source;
        }
        if resolver.reads(path, None) {
            return Self::Project;
        }
        Self::Other
    }
}

fn same_competitor(
    project: &crate::domain::code::ProjectView,
    id: usize,
    unit: &Unit,
    old: &mut dyn FnMut(usize) -> std::io::Result<Analysis>,
) -> std::io::Result<bool> {
    let Some(before) = project.before.and_then(|sources| sources.get(&id)) else {
        return Ok(false);
    };
    if !before.contains("AsSpan") && !unit.text.contains("AsSpan") {
        return Ok(true);
    }
    Ok(declares_competitor(&old(id)?, before) == declares_competitor(unit.analysis, unit.text))
}

fn declares_competitor(analysis: &Analysis, source: &str) -> bool {
    analysis.declarations.iter().any(|declaration| {
        declaration.searchable() && source.get(declaration.span.clone()) == Some("AsSpan")
    })
}

fn changed_project_dirs<'a>(
    project: &'a crate::domain::code::ProjectView,
    prior: &[Option<usize>],
    resolver: &CsharpResolver,
    old: &mut dyn FnMut(usize) -> std::io::Result<Analysis>,
) -> std::io::Result<Option<HashSet<&'a Path>>> {
    let mut config_dirs = HashSet::new();
    for (id, unit) in project
        .units
        .iter()
        .enumerate()
        .filter(|(id, _)| prior[*id].is_none())
    {
        match ChangedInput::classify(unit.path, resolver) {
            ChangedInput::Project => config_dirs.extend(Path::new(unit.path).parent()),
            ChangedInput::Source if !same_competitor(project, id, unit, old)? => return Ok(None),
            _ => {}
        }
    }
    Ok(Some(config_dirs))
}

pub(in crate::domain::code) struct CsharpResolver;
impl crate::domain::code::project_resolver::ProjectResolver for CsharpResolver {
    fn refine_config(&self) -> bool {
        true
    }
    fn refine(
        &self,
        project: &crate::domain::code::ProjectView,
        prior: &[Option<usize>],
        keep: &mut [bool],
        old: &mut dyn FnMut(usize) -> std::io::Result<Analysis>,
    ) -> std::io::Result<()> {
        let Some(config_dirs) = changed_project_dirs(project, prior, self, old)? else {
            return Ok(());
        };
        for (id, unit) in project.units.iter().enumerate() {
            if prior[id].is_none() || evidence(unit).is_none() {
                continue;
            }
            keep[id] = config_dirs.is_empty()
                || !Path::new(unit.path)
                    .ancestors()
                    .any(|dir| config_dirs.contains(dir));
        }
        Ok(())
    }

    fn reads(&self, path: &str, lang: Option<crate::domain::code::Language>) -> bool {
        use crate::domain::code::Language;
        lang == Some(Language::Csharp)
            || path.ends_with(".csproj")
            || matches!(
                path.rsplit('/').next(),
                Some("Directory.Build.props" | "Directory.Build.targets")
            )
    }
    fn bind(
        &self,
        files: &[usize],
        project: &crate::domain::code::project_resolver::ProjectView,
        output: &mut crate::domain::code::ForwardNames,
    ) {
        resolve(files, project.units, project.paths(), output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::code::{Language, Names, analyze};

    #[test]
    fn competing_extension_method_prevents_framework_binding() {
        let src = "static class C { static int Parse(string a) { return 1; } static int Parse(View a) { return 2; } static int Run(string text) { return Parse(text.AsSpan()); } }";
        let project = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><ImplicitUsings>enable</ImplicitUsings></PropertyGroup><ItemGroup><Using Alias="View" Include="System.ReadOnlySpan&lt;char&gt;" /></ItemGroup></Project>"#;
        let extension =
            "static class E { public static string AsSpan(this string input) { return input; } }";
        for (other, expected) in [("", 1), (extension, 0)] {
            let files = [
                ("dir/C.cs", src),
                ("dir/E.cs", other),
                ("dir/Demo.csproj", project),
            ];
            let local: Vec<_> = files
                .iter()
                .map(|(path, text)| {
                    Language::for_path(path)
                        .map_or_else(Analysis::default, |lang| analyze(lang, text))
                })
                .collect();
            let units: Vec<_> = files
                .iter()
                .zip(&local)
                .map(|(&(path, text), analysis)| Unit {
                    path,
                    text,
                    analysis,
                })
                .collect();
            let names = Names::build(&units);
            let offset = src.rfind("Parse").unwrap();
            let call = local[0]
                .calls
                .iter()
                .position(|c| c.span.start == offset)
                .unwrap();
            assert_eq!(
                names
                    .targets
                    .get(&Site {
                        file: 0,
                        local: call
                    })
                    .map_or(0, Vec::len),
                expected
            );
        }
    }
}
