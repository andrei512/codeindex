//! Resolve lexical names, module paths and imports in their Rust namespace.
use super::path_tail::Tail;
use super::*;

type Trail<'a> = Option<&'a Visit<'a>>;

enum AliasLookup {
    Absent,
    Cycle,
    Bound(Vec<Site>),
}

#[derive(Clone, Copy)]
struct Lookup<'a> {
    name: &'a str,
    tail: Tail<'a>,
    offset: usize,
    ns: Namespace,
}
impl Lookup<'_> {
    fn accepts_module(self) -> bool {
        self.ns != Namespace::Value || !self.tail.is_empty()
    }
    fn local(&self, resolver: &Resolver, at: Address, owner: Option<usize>) -> Vec<Site> {
        let (file, scope) = at;
        let scope = &resolver.evidence(file).scopes[scope];
        if scope.associated || !resolver.active_files[file] {
            return Vec::new();
        }
        let Some(bindings) = scope.names.get(self.name) else {
            return Vec::new();
        };
        bindings.select(
            self.offset,
            scope.owner == owner,
            |b| {
                namespace_accepts(self.ns, b.ns)
                    && resolver.configs[file].declaration_active(b.decl)
            },
            |b| resolver.binding_site(at, self.name, b),
        )
    }
}

#[derive(Clone, Copy)]
enum Position {
    Lexical(Option<usize>),
    Intermediate,
    Terminal,
}
impl Position {
    fn precedence(self) -> &'static [ScopeStep] {
        use ScopeStep::{Binding, Child, Glob};
        match self {
            Self::Lexical(_) => &[Binding, Child, Glob],
            Self::Intermediate => &[Child, Binding, Glob],
            Self::Terminal => &[Binding],
        }
    }

    fn owner(self) -> Option<usize> {
        match self {
            Self::Lexical(owner) => owner,
            Self::Intermediate | Self::Terminal => None,
        }
    }
}

enum ScopeStep {
    Binding,
    Child,
    Glob,
}

enum ScopeResult {
    Claimed(Vec<Site>),
    Module(Address),
    Continue,
}

#[derive(Clone, Copy)]
struct ScopeProbe<'a, 'q> {
    at: Address,
    query: Lookup<'q>,
    position: Position,
    trail: Trail<'a>,
}

struct Visit<'a> {
    at: Address,
    name: &'a str,
    depth: u8,
    parent: Trail<'a>,
}

impl<'a> Visit<'a> {
    fn enter(parent: Trail<'a>, at: Address, name: &'a str) -> Option<Self> {
        let depth = parent.map_or(1, |v| v.depth + 1);
        if depth > 64 {
            return None;
        }
        let mut current = parent;
        while let Some(prior) = current {
            if prior.at == at && prior.name == name {
                return None;
            }
            current = prior.parent;
        }
        Some(Self {
            at,
            name,
            depth,
            parent,
        })
    }
}

impl Resolver<'_, '_> {
    pub(in crate::domain::code::rust) fn lookup(
        &self,
        origin: Address,
        (head, tail): (&str, &[String]),
        offset: usize,
        ns: Namespace,
    ) -> Vec<Site> {
        self.lookup_inner(
            origin,
            Lookup {
                name: head,
                tail: Tail::Slice(tail),
                offset,
                ns,
            },
            None,
        )
    }

    fn lookup_inner(&self, origin: Address, query: Lookup, trail: Trail<'_>) -> Vec<Site> {
        let Lookup {
            name: head,
            tail,
            offset,
            ns,
        } = query;
        let origin_scope = &self.evidence(origin.0).scopes[origin.1];
        let module = (origin.0, origin_scope.module);
        let selected = match head {
            "crate" => self.roots.get(&module).copied().flatten(),
            "self" if tail.is_empty() && ns != Namespace::Type => {
                return self.self_value(origin, offset);
            }
            "self" => Some(module),
            "super" => self.parents.get(&module).copied().flatten(),
            _ => return self.lexical(origin, query, trail),
        };
        self.module_path(selected, tail, ns, trail)
    }

    fn self_value(&self, origin: Address, offset: usize) -> Vec<Site> {
        let scopes = &self.evidence(origin.0).scopes;
        let owner = scopes[origin.1].owner;
        let query = Lookup {
            name: "self",
            tail: Tail::Slice(&[]),
            offset,
            ns: Namespace::Value,
        };
        let mut scope = Some(origin.1);
        while let Some(id) = scope {
            let found = query.local(self, (origin.0, id), owner);
            if !found.is_empty() {
                return found;
            }
            scope = scopes[id].parent;
        }
        Vec::new()
    }

