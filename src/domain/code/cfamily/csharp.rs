//! Deferred overload selection when a .NET SDK project supplies the argument type alias.
//! C# string.AsSpan() is System.MemoryExtensions.AsSpan(string), returning
//! System.ReadOnlySpan<char>. Source methods of the same name can override this lookup.
use super::{Analyzer, Binding, Role};
use crate::domain::code::{Analysis, ForwardNames, Site, Unit, cache, direct};
use std::{collections::HashMap, path::Path};

mod project;

pub(in crate::domain::code) struct Evidence {
    pending: Vec<Pending>,
}

pub(super) struct Pending {
    call: usize,
    // Declaration ordinal and as-written parameter type; needed only until binding.
    candidates: Vec<(usize, String)>,
}

cache::record!(Evidence { pending });
cache::record!(Pending { call, candidates });

impl Analyzer<'_, '_> {
    pub(super) fn csharp_deferred(
        &self,
        name: usize,
        scope: usize,
        call: usize,
        candidates: &[&Binding<'_>],
    ) -> Option<Pending> {
        let input = name + 2;
        if self.pairs[name + 1] != input + 5
            || !(self.word(input + 1) == "."
                && self.word(input + 2) == "AsSpan"
                && self.word(input + 3) == "("
                && self.word(input + 4) == ")")
        {
            return None;
        }
        let receivers = self.lookup(scope, self.word(input), Some(Role::Value), input);
        let [receiver] = receivers.as_slice() else {
            return None;
        };
        if receiver.ty != Some("string")
            || self.t.windows(3).any(|t| {
                // An external extension namespace can change method selection.
                t[0].text == "using"
                    && t[1].kind == super::Kind::Word
                    && t[1].text != "System"
                    && !matches!(t[2].text, "=" | "(")
            })
        {
            return None;
        }
        let candidates = candidates
            .iter()
            .map(|b| {
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
                if !b.usable
                    || !self.lookup(scope, ty, Some(Role::Type), name).is_empty()
                    || self
                        .t
                        .windows(3)
                        .any(|t| t[0].text == "using" && t[1].text == *ty && t[2].text == "=")
                {
                    return None;
                }
                Some((b.id, ty.to_string()))
            })
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

pub(in crate::domain::code) fn resolve(
    units: &[Unit<'_>],
    paths: &HashMap<&str, usize>,
    names: &mut ForwardNames,
) {
    let pending = units.iter().enumerate().filter_map(|(file, unit)| {
        match unit.analysis.direct_evidence.as_deref()? {
            direct::Evidence::Csharp(ev) => Some((file, unit, ev)),
            _ => None,
        }
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
    let mut projects = HashMap::new();
    for (file, unit) in units
        .iter()
        .enumerate()
        .filter(|(_, u)| u.path.ends_with(".csproj"))
    {
        if let Some(dir) = Path::new(unit.path).parent() {
            projects
                .entry(dir)
                .and_modify(|v| *v = None)
                .or_insert(Some(file));
        }
    }
    let mut contexts = HashMap::new();
    for (file, unit, evidence) in pending {
        let Some(Some(proj)) = Path::new(unit.path)
            .ancestors()
            .skip(1)
            .find_map(|dir| projects.get(dir))
        else {
            continue;
        };
        let context = contexts
            .entry(*proj)
            .or_insert_with(|| project::config(*proj, units, paths));
        let Some(config) = context else { continue };
        for site in &evidence.pending {
            let mut eligible = site.candidates.iter().filter(|(_, ty)| config.contains(ty));
            if let Some((local, _)) = eligible.next()
                && eligible.next().is_none()
            {
                names.bind_one(
                    Site {
                        file,
                        local: site.call,
                    },
                    Site {
                        file,
                        local: *local,
                    },
                );
            }
        }
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
                    if path.ends_with(".cs") {
                        analyze(Language::Csharp, text)
                    } else {
                        Analysis::default()
                    }
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
