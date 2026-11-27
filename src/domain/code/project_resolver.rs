//! Partition producer units once, retaining a shared view of all support files.
use super::{ForwardNames, Language, Site, Unit, cfamily, direct, rust, script};
use std::collections::HashMap;

pub(crate) struct ProjectView<'a, 'src> {
    pub units: &'a [Unit<'src>],
    pub(super) before: Option<&'a std::collections::BTreeMap<usize, String>>,
    paths: std::cell::OnceCell<HashMap<&'a str, usize>>,
    modules: std::cell::OnceCell<script::Modules>,
    pub(super) shell: std::cell::OnceCell<direct::SourceEdges>,
    pub(super) proto: std::cell::OnceCell<direct::ProtoGraph<'a>>,
    rust: std::cell::OnceCell<rust::Prepared<'a, 'src>>,
}
impl<'a, 's> ProjectView<'a, 's> {
    pub(crate) fn new(units: &'a [Unit<'s>]) -> Self {
        Self {
            units,
            before: None,
            paths: Default::default(),
            modules: Default::default(),
            proto: Default::default(),
            shell: Default::default(),
            rust: Default::default(),
        }
    }
    pub(crate) fn with_changes(
        units: &'a [Unit<'s>],
        before: Option<&'a std::collections::BTreeMap<usize, String>>,
    ) -> Self {
        Self {
            before,
            ..Self::new(units)
        }
    }
    pub(super) fn paths(&self) -> &HashMap<&'a str, usize> {
        self.paths.get_or_init(|| {
            self.units
                .iter()
                .enumerate()
                .map(|(id, u)| (u.path, id))
                .collect()
        })
    }
    pub(super) fn rust(&self, incremental: bool) -> &rust::Prepared<'a, 's> {
        self.rust
            .get_or_init(|| rust::Prepared::new(self.units, self.paths(), incremental))
    }
    pub(super) fn modules(&self) -> &script::Modules {
        self.modules
            .get_or_init(|| script::Modules::link(self.units, self.paths()))
    }
}
/// Stable path membership makes a missing path observation reusable. Only
/// present imports can carry a source change through this generation.
pub(super) fn retain_import_clients(
    prior: &[Option<usize>],
    keep: &mut [bool],
    edges: impl Iterator<Item = (usize, usize)>,
    clients: impl Iterator<Item = usize>,
) {
    let mut incoming = vec![Vec::new(); prior.len()];
    for (client, target) in edges {
        incoming[target].push(client)
    }
    let dirty = affected_clients(prior, &incoming);
    for file in clients {
        keep[file] = !dirty[file];
    }
}

pub(super) fn affected_clients(prior: &[Option<usize>], incoming: &[Vec<usize>]) -> Vec<bool> {
    let mut dirty: Vec<_> = prior.iter().map(Option::is_none).collect();
    let mut pending: Vec<_> = (0..prior.len()).filter(|&f| dirty[f]).collect();
    while let Some(target) = pending.pop() {
        for &file in &incoming[target] {
            if !std::mem::replace(&mut dirty[file], true) {
                pending.push(file)
            }
        }
    }
    dirty
}
pub(super) trait ProjectResolver {
    fn refine(
        &self,
        _project: &ProjectView,
        _prior: &[Option<usize>],
        _keep: &mut [bool],
        _old: &mut dyn FnMut(usize) -> std::io::Result<super::Analysis>,
    ) -> std::io::Result<()> {
        Ok(())
    }
    fn refine_config(&self) -> bool {
        false
    }
    fn isolated(&self, _unit: &Unit) -> bool {
        false
    }
    fn reads(&self, path: &str, lang: Option<Language>) -> bool;
    fn bind(&self, files: &[usize], project: &ProjectView, output: &mut ForwardNames);
}
#[derive(Clone, Copy)]
enum EvidenceSource {
    Rust,
    Script,
    Direct,
    Calls,
    Local,
}

impl EvidenceSource {
    fn available(self, facts: &super::Analysis) -> bool {
        match self {
            Self::Rust => facts.rust.is_some(),
            Self::Script => facts.script.is_some(),
            Self::Direct => facts.direct_evidence.is_some(),
            Self::Calls => !facts.calls.is_empty(),
            Self::Local => false,
        }
    }
}