    fn lexical(&self, origin: Address, query: Lookup, trail: Trail<'_>) -> Vec<Site> {
        let scopes = &self.evidence(origin.0).scopes;
        let position = Position::Lexical(scopes[origin.1].owner);
        let mut ancestors = std::iter::successors(Some(origin.1), |&id| {
            let scope = &scopes[id];
            (id != scope.module).then_some(scope.parent).flatten()
        });
        let found = ancestors.find_map(|id| {
            let probe = ScopeProbe {
                at: (origin.0, id),
                query,
                position,
                trail,
            };
            self.lexical_scope(probe, origin, scopes[id].module)
        });
        match found {
            Some(ScopeResult::Claimed(sites)) => sites,
            Some(ScopeResult::Module(child)) => {
                self.module_path(Some(child), query.tail, query.ns, trail)
            }
            _ => Vec::new(),
        }
    }

    fn lexical_scope(
        &self,
        probe: ScopeProbe<'_, '_>,
        origin: Address,
        boundary: usize,
    ) -> Option<ScopeResult> {
        let at_boundary = probe.at.1 == boundary;
        if probe.at != origin
            && at_boundary
            && let Some(sites) = self.cached_suffix(probe.at, probe.query, probe.trail)
        {
            return Some(ScopeResult::Claimed(sites));
        }
        match self.probe_scope(probe) {
            ScopeResult::Continue if at_boundary => self
                .external_module(probe.at.0, probe.query)
                .map(ScopeResult::Module),
            ScopeResult::Continue => None,
            result => Some(result),
        }
    }

    fn external_module(&self, file: usize, query: Lookup) -> Option<Address> {
        self.externs[file]
            .as_ref()
            .filter(|_| query.accepts_module())?
            .get(query.name)
            .map(|&file| (file, 0))
    }

    fn probe_scope(&self, probe: ScopeProbe<'_, '_>) -> ScopeResult {
        probe
            .position
            .precedence()
            .iter()
            .find_map(|step| self.probe_step(step, probe))
            .unwrap_or(ScopeResult::Continue)
    }

    fn probe_step(&self, step: &ScopeStep, probe: ScopeProbe<'_, '_>) -> Option<ScopeResult> {
        let ScopeProbe {
            at,
            query,
            position,
            trail,
        } = probe;
        match step {
            ScopeStep::Binding => self
                .scope_binding(at, query, position, trail)
                .map(ScopeResult::Claimed),
            ScopeStep::Child if query.accepts_module() => {
                self.child(at, query.name).map(ScopeResult::Module)
            }
            ScopeStep::Child => None,
            ScopeStep::Glob => {
                let scope = &self.evidence(at.0).scopes[at.1];
                let found = self.glob_targets(at, &scope.globs, query, trail);
                (!found.is_empty()).then_some(ScopeResult::Claimed(found))
            }
        }
    }

    fn scope_binding(
        &self,
        at: Address,
        query: Lookup,
        position: Position,
        trail: Trail,
    ) -> Option<Vec<Site>> {
        if query.tail.is_empty() {
            let local = query.local(self, at, position.owner());
            if !local.is_empty() {
                return Some(local);
            }
        }
        match self.alias_lookup(at, query, trail) {
            AliasLookup::Bound(sites) => Some(sites),
            AliasLookup::Cycle if !matches!(position, Position::Terminal) => Some(Vec::new()),
            _ => None,
        }
    }

    // A terminal path can fall back to glob lookup after an import cycle.
    // Lexical and intermediate components instead stop at the written alias.
    fn alias_lookup(&self, at: Address, query: Lookup, trail: Trail<'_>) -> AliasLookup {
        let Lookup { name, tail, .. } = query;
        let Some(path) = self.alias(at.0, at.1, name) else {
            return AliasLookup::Absent;
        };
        let Some(visit) = Visit::enter(trail, at, name) else {
            return AliasLookup::Cycle;
        };
        let more = Tail::prepend(&path[1..], &tail);
        AliasLookup::Bound(self.lookup_inner(
            at,
            Lookup {
                name: &path[0],
                tail: more,
                ..query
            },
            Some(&visit),
        ))
    }

