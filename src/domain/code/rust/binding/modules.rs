//! Select visible Rust module edges and follow them from crate entry points.
use super::{Address, HashMap, HashSet, Module, Path, PathBuf, ResolverState, Unit, config};

/// A conventional foo.rs owns foo/, while a crate root, an explicit #[path]
/// source and mod.rs all resolve children relative to their physical parent.
fn module_stem(path: &Path) -> Option<&std::ffi::OsStr> {
    path.file_stem().filter(|&stem| stem != "mod")
}

fn conventional_entry(path: &Path) -> bool {
    matches!(
        path.file_stem().and_then(|s| s.to_str()),
        Some("main" | "lib")
    ) || matches!(
        path.parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str()),
        Some("tests" | "examples" | "benches" | "bin")
    )
}

struct Sources<'a, 's> {
    paths: &'a HashMap<&'a str, usize>,
    units: &'a [Unit<'s>],
}

impl Sources<'_, '_> {
    fn file(&self, path: &Path) -> Option<Address> {
        let file = *self.paths.get(path.to_str()?)?;
        self.units[file].analysis.rust.as_ref()?;
        Some((file, 0))
    }

    fn external(&self, dir: &Path, key: &str) -> Option<Address> {
        let next = dir.join(key);
        match (
            self.file(&next.with_extension("rs")),
            self.file(&next.join("mod.rs")),
        ) {
            (Some(addr), None) | (None, Some(addr)) => Some(addr),
            _ => None,
        }
    }
}

// An external module named by #[path] resolves its own descendants from
// the physical parent. A conventional foo.rs resolves them under foo/.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FileContext {
    file: usize,
    explicit: bool,
}

type ModuleLink<'a> = (Address, &'a str, Address);

#[derive(Default)]
struct ContextEdges<'a> {
    modules: Vec<ModuleLink<'a>>,
    children: Vec<FileContext>,
    dependencies: Vec<usize>,
}

struct Directory<'a> {
    children: &'a Path,
    written: &'a Path,
}

impl Sources<'_, '_> {
    fn context(&self, file: usize, explicit: bool) -> FileContext {
        let path = Path::new(self.units[file].path);
        let explicit = explicit && module_stem(path).is_some();
        FileContext { file, explicit }
    }

    fn directory(&self, input: FileContext) -> (&Path, PathBuf) {
        let path = Path::new(self.units[input.file].path);
        let parent = path.parent().unwrap_or(Path::new(""));
        let child = if input.explicit {
            None
        } else {
            module_stem(path)
        };
        (
            parent,
            child.map_or_else(|| parent.to_owned(), |stem| parent.join(stem)),
        )
    }

    fn physical(
        &self,
        directory: &Directory,
        name: &str,
        written: Option<&str>,
    ) -> Option<Address> {
        match written {
            Some(path) => self.file(&crate::domain::code::relative_path(
                directory.written,
                path,
            )?),
            None => self.external(directory.children, name),
        }
    }

    fn edges<'e>(
        &self,
        input: FileContext,
        evidence: &'e super::super::Evidence,
        config: &config::FileConfig,
    ) -> ContextEdges<'e> {
        self.walk(input, evidence, Selection::Current(config))
            .expect("current module discovery is unbounded")
    }

    fn walk<'e>(
        &self,
        input: FileContext,
        evidence: &'e super::super::Evidence,
        selection: Selection<'_>,
    ) -> Option<ContextEdges<'e>> {
        let mut edges = ContextEdges::default();
        if evidence.scopes[0].children.is_empty() {
            return Some(edges);
        }
        let (parent, root) = self.directory(input);
        let mut pending = vec![(0, root)];
        let mut former = selection.former().then(FormerVisits::default);
        while let Some((scope, base)) = pending.pop() {
            if !FormerVisits::admit(former.as_mut(), scope, &base, pending.len())? {
                continue;
            }
            let written = (scope == 0).then_some(parent).unwrap_or(&base);
            let directory = Directory {
                children: &base,
                written,
            };
            let mut walk = ScopeWalk {
                sources: self,
                evidence,
                directory,
                selection,
                parent: (input.file, scope),
                edges: &mut edges,
                pending: &mut pending,
            };
            walk.scan(scope);
        }
        Some(edges)
    }
}

