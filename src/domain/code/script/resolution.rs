mod exports;
mod file;
mod modules;
use super::{Binding, Evidence, ForwardNames, Language, Lookup, Site, Unit};
use modules::ModuleLinks;
pub(super) use modules::module_in;
use std::collections::HashMap;
fn binding_target(
    file: usize,
    binding: Option<&Binding>,
    imported: impl FnOnce(usize) -> Option<Site>,
) -> Option<Site> {
    match binding? {
        Binding::Local(local) | Binding::Namespace { local, .. } => Some(Site {
            file,
            local: *local,
        }),
        Binding::Import(index) => imported(*index),
        Binding::Package | Binding::Conflicting => None,
    }
}

/// Resolve the shape of a value before selecting a property. A result-type
/// hint names a function, while a declared or iterated type names the shape itself.
fn member_owner(
    site: Site,
    ev: &Evidence,
    mut binding: impl FnMut(usize, &(usize, std::ops::Range<usize>)) -> Option<Site>,
) -> Option<(Site, bool)> {
    if let Some(hint) = ev.result_types.get(&site.local) {
        return Some((binding(site.file, hint)?, true));
    }
    if let Some(hint) = ev.types.get(&site.local) {
        return Some((binding(site.file, hint)?, false));
    }
    if let Some(hint) = ev.iterated.get(&site.local) {
        let collection = binding(site.file, hint)?;
        if collection.file != site.file {
            return None;
        }
        let element = ev.element_types.get(&collection.local)?;
        return Some((binding(site.file, element)?, false));
    }
    Some((site, false))
}

struct MemberLookup<'a> {
    owner: Site,
    name: &'a str,
    called: bool,
}

fn member(
    units: &[Unit],
    lookup: MemberLookup<'_>,
    mut imported: impl FnMut(usize, usize) -> Option<Site>,
) -> Option<Site> {
    let MemberLookup { owner: site, name, called } = lookup;
    let u = &units[site.file];
    let ev = u.analysis.script.as_ref()?;
    let mut binding = |file: usize, (scope, span): &(usize, std::ops::Range<usize>)| {
        let unit = &units[file];
        let evidence = unit.analysis.script.as_ref()?;
        binding_target(
            file,
            evidence.binding(*scope, &unit.text[span.clone()]),
            |n| imported(file, n),
        )
    };
    let (mut owner, mut result) = if called {
        (site, true)
    } else {
        member_owner(site, ev, &mut binding)?
    };
    let mut dest = units[owner.file].analysis.script.as_ref()?;
    if result && let Some(hint) = dest.annotated_returns.get(&owner.local) {
        owner = binding(owner.file, hint)?;
        dest = units[owner.file].analysis.script.as_ref()?;
        result = false;
    }
    let shape = if result {
        dest.annotated_shapes
            .get(&owner.local)
            .or_else(|| dest.result_shapes.get(&owner.local))
    } else {
        dest.shapes.get(&owner.local)
    }?;
    dest.members.get(*shape, name).map(|local| Site {
        file: owner.file,
        local,
    })
}

/// Only a package binding can resolve a dotted import. Ordinary object chains
/// use their previous target without allocating a module name.
fn package_import(
    e: &Evidence,
    mut scope: usize,
    source: &str,
    longest_import: &std::cell::OnceCell<usize>,
) -> Option<usize> {
    if e.packages.is_empty() {
        return None;
    }
    let mut parts = source.split('.');
    let first = parts.next()?;
    let root = first.trim();
    loop {
        if let Some(binding) = e.scopes[scope].bindings.get(root) {
            if !matches!(binding, Binding::Package) {
                return None;
            }
            // A growing member chain cannot match once it exceeds every
            // written import. Spaces around dots do not count toward its length.
            let limit = *longest_import.get_or_init(|| e.packages.longest_name());
            let name = package_spelling(source, std::iter::once(first).chain(parts), limit)?;
            return e.packages.get(scope, &name);
        }
        scope = e.scopes[scope].parent?;
    }
}

/// Borrow a contiguous dotted name. Only whitespace around a separator
/// requires a normalized copy; stop as soon as no declared import could fit.
fn package_spelling<'a>(
    source: &'a str,
    parts: impl Iterator<Item = &'a str>,
    limit: usize,
) -> Option<std::borrow::Cow<'a, str>> {
    let mut normalized = None;
    let mut len = 0;
    for (i, raw) in parts.enumerate() {
        let part = raw.trim();
        let end = len + usize::from(i != 0) + part.len();
        (end <= limit).then_some(())?;
        append_package_part(&mut normalized, source, (i, len), (raw, part));
        len = end;
    }
    Some(normalized.map_or(std::borrow::Cow::Borrowed(source), std::borrow::Cow::Owned))
}

fn append_package_part(
    normalized: &mut Option<String>,
    source: &str,
    (i, len): (usize, usize),
    (raw, part): (&str, &str),
) {
    if normalized.is_none() && raw.len() != part.len() {
        *normalized = Some(source[..len].to_owned());
    }
    if let Some(name) = normalized {
        push_package_part(name, part, i != 0);
    }
}