    fn glob_targets(
        &self,
        at: Address,
        globs: &[(usize, Vec<String>)],
        query: Lookup,
        trail: Trail<'_>,
    ) -> Vec<Site> {
        let Lookup {
            name: head, tail, ..
        } = query;
        if globs.is_empty() {
            return vec![];
        }
        let Some(visit) = Visit::enter(trail, at, head) else {
            return vec![];
        };
        let mut found = Vec::new();
        let suffix = Tail::Name(head, &tail);
        let active = globs
            .iter()
            .filter(|(offset, _)| self.active(at.0, *offset));
        for (_, prefix) in active {
            let imported = prefix.split_first().map_or(query, |(name, more)| Lookup {
                name,
                tail: Tail::prepend(more, &suffix),
                ..query
            });
            found.extend(self.lookup_inner(at, imported, Some(&visit)));
        }
        found
    }

    fn cached_suffix(&self, at: Address, query: Lookup, trail: Trail) -> Option<Vec<Site>> {
        let Tail::Slice(cache_key) = query.tail else {
            return None;
        };
        if trail.is_some() || !self.invariant_modules.contains(&at) {
            return None;
        }
        let Lookup { name: head, ns, .. } = query;
        // After lexical scopes have failed, module-level name queries
        // cannot see an enclosing function's locals. Cache that suffix
        // only when every binding in the module is position-independent.
        if let Some(sites) = self
            .module_results
            .borrow()
            .get(&(at, ns))
            .and_then(|m| m.get(head))
            .and_then(|tails| tails.get(cache_key))
        {
            return Some(sites.clone());
        }
        let sites = self.lookup_inner(
            at,
            Lookup {
                name: head,
                tail: Tail::Slice(cache_key),
                offset: usize::MAX,
                ns,
            },
            None,
        );
        self.module_results
            .borrow_mut()
            .entry((at, ns))
            .or_default()
            .entry(head.into())
            .or_default()
            .insert(cache_key.to_vec(), sites.clone());
        Some(sites)
    }

    fn terminal(&self, module: Address, query: Lookup, trail: Trail) -> Vec<Site> {
        let scope = &self.evidence(module.0).scopes[module.1];
        if scope.globs.is_empty() {
            return self
                .terminal_binding(module, query, trail)
                .unwrap_or_default();
        }
        // Ordinary module lookup already checks its own items and imports before globs.
        // A function-owned module or a special path head has distinct lexical lookup rules.
        if (scope.owner.is_some() || matches!(query.name, "self" | "crate"))
            && let Some(sites) = self.terminal_binding(module, query, trail)
        {
            return sites;
        }
        self.lookup_inner(module, query, trail)
    }

    fn terminal_binding(
        &self,
        module: Address,
        query: Lookup,
        trail: Trail<'_>,
    ) -> Option<Vec<Site>> {
        match self.probe_scope(ScopeProbe {
            at: module,
            query,
            position: Position::Terminal,
            trail,
        }) {
            ScopeResult::Claimed(sites) => Some(sites),
            _ => None,
        }
    }

    fn module_path(
        &self,
        mut selected: Option<Address>,
        mut rest: Tail<'_>,
        ns: Namespace,
        trail: Trail<'_>,
    ) -> Vec<Site> {
        if rest.is_empty() {
            return selected
                .and_then(|a| self.module_target(a))
                .into_iter()
                .collect();
        }
        while let (Some(module), Some((part, tail))) = (selected, rest.split_first()) {
            let query = Lookup {
                name: part,
                tail,
                offset: usize::MAX,
                ns,
            };
            selected = match self.path_step(module, query, trail) {
                ScopeResult::Module(child) => Some(child),
                ScopeResult::Claimed(sites) => return sites,
                ScopeResult::Continue => return Vec::new(),
            };
            rest = tail;
        }
        vec![]
    }
    fn path_step(&self, module: Address, query: Lookup, trail: Trail<'_>) -> ScopeResult {
        if query.name == "super" {
            return self
                .parents
                .get(&module)
                .copied()
                .flatten()
                .map_or(ScopeResult::Continue, ScopeResult::Module);
        }
        if query.tail.is_empty() {
            return ScopeResult::Claimed(self.terminal(module, query, trail));
        }
        self.probe_scope(ScopeProbe {
            at: module,
            query,
            position: Position::Intermediate,
            trail,
        })
    }
}