#[derive(Default)]
struct FormerVisits {
    seen: HashSet<(usize, PathBuf)>,
}

impl FormerVisits {
    fn admit(state: Option<&mut Self>, scope: usize, base: &Path, pending: usize) -> Option<bool> {
        let Some(state) = state else {
            return Some(true);
        };
        if state.seen.len() + pending > 4096 {
            return None;
        }
        Some(state.seen.insert((scope, base.to_owned())))
    }
}

#[derive(Clone, Copy)]
enum ModuleSite {
    Inline(usize),
    External,
}

struct Alternative<'a> {
    site: ModuleSite,
    path: Option<&'a str>,
    selected: bool,
}

struct ScopeWalk<'a, 'e, 'u, 's> {
    sources: &'a Sources<'u, 's>,
    evidence: &'e super::super::Evidence,
    directory: Directory<'a>,
    selection: Selection<'a>,
    parent: Address,
    edges: &'a mut ContextEdges<'e>,
    pending: &'a mut Vec<(usize, PathBuf)>,
}
impl<'e> ScopeWalk<'_, 'e, '_, '_> {
    fn scan(&mut self, scope: usize) {
        for (name, variants) in &self.evidence.scopes[scope].children {
            self.named(name, variants);
        }
    }

    fn named(&mut self, name: &'e str, variants: &[(usize, Module)]) {
        let mut winner = None;
        let mut ambiguous = false;
        for (offset, module) in variants {
            let active = self.selection.active(*offset);
            module.paths(self.selection, |alternative| {
                if let Some(child) = self.candidate(name, alternative, active) {
                    ambiguous |= winner.replace(child).is_some();
                }
            });
        }
        if let Some(child) = winner.filter(|_| !ambiguous) {
            self.edges.modules.push((self.parent, name, child));
        }
    }
    fn candidate(
        &mut self,
        name: &str,
        candidate: Alternative<'_>,
        active: bool,
    ) -> Option<Address> {
        let eligible = active && candidate.selected;
        match candidate.site {
            ModuleSite::Inline(scope) if eligible => {
                self.inline_candidate(name, scope, candidate.path)
            }
            ModuleSite::Inline(_) => None,
            ModuleSite::External => self.external_candidate(name, candidate.path, eligible),
        }
    }

    fn inline_candidate(
        &mut self,
        name: &str,
        scope: usize,
        path: Option<&str>,
    ) -> Option<Address> {
        if !self.evidence.scopes[scope].children.is_empty() {
            self.pending
                .extend(self.directory.inline(name, path).map(|base| (scope, base)));
        }
        (!self.selection.former()).then_some((self.parent.0, scope))
    }

    fn external_candidate(
        &mut self,
        name: &str,
        path: Option<&str>,
        eligible: bool,
    ) -> Option<Address> {
        let child = self.sources.physical(&self.directory, name, path)?;
        self.edges.dependencies.push(child.0);
        if !eligible || self.selection.former() {
            return None;
        }
        self.edges
            .children
            .push(self.sources.context(child.0, path.is_some()));
        Some(child)
    }
}

#[derive(Clone, Copy)]
enum Selection<'a> {
    Current(&'a config::FileConfig),
    Former,
}

impl Selection<'_> {
    fn former(self) -> bool {
        matches!(self, Self::Former)
    }
    fn active(self, offset: usize) -> bool {
        match self {
            Self::Current(config) => config::active(&config.ranges, offset),
            Self::Former => true,
        }
    }
}

