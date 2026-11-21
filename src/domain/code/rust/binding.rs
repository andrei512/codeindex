use super::*;

pub(super) struct Resolver<'a, 's> {
    pub(super) units: &'a [Unit<'s>],
    pub(super) modules: HashMap<Address, HashMap<String, Address>>,
    externs: Vec<Option<std::sync::Arc<HashMap<String, usize>>>>,
    parents: HashMap<Address, Option<Address>>,
    roots: HashMap<Address, Option<Address>>,
    disabled: Vec<Vec<std::ops::Range<usize>>>,
    active_files: Vec<bool>,
}
impl<'a, 's> Resolver<'a, 's> {
    pub(super) fn active(&self, file: usize, at: usize) -> bool {
        self.active_files[file] && config::active(&self.disabled[file], at)
    }
    pub(super) fn evidence(&self, file: usize) -> &Evidence {
        self.units[file].analysis.rust.as_ref().unwrap()
    }
    fn root(&self, mut module: Address) -> Option<Address> {
        let mut seen = HashSet::new();
        while let Some(&parent) = self.parents.get(&module) {
            if !seen.insert(module) {
                return None;
            }
            module = parent?;
        }
        Some(module)
    }
    fn child(&self, parent: Address, key: &str) -> Option<Address> {
        self.modules.get(&parent)?.get(key).copied()
    }
    fn module_target(&self, addr: Address) -> Option<Site> {
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
    fn local(
        &self,
        addr: Address,
        key: &str,
        offset: usize,
        owner: Option<usize>,
        ns: Namespace,
    ) -> Vec<Site> {
        let scope = &self.evidence(addr.0).scopes[addr.1];
        if scope.associated {
            return Vec::new();
        }
        let mut max = None;
        let mut sites = Vec::new();
        for b in scope.names.get(key).into_iter().flatten() {
            if !self.active(
                addr.0,
                self.units[addr.0].analysis.declarations[b.decl].span.start,
            ) || b.visibility > offset
                || (b.local && scope.owner != owner)
                || (ns != Namespace::Either && b.ns != Namespace::Either && b.ns != ns)
            {
                continue;
            }
            let priority = (b.local, b.visibility);
            if max.is_none_or(|p| priority > p) {
                max = Some(priority);
                sites.clear();
            }
            if max == Some(priority) {
                let site = Site {
                    file: addr.0,
                    local: b.decl,
                };
                let target = if self.units[addr.0].analysis.declarations[b.decl].kind == "mod_item"
                {
                    match self.child(addr, key) {
                        Some(child) => self.module_target(child).unwrap_or(site),
                        None => continue,
                    }
                } else {
                    site
                };
                sites.push(target);
            }
        }
        sites
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
    pub(super) fn lookup(
        &self,
        origin: Address,
        head: &str,
        tail: &[String],
        offset: usize,
        ns: Namespace,
        visited: &mut Vec<(Address, String)>,
    ) -> Vec<Site> {
        let origin_scope = &self.evidence(origin.0).scopes[origin.1];
        let module = (origin.0, origin_scope.module);
        let mut at = origin;
        let mut rest = tail;
        let mut selected = match head {
            "crate" => self.roots.get(&module).copied().flatten(),
            "self" if tail.is_empty() && ns != Namespace::Type => {
                let mut scope = Some(origin.1);
                while let Some(id) = scope {
                    let found = self.local(
                        (origin.0, id),
                        "self",
                        offset,
                        origin_scope.owner,
                        Namespace::Value,
                    );
                    if !found.is_empty() {
                        return found;
                    }
                    scope = self.evidence(origin.0).scopes[id].parent;
                }
                return vec![];
            }
            "self" => Some(module),
            "super" => self.parents.get(&module).copied().flatten(),
            _ => loop {
                let s = &self.evidence(at.0).scopes[at.1];
                if tail.is_empty() {
                    let local = self.local(at, head, offset, origin_scope.owner, ns);
                    if !local.is_empty() {
                        return local;
                    }
                }
                if let Some(path) = self.alias(at.0, at.1, head) {
                    let k = (at, head.to_owned());
                    if visited.contains(&k) || visited.len() >= 64 {
                        return vec![];
                    }
                    let mut full = path.clone();
                    full.extend_from_slice(tail);
                    visited.push(k);
                    return self.lookup(at, &full[0], &full[1..], offset, ns, visited);
                }
                if (ns != Namespace::Value || !tail.is_empty())
                    && let Some(child) = self.child(at, head)
                {
                    break Some(child);
                }
                if !s.globs.is_empty()
                    && visited.len() < 64
                    && !visited.iter().any(|(addr, k)| *addr == at && k == head)
                {
                    let mut found = Vec::new();
                    visited.push((at, head.to_owned()));
                    for (at_offset, prefix) in &s.globs {
                        if !self.active(at.0, *at_offset) {
                            continue;
                        }
                        let depth = visited.len();
                        let mut full = prefix.clone();
                        full.push(head.into());
                        full.extend_from_slice(tail);
                        found.extend(self.lookup(at, &full[0], &full[1..], offset, ns, visited));
                        visited.truncate(depth);
                    }
                    visited.pop();
                    if !found.is_empty() {
                        return found;
                    }
                }
                if at.1 == s.module {
                    if tail.is_empty() && ns == Namespace::Value {
                        break None;
                    }
                    break self.externs[at.0]
                        .as_ref()
                        .and_then(|m| m.get(head))
                        .map(|&f| (f, 0));
                }
                let Some(parent) = s.parent else {
                    break None;
                };
                at.1 = parent;
            },
        };
        if rest.is_empty() {
            return selected
                .and_then(|a| self.module_target(a))
                .into_iter()
                .collect();
        }
        while let (Some(module), Some((part, tail))) = (selected, rest.split_first()) {
            if part == "super" {
                selected = self.parents.get(&module).copied().flatten();
            } else if tail.is_empty() {
                let found = self.local(module, part, usize::MAX, None, ns);
                if !found.is_empty() {
                    return found;
                }
                if let Some(alias) = self.alias(module.0, module.1, part) {
                    let k = (module, part.clone());
                    if !visited.contains(&k) && visited.len() < 64 {
                        visited.push(k);
                        return self.lookup(
                            module,
                            &alias[0],
                            &alias[1..],
                            usize::MAX,
                            ns,
                            visited,
                        );
                    }
                }
                if !self.evidence(module.0).scopes[module.1].globs.is_empty() {
                    return self.lookup(module, part, &[], usize::MAX, ns, visited);
                }
                break;
            } else {
                selected = self.child(module, part);
                if selected.is_none() {
                    if let Some(alias) = self.alias(module.0, module.1, part) {
                        let marker = (module, part.clone());
                        if visited.contains(&marker) || visited.len() >= 64 {
                            return vec![];
                        }
                        visited.push(marker);
                        let mut full = alias.clone();
                        full.extend_from_slice(tail);
                        return self.lookup(module, &full[0], &full[1..], usize::MAX, ns, visited);
                    }
                }
            }
            rest = tail;
        }
        vec![]
    }
}

pub(in crate::domain::code) fn resolve(
    units: &[Unit],
    paths: &HashMap<&str, usize>,
    names: &mut Names,
) {
    let cargo = cargo::inputs(units, paths);
    let mut r = Resolver {
        units,
        modules: HashMap::new(),
        externs: cargo.externs,
        parents: HashMap::new(),
        roots: HashMap::new(),
        disabled: config::disabled(units),
        active_files: vec![true; units.len()],
    };
    let mut referred = HashSet::new();
    let mut active_links = HashMap::<usize, Vec<usize>>::new();
    for (file, u) in units.iter().enumerate() {
        let Some(e) = &u.analysis.rust else {
            continue;
        };
        let path = Path::new(u.path);
        let mut base = path.parent().unwrap_or(Path::new("")).to_path_buf();
        let is_target = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .is_some_and(|s| matches!(s, "tests" | "examples" | "benches" | "bin"));
        if !is_target
            && !matches!(
                path.file_stem().and_then(|s| s.to_str()),
                Some("lib" | "main" | "mod")
            )
        {
            if let Some(stem) = path.file_stem() {
                base.push(stem);
            }
        }
        let mut queue = vec![(0, base)];
        while let Some((scope, dir)) = queue.pop() {
            for (key, variants) in &e.scopes[scope].children {
                let mut visible = Vec::new();
                for (offset, child) in variants {
                    let next = dir.join(key);
                    let addr = match child {
                        Module::Inline(id) => {
                            queue.push((*id, next));
                            Some((file, *id))
                        }
                        Module::File(Some(relative)) => {
                            let base = if scope == 0 {
                                path.parent().unwrap_or(Path::new(""))
                            } else {
                                dir.as_path()
                            };
                            cargo::join(base, relative)
                                .and_then(|candidate| paths.get(candidate.to_str()?).copied())
                                .filter(|&f| units[f].analysis.rust.is_some())
                                .map(|f| (f, 0))
                        }
                        Module::File(None) => {
                            let options: [PathBuf; 2] =
                                [next.with_extension("rs"), next.join("mod.rs")];
                            let mut candidates = options
                                .iter()
                                .filter_map(|p| paths.get(p.to_str()?))
                                .copied()
                                .filter(|&f| units[f].analysis.rust.is_some());
                            candidates
                                .next()
                                .filter(|_| candidates.next().is_none())
                                .map(|f| (f, 0))
                        }
                    };
                    if let Some(addr) = addr {
                        if addr.0 != file {
                            referred.insert(addr.0);
                        }
                        if config::active(&r.disabled[file], *offset) {
                            visible.push(addr);
                            if addr.0 != file {
                                active_links.entry(file).or_default().push(addr.0);
                            }
                        }
                    }
                }
                if let [addr] = visible.as_slice() {
                    let addr = *addr;
                    r.modules
                        .entry((file, scope))
                        .or_default()
                        .insert(key.clone(), addr);
                    r.parents
                        .entry(addr)
                        .and_modify(|old| {
                            if *old != Some((file, scope)) {
                                *old = None;
                            }
                        })
                        .or_insert(Some((file, scope)));
                }
            }
        }
    }
    let mut reachable = vec![false; units.len()];
    let mut queue: Vec<_> = (0..units.len()).filter(|f| !referred.contains(f)).collect();
    for imports in r.externs.iter().flatten() {
        queue.extend(imports.values().copied());
    }
    while let Some(file) = queue.pop() {
        if reachable[file] {
            continue;
        }
        reachable[file] = true;
        if let Some(next) = active_links.get(&file) {
            queue.extend(next.iter().copied());
        }
    }
    r.active_files = reachable;
    for (file, unit) in units.iter().enumerate() {
        if let Some(e) = &unit.analysis.rust {
            for (scope, _) in e
                .scopes
                .iter()
                .enumerate()
                .filter(|(scope, item)| *scope == item.module)
            {
                let addr = (file, scope);
                r.roots.insert(addr, r.root(addr));
            }
        }
    }
    identity::collect(&r, &cargo.target_prefixes, &mut names.public_labels);
    let mut members = members::Table::build(&r);
    for (file, u) in units.iter().enumerate() {
        let Some(e) = &u.analysis.rust else {
            continue;
        };
        for (local, v) in e.uses.iter().enumerate() {
            if !r.active(file, u.analysis.calls[local].span.start) {
                continue;
            }
            if let Some(&target) = e.direct.get(&local) {
                names.bind(
                    Site { file, local },
                    vec![Site {
                        file,
                        local: target,
                    }],
                );
                continue;
            }
            let c = &u.analysis.calls[local];
            let (head, tail) = v
                .path
                .split_first()
                .map(|(h, t)| (h.as_str(), t))
                .unwrap_or_else(|| (name(&u.text[c.span.clone()]), &[]));
            let addr = (file, v.scope);
            let targets =
                if head == "Self" && tail.is_empty() && !e.members.uses.contains_key(&local) {
                    // SCIP identifies the impl block itself as Self; it has no
                    // written definition range. Keep the nominal owner for member lookup.
                    Vec::new()
                } else if let Some(receiver) = e.members.uses.get(&local) {
                    members.receiver(&r, names, addr, receiver, head, c.span.start)
                } else {
                    members.path(&r, addr, head, tail, c.span.start, v.ns)
                };
            let targets = targets
                .into_iter()
                .filter(|target| {
                    let d = &units[target.file].analysis.declarations[target.local];
                    if !r.active(target.file, d.span.start) {
                        return false;
                    }
                    let kind = d.kind;
                    match e.members.uses.get(&local) {
                        Some(members::Receiver::Literal(_)) => kind == "field_declaration",
                        Some(members::Receiver::Previous(_)) => {
                            if c.is_call {
                                kind.starts_with("function")
                            } else {
                                kind == "field_declaration"
                            }
                        }
                        None => kind != "field_declaration",
                    }
                })
                .collect();
            names.bind(Site { file, local }, targets);
        }
    }
}
