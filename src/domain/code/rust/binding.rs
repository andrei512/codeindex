mod components;
mod lookup;
mod modules;
pub(super) use modules::previous_neighbors;
mod path_tail;
mod prepare;
mod roots;
use super::*;
use prepare::{Action, prepare_paths};

type ModuleResults =
    HashMap<(Address, Namespace), HashMap<String, HashMap<Vec<String>, Vec<Site>>>>;
use std::sync::Arc;

pub(super) struct Resolver<'a, 's> {
    state: Arc<ResolverState<'a, 's>>,
    module_results: std::cell::RefCell<ModuleResults>,
}
impl<'a, 's> std::ops::Deref for Resolver<'a, 's> {
    type Target = ResolverState<'a, 's>;
    fn deref(&self) -> &Self::Target {
        &self.state
    }
}
impl std::ops::DerefMut for Resolver<'_, '_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::get_mut(&mut self.state).expect("resolver state is uniquely held during construction")
    }
}

pub(super) struct ResolverState<'a, 's> {
    pub(super) units: &'a [Unit<'s>],
    pub(super) modules: HashMap<Address, HashMap<String, Address>>,
    externs: Vec<Option<std::sync::Arc<HashMap<String, usize>>>>,
    pub(super) parents: HashMap<Address, Option<Address>>,
    // Two distinct incoming names also prevent a public identity when both
    // names come from the same parent and lexical super still resolves.
    pub(super) aliased_modules: HashSet<Address>,
    roots: HashMap<Address, Option<Address>>,
    configs: Vec<config::FileConfig>,
    active_files: Vec<bool>,
    invariant_modules: HashSet<Address>,
}
fn namespace_accepts(query: Namespace, binding: Namespace) -> bool {
    query == Namespace::Either || binding == Namespace::Either || query == binding
}

impl<'a, 's> Resolver<'a, 's> {
    pub(super) fn active(&self, file: usize, at: usize) -> bool {
        self.active_files[file] && config::active(&self.configs[file].ranges, at)
    }
    pub(super) fn active_declaration(&self, site: Site) -> bool {
        self.active_files[site.file] && self.configs[site.file].declaration_active(site.local)
    }
    pub(super) fn evidence(&self, file: usize) -> &Evidence {
        self.units[file].analysis.rust.as_ref().unwrap()
    }
    pub(super) fn child(&self, parent: Address, key: &str) -> Option<Address> {
        self.modules.get(&parent)?.get(key).copied()
    }
    pub(super) fn module_target(&self, addr: Address) -> Option<Site> {
        if addr.1 != 0 {
            return None;
        }
        let decls = &self.units[addr.0].analysis.declarations;
        decls
            .last()
            .filter(|d| d.kind == "source_file")
            .map(|_| Site {
                file: addr.0,
                local: decls.len() - 1,
            })
    }
    fn binding_site(&self, addr: Address, key: &str, binding: &Binding) -> Option<Site> {
        let site = Site {
            file: addr.0,
            local: binding.decl,
        };
        if !binding.local
            && self.units[site.file].analysis.declarations[site.local].kind == "mod_item"
        {
            let child = self.child(addr, key)?;
            return Some(self.module_target(child).unwrap_or(site));
        }
        Some(site)
    }

    pub(super) fn alias(&self, file: usize, scope: usize, key: &str) -> Option<&Vec<String>> {
        let mut candidates = self.evidence(file).scopes[scope]
            .imports
            .get(key)?
            .iter()
            .filter(|(at, _)| self.active(file, *at))
            .map(|(_, path)| path);
        let path = candidates.next()?;
        candidates.all(|p| p == path).then_some(path)
    }
}

pub(in crate::domain::code) struct Prepared<'a, 's> {
    resolver: Resolver<'a, 's>,
    targets: HashMap<usize, Arc<str>>,
    components: Option<Vec<usize>>,
}
impl<'a, 's> Prepared<'a, 's> {
    pub(in crate::domain::code) fn new(
        units: &'a [Unit<'s>],
        paths: &HashMap<&str, usize>,
        incremental: bool,
    ) -> Self {
        let cargo = cargo::inputs(units, paths);
        let mut r = Resolver {
            module_results: Default::default(),
            state: Arc::new(ResolverState {
                units,
                modules: HashMap::new(),
                externs: cargo.externs,
                parents: HashMap::new(),
                aliased_modules: HashSet::new(),
                roots: HashMap::new(),
                configs: config::file_configs(units),
                active_files: vec![false; units.len()],
                invariant_modules: HashSet::new(),
            }),
        };
        let mut components = incremental.then(|| components::Components::new(units.len()));
        if let Some(groups) = &mut components {
            for (file, deps) in r
                .externs
                .iter()
                .enumerate()
                .filter_map(|(file, deps)| Some((file, deps.as_ref()?)))
            {
                for &target in deps.values() {
                    groups.join(file, target);
                }
            }
        }
        r.discover_modules(paths, &cargo.entry_files, components.as_mut());
        r.index_module_roots();
        Self {
            resolver: r,
            targets: cargo.target_prefixes,
            components: components.map(components::Components::finish),
        }
    }
    pub(in crate::domain::code) fn refine(
        &self,
        prior: &[Option<usize>],
        keep: &mut [bool],
        former: &[usize],
    ) {
        let groups = self
            .components
            .as_ref()
            .expect("incremental Rust preparation");
        let mut dirty = vec![false; groups.len()];
        for &id in former {
            dirty[groups[id]] = true;
        }
        let rust_files = self
            .resolver
            .units
            .iter()
            .enumerate()
            .filter_map(|(id, unit)| unit.analysis.rust.as_ref().map(|_| id));
        for id in rust_files.clone().filter(|&id| prior[id].is_none()) {
            dirty[groups[id]] = true;
        }
        for id in rust_files {
            keep[id] = !dirty[groups[id]];
        }
    }
    pub(in crate::domain::code) fn bind(&self, files: &[usize], names: &mut ForwardNames) {
        let r = &self.resolver;
        let targets = &self.targets;
        std::thread::scope(|scope| {
            let worker = Resolver {
                state: Arc::clone(&r.state),
                module_results: Default::default(),
            };
            let identity = scope.spawn(move || {
                let mut labels = HashMap::new();
                identity::collect(&worker, files, targets, &mut labels);
                labels
            });
            let mut members = members::Table::build(r, files);
            let groups = prepare_paths(files, r, &members, names);
            // Receiver inference can consult both earlier bindings and an inference
            // cache. Commit results in baseline source order, including across files.
            for (file, pending) in groups.into_iter().flatten() {
                for (local, action) in pending {
                    let targets = match action {
                        Action::One(target) => {
                            names.bind_one(Site { file, local }, target);
                            continue;
                        }
                        Action::Resolved(targets) => targets,
                        Action::Receiver(previous) => {
                            members.previous_members(r, names, Site { file, local }, previous)
                        }
                    };
                    names.bind(Site { file, local }, targets);
                }
            }
            names
                .public_labels
                .extend(identity.join().expect("identity worker"));
        });
    }
}