impl Directory<'_> {
    fn inline(&self, key: &str, written: Option<&str>) -> Option<PathBuf> {
        match written {
            Some(path) => crate::domain::code::relative_path(self.written, path),
            None => Some(self.children.join(key)),
        }
    }
}
impl Module {
    fn paths(&self, selection: Selection, mut visit: impl FnMut(Alternative<'_>)) {
        let (site, paths) = match self {
            Module::File(path) => {
                visit(Alternative {
                    site: ModuleSite::External,
                    path: path.as_deref(),
                    selected: true,
                });
                return;
            }
            Module::Inline(scope) => {
                visit(Alternative {
                    site: ModuleSite::Inline(*scope),
                    path: None,
                    selected: true,
                });
                return;
            }
            Module::Paths(paths) => (ModuleSite::External, paths),
            Module::InlinePaths(scope, paths) => (ModuleSite::Inline(*scope), paths),
        };
        let chosen = PathChoice::new(paths, selection);
        for (index, (_, path)) in paths.iter().enumerate() {
            visit(Alternative {
                site,
                path: Some(path),
                selected: chosen.accepts(Some(index)),
            });
        }
        if paths.iter().all(|(guards, _)| !guards.is_empty()) {
            visit(Alternative {
                site,
                path: None,
                selected: chosen.accepts(None),
            });
        }
    }
}

enum PathChoice {
    Any,
    Exact(Result<Option<usize>, ()>),
}

impl PathChoice {
    fn new(paths: &[(Vec<config::Predicate>, String)], selection: Selection<'_>) -> Self {
        match selection {
            Selection::Current(config) => Self::Exact(selected_path(paths, config.target)),
            Selection::Former => Self::Any,
        }
    }

    fn accepts(&self, index: Option<usize>) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(selected) => *selected == Ok(index),
        }
    }
}

// An undecidable or multiply selected path cannot prove a module edge,
// even when only one possible file happens to exist in the snapshot.
fn selected_path(
    options: &[(Vec<config::Predicate>, String)],
    target: Option<config::Target>,
) -> Result<Option<usize>, ()> {
    let mut selected = None;
    for (index, (guards, _)) in options.iter().enumerate() {
        match config::Predicate::combine(guards, target, true) {
            Some(false) => {}
            Some(true) if selected.is_none() => selected = Some(index),
            _ => return Err(()),
        }
    }
    Ok(selected)
}

pub(in crate::domain::code::rust) fn previous_neighbors(
    file: usize,
    project: &crate::domain::code::ProjectView,
    evidence: &super::super::Evidence,
    out: &mut Vec<usize>,
) {
    let sources = Sources {
        paths: project.paths(),
        units: project.units,
    };
    // Previous facts have no reliable old target. Follow each possible physical
    // inline directory, but publish only the external files as dirty neighbors.
    // Keep the all-Rust fallback for path sets too large to enumerate.
    let mut contexts = vec![sources.context(file, false), sources.context(file, true)];
    contexts.dedup();
    for context in contexts {
        let Some(edges) = sources.walk(context, evidence, Selection::Former) else {
            out.extend(
                project
                    .units
                    .iter()
                    .enumerate()
                    .filter_map(|(file, u)| u.analysis.rust.is_some().then_some(file)),
            );
            return;
        };
        out.extend(edges.dependencies);
    }
}

// A source may be reached both conventionally and through #[path]. Its
// physical reference sites can claim only edges proved in both contexts.
fn agreed_links([ordinary, explicit]: [Option<Vec<ModuleLink>>; 2]) -> Vec<ModuleLink> {
    match (ordinary, explicit) {
        (Some(mut links), Some(other)) => {
            let other: HashSet<_> = other.into_iter().collect();
            links.retain(|link| other.contains(link));
            links
        }
        (links, other) => links.or(other).unwrap_or_default(),
    }
}

#[derive(Default)]
struct Discovered<'e> {
    graph: HashMap<FileContext, ContextEdges<'e>>,
    referred: HashSet<usize>,
}