struct ProviderSpec {
    evidence: EvidenceSource,
    resolver: &'static dyn ProjectResolver,
}

// The declaration order is also the publication and refinement order.
macro_rules! providers {
    ($($lane:ident : $source:ident($resolver:expr)),+ $(,)?) => {
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub(super) enum Provider { $($lane),+ }

        const PROVIDERS: &[ProviderSpec] = &[
            $(ProviderSpec {
                evidence: EvidenceSource::$source,
                resolver: $resolver,
            }),+
        ];
    };
}

providers! {
    Proto: Direct(&direct::ProtoResolver),
    Shell: Direct(&direct::ShellResolver),
    Csharp: Direct(&cfamily::csharp::CsharpResolver),
    Rust: Rust(&rust::analyzer::RustResolver),
    Script: Script(&script::analyzer::ScriptResolver),
    Local: Local(&LocalResolver),
    Powershell: Direct(&direct::powershell::PowershellResolver),
    Starlark: Calls(&direct::starlark::StarlarkResolver),
}

impl Provider {
    fn project(u: &Unit) -> Option<Self> {
        let facts = u.analysis;
        if facts.calls.is_empty()
            && facts.rust.is_none()
            && facts.script.is_none()
            && facts.direct_evidence.is_none()
        {
            return None;
        }
        let lane = Language::for_path(u.path)?.descriptor().resolver?;
        lane.spec().evidence.available(facts).then_some(lane)
    }

    fn spec(self) -> &'static ProviderSpec {
        &PROVIDERS[self as usize]
    }
}

#[derive(Clone, Copy, Default)]
enum Changes {
    #[default]
    None,
    Source,
    Configuration,
}

impl Changes {
    fn observed(&mut self, language: Option<Language>) {
        if language.is_none() {
            *self = Self::Configuration;
        } else if matches!(self, Self::None) {
            *self = Self::Source;
        }
    }

    fn reusable(self, resolver: &dyn ProjectResolver, unit: &Unit) -> bool {
        match self {
            Self::None => true,
            Self::Source => resolver.isolated(unit),
            Self::Configuration => false,
        }
    }

    fn refine(self, resolver: &dyn ProjectResolver) -> bool {
        match self {
            Self::None => false,
            Self::Source => true,
            Self::Configuration => resolver.refine_config(),
        }
    }
}

/// Inventory and rule versions are checked by the caller. A provider owns
/// the file contents it observes; path-only observations survive these edits.
pub(super) fn consumes_support_text(path: &str) -> bool {
    PROVIDERS.iter().any(|p| p.resolver.reads(path, None))
}

fn observe_change(changes: &mut [Changes; PROVIDERS.len()], path: &str) {
    let language = Language::for_path(path);
    for (provider, change) in PROVIDERS.iter().zip(changes) {
        if provider.resolver.reads(path, language) {
            change.observed(language);
        }
    }
}

pub(crate) fn retained_files(
    project: &ProjectView,
    prior: &[Option<usize>],
    mut old: impl FnMut(usize) -> std::io::Result<super::Analysis>,
) -> std::io::Result<Option<Vec<bool>>> {
    let units = project.units;
    let mut changes = [Changes::None; PROVIDERS.len()];
    for (unit, _) in units.iter().zip(prior).filter(|(_, old)| old.is_none()) {
        observe_change(&mut changes, unit.path);
    }
    let mut keep: Vec<_> = units
        .iter()
        .zip(prior)
        .map(|(u, old)| {
            if old.is_none() {
                return false;
            }
            Provider::project(u).is_some_and(|provider| {
                changes[provider as usize].reusable(provider.spec().resolver, u)
            })
        })
        .collect();
    for (provider, change) in PROVIDERS.iter().zip(changes) {
        if change.refine(provider.resolver) {
            provider
                .resolver
                .refine(project, prior, &mut keep, &mut old)?;
        }
    }
    Ok(keep.iter().any(|&k| k).then_some(keep))
}

pub(super) fn resolve(project: &ProjectView, output: &mut ForwardNames, retained: &[bool]) {
    let units = project.units;
    let mut batches: [Vec<usize>; PROVIDERS.len()] = std::array::from_fn(|_| Vec::new());
    let changed = units
        .iter()
        .enumerate()
        .filter(|(file, _)| retained.get(*file) != Some(&true));
    for (file, unit) in changed {
        schedule_file(&mut batches, file, unit);
    }
    for (provider, files) in PROVIDERS.iter().zip(&batches) {
        if !files.is_empty() {
            provider.resolver.bind(files, project, output);
        }
    }
}