fn push_package_part(name: &mut String, part: &str, dotted: bool) {
    if dotted {
        name.push('.');
    }
    name.push_str(part);
}

/// Python normally restricts star imports to module scope. Also preserve the
/// parser's lexical lookup for incomplete files with nested star imports.
pub(super) enum WildcardScopes {
    Module,
    Lexical(HashMap<usize, Vec<usize>>),
}

impl WildcardScopes {
    pub(super) fn index(imports: &[(usize, usize)]) -> Self {
        if imports.iter().all(|&(scope, _)| scope == 0) {
            return Self::Module;
        }
        let mut scopes = HashMap::<_, Vec<_>>::new();
        for &(scope, import) in imports {
            scopes.entry(scope).or_default().push(import);
        }
        Self::Lexical(scopes)
    }

    fn resolve(
        &self,
        evidence: &Evidence,
        mut scope: usize,
        mut exported: impl FnMut(usize) -> Option<Site>,
    ) -> Option<Site> {
        fn consensus(mut candidates: impl Iterator<Item = Site>) -> Option<Option<Site>> {
            let first = candidates.next()?;
            Some(candidates.all(|site| site == first).then_some(first))
        }
        match self {
            Self::Module => consensus(evidence.wildcards.iter().filter_map(|&(_, i)| exported(i)))?,
            Self::Lexical(scopes) => loop {
                if let Some(imports) = scopes.get(&scope)
                    && let Some(result) = consensus(imports.iter().filter_map(|&i| exported(i)))
                {
                    return result;
                }
                scope = evidence.scopes[scope].parent?;
            },
        }
    }
}

pub(in crate::domain::code) fn resolve_files(
    files: &[usize],
    units: &[Unit],
    paths: &HashMap<&str, usize>,
    out: &mut ForwardNames,
) {
    Modules::link(units, paths).bind_files(files, units, paths, out);
}

pub(in crate::domain::code) struct Modules(Vec<ModuleLinks>);
impl Modules {
    pub(in crate::domain::code) fn link(units: &[Unit], paths: &HashMap<&str, usize>) -> Self {
        Self(modules::link(units, paths))
    }
    pub(super) fn bind(
        &self,
        files: &[usize],
        project: &crate::domain::code::ProjectView,
        out: &mut ForwardNames,
    ) {
        self.bind_files(files, project.units, project.paths(), out);
    }

    fn bind_files(
        &self,
        files: &[usize],
        units: &[Unit],
        paths: &HashMap<&str, usize>,
        out: &mut ForwardNames,
    ) {
        let mut exports = exports::Cache::new(units, &self.0, paths);
        for &id in files {
            if let Some(file) = file::File::new(id, units, &self.0[id], &mut exports) {
                file.bind(out);
            }
        }
    }
    /// With path membership fixed, a missing import cannot appear through a
    /// source-only edit. Follow every present alternative, including Python's
    /// submodule fallback when a package gains or loses a conflicting export.
    pub(super) fn refine(
        &self,
        project: &crate::domain::code::ProjectView,
        prior: &[Option<usize>],
        keep: &mut [bool],
    ) {
        let mut dependents = vec![Vec::new(); prior.len()];
        let scripts = project
            .units
            .iter()
            .enumerate()
            .filter_map(|(id, u)| u.analysis.script.as_deref().map(|facts| (id, u, facts)));
        for (id, unit, facts) in scripts.clone().filter(|(id, _, _)| prior[*id].is_some()) {
            self.0[id].record_dependents(id, (unit, facts), project.paths(), &mut dependents);
        }
        let dirty = super::super::project_resolver::affected_clients(prior, &dependents);
        for (id, _, _) in scripts {
            keep[id] = !dirty[id];
        }
    }
}
impl ModuleLinks {
    fn record_dependents(
        &self,
        id: usize,
        (unit, facts): (&Unit, &Evidence),
        paths: &HashMap<&str, usize>,
        dependents: &mut [Vec<usize>],
    ) {
        let linked = self.imports.iter().flatten().chain(&self.stars).copied();
        let fallback = self
            .python
            .then(|| submodule_dependencies(unit, facts, paths))
            .into_iter()
            .flatten();
        for target in linked.chain(fallback) {
            dependents[target].push(id);
        }
    }
}

fn submodule_dependencies<'a>(
    unit: &'a Unit,
    evidence: &'a Evidence,
    paths: &'a HashMap<&str, usize>,
) -> impl Iterator<Item = usize> + 'a {
    let dir = std::path::Path::new(unit.path)
        .parent()
        .unwrap_or(std::path::Path::new(""));
    evidence.imports.iter().filter_map(move |import| {
        python_submodule(dir, &import.module, import.name.as_ref()?, paths)
    })
}

fn python_submodule(dir: &std::path::Path, module: &str, member: &str, paths: &HashMap<&str, usize>) -> Option<usize> {
    let dot = if module.ends_with('.') { "" } else { "." };
    module_in(dir, Language::Python, &format!("{module}{dot}{member}"), paths)
}