impl<'e> Discovered<'e> {
    fn collect(
        &mut self,
        pending: &mut Vec<FileContext>,
        sources: &Sources<'e, '_>,
        configs: &[config::FileConfig],
        components: &mut Option<&mut super::components::Components>,
    ) {
        use std::collections::hash_map::Entry;
        while let Some(context) = pending.pop() {
            let Entry::Vacant(slot) = self.graph.entry(context) else {
                continue;
            };
            let file = context.file;
            let evidence = sources.units[file].analysis.rust.as_ref().unwrap();
            let edges = sources.edges(context, evidence, &configs[file]);
            self.referred.extend(
                edges
                    .dependencies
                    .iter()
                    .copied()
                    .filter(|&dep| dep != file),
            );
            join_dependencies(components.as_deref_mut(), file, &edges);
            pending.extend(edges.children.iter().filter(|c| c.explicit).copied());
            slot.insert(edges);
        }
    }

    fn roots<'a>(&'a self, targets: &'a HashSet<usize>) -> impl Iterator<Item = FileContext> + 'a {
        self.graph
            .keys()
            .filter(|c| !c.explicit && !self.referred.contains(&c.file))
            .filter(|c| !targets.contains(&c.file))
            .copied()
    }
}

fn join_dependencies(
    components: Option<&mut super::components::Components>,
    file: usize,
    edges: &ContextEdges,
) {
    let Some(groups) = components else { return };
    for &dependency in &edges.dependencies {
        groups.join(file, dependency);
    }
}

impl ResolverState<'_, '_> {
    pub(super) fn discover_modules(
        &mut self,
        paths: &HashMap<&str, usize>,
        targets: &HashSet<usize>,
        mut components: Option<&mut super::components::Components>,
    ) {
        let sources = Sources {
            paths,
            units: self.units,
        };
        let mut pending: Vec<_> = self
            .units
            .iter()
            .enumerate()
            .filter_map(|(file, u)| {
                u.analysis.rust.as_ref().map(|_| FileContext {
                    file,
                    explicit: false,
                })
            })
            .collect();
        let entries: HashSet<_> = targets.iter().map(|&f| sources.context(f, true)).collect();
        pending.extend(entries.iter().copied());
        let mut discovered = Discovered::default();
        discovered.collect(&mut pending, &sources, &self.configs, &mut components);
        // Naming is only evidence for otherwise unclaimed standalone roots.
        // A written mod declaration already defines the directory of its child.
        let mut roots: Vec<_> = discovered
            .roots(targets)
            .map(|c| {
                let explicit = conventional_entry(Path::new(self.units[c.file].path));
                sources.context(c.file, explicit)
            })
            .collect();
        pending.extend(roots.iter().filter(|c| c.explicit).copied());
        discovered.collect(&mut pending, &sources, &self.configs, &mut components);
        roots.retain(|c| !discovered.referred.contains(&c.file));
        let mut graph = discovered.graph;
        roots.extend(entries);
        // Reaching a context consumes its rows; cycles cannot enqueue its children twice.
        let mut reached: HashMap<usize, [Option<Vec<ModuleLink>>; 2]> = HashMap::new();
        while let Some(context) = roots.pop() {
            let Some(edges) = graph.remove(&context) else {
                continue;
            };
            roots.extend(edges.children);
            reached.entry(context.file).or_default()[usize::from(context.explicit)] =
                Some(edges.modules);
        }
        for (file, modes) in reached {
            self.activate_module(file, agreed_links(modes));
        }
    }

    fn activate_module(&mut self, file: usize, links: Vec<ModuleLink>) {
        self.active_files[file] = true;
        for (parent, name, child) in links {
            self.link_module(parent, name, child);
        }
    }

    fn link_module(&mut self, parent: Address, key: &str, child: Address) {
        self.modules
            .entry(parent)
            .or_default()
            .insert(key.to_owned(), child);
        self.parents
            .entry(child)
            .and_modify(|old| {
                self.aliased_modules.insert(child);
                if *old != Some(parent) {
                    *old = None;
                }
            })
            .or_insert(Some(parent));
    }
}