fn schedule_file(batches: &mut [Vec<usize>; PROVIDERS.len()], file: usize, unit: &Unit) {
    if let Some(provider) = Provider::project(unit) {
        batches[provider as usize].push(file);
    }
    if !unit.analysis.direct.is_empty() {
        batches[Provider::Local as usize].push(file);
    }
}

struct LocalResolver;
impl ProjectResolver for LocalResolver {
    fn reads(&self, _: &str, _: Option<Language>) -> bool {
        false
    }
    fn bind(&self, files: &[usize], project: &ProjectView, output: &mut ForwardNames) {
        for &file in files {
            let facts = project.units[file].analysis;
            for (local, targets) in &facts.direct {
                let source = Site {
                    file,
                    local: *local,
                };
                match targets.as_slice() {
                    [] => {}
                    &[local] => output.bind_one(source, Site { file, local }),
                    many => output.bind(
                        source,
                        many.iter().map(|&local| Site { file, local }).collect(),
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_changed_manifest_invalidates_its_provider_and_preserves_unrelated_producers() {
        let docs = [
            ("Cargo.toml", ""),
            ("lib.rs", "pub fn target() {}"),
            ("package.json", "{}"),
            ("lib.ts", "export function target() {}"),
            ("script.sh", "target() { :; }"),
        ];
        let parsed: Vec<_> = docs
            .iter()
            .map(|(p, t)| {
                Language::for_path(p).map_or_else(Default::default, |l| super::super::analyze(l, t))
            })
            .collect();
        let units: Vec<_> = docs
            .iter()
            .zip(&parsed)
            .map(|(&(path, text), analysis)| Unit {
                path,
                text,
                analysis,
            })
            .collect();
        for (edited, expected) in [
            (0, [false, false, false, true, true]),
            (2, [false, true, false, false, true]),
            (4, [false, true, false, true, false]),
        ] {
            let mut old: Vec<_> = (0..docs.len()).map(Some).collect();
            old[edited] = None;
            assert_eq!(
                retained_files(&ProjectView::new(&units), &old, |_| unreachable!())
                    .unwrap()
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn changing_a_script_keeps_unrelated_import_trees_and_invalidates_reexports() {
        let docs = [
            ("leaf.ts", "export function f(){}"),
            ("barrel.ts", "export * from './leaf';"),
            ("client.ts", "import {f} from './barrel'; f();"),
            ("other.ts", "export function g(){}"),
            ("independent.ts", "import {g} from './other'; g();"),
        ];
        let facts: Vec<_> = docs
            .iter()
            .map(|(_, text)| super::super::analyze(Language::Js, text))
            .collect();
        let units: Vec<_> = docs
            .iter()
            .zip(&facts)
            .map(|(&(path, text), analysis)| Unit {
                path,
                text,
                analysis,
            })
            .collect();
        assert_eq!(
            retained_files(
                &ProjectView::new(&units),
                &[None, Some(1), Some(2), Some(3), Some(4)],
                |_| unreachable!()
            )
            .unwrap()
            .unwrap(),
            [false, false, false, true, true]
        );
    }
    #[test]
    fn removed_rust_module_invalidates_its_former_tree_only() {
        let docs = [
            ("src/lib.rs", "fn now(){}"),
            ("src/leaf.rs", "mod child;"),
            ("src/leaf/child.rs", "fn target(){}"),
            ("independent.rs", "fn elsewhere(){}"),
        ];
        let facts: Vec<_> = docs
            .iter()
            .map(|(_, t)| super::super::analyze(Language::Rust, t))
            .collect();
        let units: Vec<_> = docs
            .iter()
            .zip(&facts)
            .map(|(&(path, text), analysis)| Unit {
                path,
                text,
                analysis,
            })
            .collect();
        let keep = retained_files(
            &ProjectView::new(&units),
            &[None, Some(1), Some(2), Some(3)],
            |id| {
                assert_eq!(id, 0);
                Ok(super::super::analyze(Language::Rust, "mod leaf;"))
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(keep, [false, false, false, true]);
    }
}
