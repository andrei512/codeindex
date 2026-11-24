use super::{Binding, Language, Lookup, ForwardNames, Site, Unit};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};
pub(super) fn module_file(
    path: &str,
    lang: Language,
    module: &str,
    paths: &HashMap<&str, usize>,
) -> Option<usize> {
    let file = Path::new(path);
    let mut base = PathBuf::new();
    let suffixes: &[&str];
    if lang == Language::Python {
        let dots = module.bytes().take_while(|&c| c == b'.').count();
        if dots > 0 {
            base = file.parent()?.to_owned();
            for _ in 1..dots {
                if !base.pop() {
                    return None;
                }
            }
        }
        for part in module[dots..].split('.').filter(|p| !p.is_empty()) {
            base.push(part);
        }
        suffixes = &[".pyi", ".py", "/__init__.pyi", "/__init__.py"];
    } else {
        if !(module.starts_with("./") || module.starts_with("../")) {
            return None;
        }
        base = file.parent()?.to_owned();
        for component in Path::new(module).components() {
            match component {
                Component::Normal(v) => base.push(v),
                Component::CurDir => {}
                Component::ParentDir if base.pop() => {}
                _ => return None,
            }
        }
        if let Some(ext) = base.extension().and_then(|v| v.to_str()) {
            let extensions: &[&str] = match ext {
                "js" => &["ts", "tsx", "d.ts", "js", "jsx"],
                "mjs" => &["mts", "d.mts", "mjs"],
                "cjs" => &["cts", "d.cts", "cjs"],
                _ => &[],
            };
            for ext in extensions {
                let candidate = base.with_extension(ext);
                if let Some(id) = paths.get(candidate.to_str()?) {
                    return Some(*id);
                }
            }
        }
        suffixes = &[
            "",
            ".ts",
            ".tsx",
            ".d.ts",
            ".js",
            ".jsx",
            "/index.ts",
            "/index.tsx",
            "/index.d.ts",
            "/index.js",
            "/index.jsx",
        ];
    }
    let base = base.to_str()?;
    let probe = |prefix: &str| {
        suffixes
            .iter()
            .find_map(|s| paths.get(format!("{prefix}{s}").as_str()).copied())
    };
    let root = probe(base);
    if lang != Language::Python || module.starts_with('.') {
        return root;
    }
    let parent = file.parent()?;
    if parent.as_os_str().is_empty()
        || ["__init__.py", "__init__.pyi"].iter().any(|name| {
            parent
                .join(name)
                .to_str()
                .is_some_and(|p| paths.contains_key(p))
        })
    {
        return root;
    }
    // Directly launched scripts include their own directory on the import path.
    // Leave conflicting roots unresolved without an explicit launch context.
    let script = probe(parent.join(base).to_str()?);
    match (root, script) {
        (Some(a), Some(b)) if a != b => None,
        (a, b) => a.or(b),
    }
}

fn member(
    units: &[Unit],
    site: Site,
    name: &str,
    called: bool,
    mut imported: impl FnMut(usize, usize) -> Option<Site>,
) -> Option<Site> {
    let u = &units[site.file];
    let ev = u.analysis.script.as_ref()?;
    let binding = |hint: &(usize, std::ops::Range<usize>),
                   lookup: &mut dyn FnMut(usize, usize) -> Option<Site>| match ev
        .binding(hint.0, &u.text[hint.1.clone()])
    {
        Some(Binding::Local(id)) => Some(Site {
            file: site.file,
            local: *id,
        }),
        Some(Binding::Import(n)) => lookup(site.file, *n),
        _ => None,
    };
    let mut owner = site;
    let mut result = called;
    if !called {
        if let Some(h) = ev.result_types.get(&site.local) {
            owner = binding(h, &mut imported)?;
            result = true;
        } else if let Some(h) = ev.types.get(&site.local) {
            owner = binding(h, &mut imported)?;
        } else if let Some(h) = ev.iterated.get(&site.local) {
            let collection = binding(h, &mut imported)?;
            let e = units[collection.file].analysis.script.as_ref()?;
            let ty = e.element_types.get(&collection.local)?;
            if collection.file != site.file {
                return None;
            }
            owner = binding(ty, &mut imported)?;
        }
    }
    let mut dest = units[owner.file].analysis.script.as_ref()?;
    if result && let Some((scope, range)) = dest.annotated_returns.get(&owner.local) {
        owner = match dest.binding(*scope, &units[owner.file].text[range.clone()]) {
            Some(Binding::Local(id)) => Site {
                file: owner.file,
                local: *id,
            },
            Some(Binding::Import(n)) => imported(owner.file, *n)?,
            _ => return None,
        };
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
    dest.members
        .get(&(*shape, name.to_owned()))
        .map(|&local| Site {
            file: owner.file,
            local,
        })
}

fn namespace<'a>(
    units: &'a [Unit],
    modules: &[Vec<Option<usize>>],
    mut file: usize,
    mut name: &'a str,
) -> Option<usize> {
    let mut seen = std::collections::HashSet::new();
    while seen.insert((file, name)) {
        let ev = units[file].analysis.script.as_ref()?;
        let imp = match ev.forward.get(name)? {
            super::Forward::Namespace(n) => return modules[file][*n],
            super::Forward::Import(n) => *n,
            super::Forward::Local(s) => match ev.scopes[0].bindings.get(s)? {
                Binding::Import(n) => *n,
                _ => return None,
            },
        };
        file = modules[file][imp]?;
        match ev.imports[imp].name.as_deref() {
            Some(n) => name = n,
            None => return Some(file),
        }
    }
    None
}

pub(in crate::domain::code) fn resolve(
    units: &[Unit],
    paths: &HashMap<&str, usize>,
    out: &mut ForwardNames,
) {
    let projects = super::projects::Projects::read(units);
    let mut modules = Vec::with_capacity(units.len());
    let mut stars = Vec::with_capacity(units.len());
    let mut submodules = Vec::with_capacity(units.len());
    for u in units {
        let lang = Language::for_path(u.path);
        let mut cache = HashMap::new();
        let mut lookup = |m: &str| -> Option<usize> {
            if let Some(v) = cache.get(m) {
                return *v;
            }
            let l = lang?;
            let id = module_file(u.path, l, m, paths).or_else(|| {
                (l == Language::Js)
                    .then(|| projects.lookup(u.path, m, paths))
                    .flatten()
            });
            cache.insert(m.to_owned(), id);
            id
        };
        modules.push(
            u.analysis
                .script
                .as_ref()
                .map(|e| {
                    e.imports
                        .iter()
                        .map(|m| lookup(&m.module))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
        let mut fallback = HashMap::new();
        if lang == Some(Language::Python)
            && let Some(e) = u.analysis.script.as_ref()
        {
            for (id, imp) in e.imports.iter().enumerate() {
                if let Some(member) = &imp.name {
                    let active = modules
                        .last()
                        .and_then(|m| m.get(id).copied())
                        .flatten()
                        .and_then(|ix| units[ix].analysis.script.as_ref());
                    if active.is_some_and(|p| {
                        p.exports.contains_key(member) || p.forward.contains_key(member)
                    }) {
                        continue;
                    }
                    let separator = if imp.module.ends_with('.') { "" } else { "." };
                    if let Some(file) = lookup(&format!("{}{separator}{member}", imp.module)) {
                        fallback.insert(id, file);
                    }
                }
            }
        }
        submodules.push(fallback);
        stars.push(
            u.analysis
                .script
                .as_ref()
                .map(|e| e.stars.iter().filter_map(|m| lookup(m)).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
    }
    let mut resolved = HashMap::<(usize, String), Option<Site>>::new();
    for (file, u) in units.iter().enumerate() {
        let Some(e) = &u.analysis.script else {
            continue;
        };
        let mut exported = |file: usize, import: usize, member: Option<&str>| {
            let imp = &units[file].analysis.script.as_ref()?.imports[import];
            let (id, name) = match (&imp.name, member) {
                (Some(name), None) => (modules[file][import]?, name.as_str()),
                (None, Some(name)) => (modules[file][import]?, name),
                (Some(primary), Some(name)) => (
                    submodules[file]
                        .get(&import)
                        .copied()
                        .or_else(|| namespace(units, &modules, modules[file][import]?, primary))?,
                    name,
                ),
                _ => return None,
            };
            let key = (id, name.to_owned());
            if let Some(result) = resolved.get(&key) {
                return *result;
            }
            let mut todo = vec![(id, name)];
            let mut seen = std::collections::HashSet::new();
            let mut found = None;
            let mut ambiguous = false;
            while let Some((file, name)) = todo.pop() {
                if !seen.insert((file, name)) {
                    continue;
                }
                let Some(ev) = units[file].analysis.script.as_ref() else {
                    continue;
                };
                let local = ev
                    .exports
                    .get(name)
                    .copied()
                    .or_else(|| match ev.forward.get(name) {
                        Some(super::Forward::Local(s)) => match ev.scopes[0].bindings.get(s) {
                            Some(Binding::Local(id)) => Some(*id),
                            _ => None,
                        },
                        _ => None,
                    });
                if let Some(local) = local {
                    let d = &units[file].analysis.declarations[local];
                    if !matches!(ev.scopes[0].bindings.get(&units[file].text[d.span.clone()]),Some(Binding::Local(n)) if *n==local)
                    {
                        continue;
                    }
                    let site = Site { file, local };
                    if found.is_some_and(|x| x != site) {
                        ambiguous = true;
                        break;
                    }
                    found = Some(site);
                    continue;
                }
                let imp = match ev.forward.get(name) {
                    Some(super::Forward::Import(n)) => Some(*n),
                    Some(super::Forward::Local(s)) => match ev.scopes[0].bindings.get(s) {
                        Some(Binding::Import(n)) => Some(*n),
                        _ => None,
                    },
                    Some(super::Forward::Namespace(_)) | None => None,
                };
                if let Some(n) = imp {
                    if let (Some(next), Some(original)) =
                        (modules[file][n], ev.imports[n].name.as_deref())
                    {
                        todo.push((next, original));
                    }
                } else if name != "default" {
                    todo.extend(stars[file].iter().map(|&id| (id, name)));
                }
            }
            let result = if ambiguous { None } else { found };
            resolved.insert(key, result);
            result
        };
        for (local, lookup) in e.uses.iter().enumerate() {
            let c = &u.analysis.calls[local];
            let name = &u.text[c.span.clone()];
            let target = match lookup {
                Lookup::Super {
                    scope,
                    base_scope,
                    base,
                } => {
                    // Only the builtin zero-argument form on a single written base.
                    let owner = match e.binding(*base_scope, &u.text[base.clone()]) {
                        Some(Binding::Local(id)) => Some(Site { file, local: *id }),
                        Some(Binding::Import(n)) => exported(file, *n, None),
                        _ => None,
                    };
                    owner
                        .filter(|s| {
                            e.binding(*scope, "super").is_none()
                                && units[s.file].analysis.declarations[s.local].kind
                                    == "class_definition"
                        })
                        .and_then(|s| member(units, s, name, false, |f, n| exported(f, n, None)))
                }
                Lookup::Construct { scope } => {
                    let definition = match e.binding(*scope, name) {
                        Some(Binding::Local(id)) => Some(Site { file, local: *id }),
                        Some(Binding::Import(n)) => exported(file, *n, None),
                        _ => None,
                    };
                    definition.map(|d| {
                        let ev = units[d.file].analysis.script.as_ref().unwrap();
                        ev.shapes
                            .get(&d.local)
                            .and_then(|shape| ev.members.get(&(*shape, "constructor".to_owned())))
                            .map_or(d, |&local| Site {
                                file: d.file,
                                local,
                            })
                    })
                }
                Lookup::Chained {
                    prior,
                    scope,
                    qualification,
                } => {
                    let package = qualification.as_ref().and_then(|span| {
                        let name = u.text[span.clone()]
                            .split('.')
                            .map(str::trim)
                            .collect::<Vec<_>>()
                            .join(".");
                        let root = name.split('.').next()?;
                        let mut current = Some(*scope);
                        while let Some(sc) = current {
                            if let Some(b) = e.scopes[sc].bindings.get(root) {
                                return if matches!(b, Binding::Package) {
                                    e.packages.get(&(sc, name)).copied()
                                } else {
                                    None
                                };
                            }
                            current = e.scopes[sc].parent;
                        }
                        None
                    });
                    if let Some(n) = package {
                        exported(file, n, Some(name))
                    } else {
                        let targets = out.row(Site {
                            file,
                            local: *prior,
                        });
                        targets.first().and_then(|&d| {
                            member(units, d, name, u.analysis.calls[*prior].is_call, |f, n| {
                                exported(f, n, None)
                            })
                        })
                    }
                }
                Lookup::Known(local) => Some(Site {
                    file,
                    local: *local,
                }),
                Lookup::Prop { scope, component } => {
                    let func = match e.binding(*scope, &u.text[component.clone()]) {
                        Some(Binding::Local(id)) => Some(Site { file, local: *id }),
                        Some(Binding::Import(n)) => exported(file, *n, None),
                        _ => None,
                    };
                    func.and_then(|f| {
                        let ev = units[f.file].analysis.script.as_ref()?;
                        let shape = ev.props.get(&f.local)?;
                        ev.members
                            .get(&(*shape, name.to_owned()))
                            .map(|&local| Site {
                                file: f.file,
                                local,
                            })
                    })
                }
                Lookup::Argument { scope, callee } => {
                    let func = match e.binding(*scope, &u.text[callee.clone()]) {
                        Some(Binding::Local(id)) => Some(Site { file, local: *id }),
                        Some(Binding::Import(n)) => exported(file, *n, None),
                        _ => None,
                    };
                    func.and_then(|f| {
                        let ev = units[f.file].analysis.script.as_ref()?;
                        let scope = *ev.functions.get(&f.local)?;
                        match ev.scopes[scope].bindings.get(name) {
                            Some(Binding::Local(id))
                                if units[f.file].analysis.declarations[*id].kind == "parameter" =>
                            {
                                Some(Site {
                                    file: f.file,
                                    local: *id,
                                })
                            }
                            _ => None,
                        }
                    })
                }
                Lookup::Imported(n) => exported(file, *n, None),
                Lookup::Name { scope, receiver } => {
                    let base = receiver.as_ref().map_or(name, |r| &u.text[r.clone()]);
                    let receiver_scope = if receiver.is_some() && matches!(base, "self" | "cls") {
                        match e.binding(*scope, base) {
                            Some(Binding::Local(id)) => e.self_params.get(id).copied(),
                            _ => None,
                        }
                    } else if receiver.is_some() && base == "this" {
                        let mut here = Some(*scope);
                        let mut result = None;
                        while let Some(i) = here {
                            if let Some(shape) = e.scopes[i].receiver {
                                result = Some(shape);
                                break;
                            }
                            if e.scopes[i].this_boundary {
                                break;
                            }
                            here = e.scopes[i].parent;
                        }
                        result
                    } else {
                        None
                    };
                    if let Some(shape) = receiver_scope {
                        e.members
                            .get(&(shape, name.to_owned()))
                            .map(|&local| Site { file, local })
                    } else {
                        match e.binding(*scope, base) {
                            Some(Binding::Local(id)) if receiver.is_none() => {
                                Some(Site { file, local: *id })
                            }
                            Some(Binding::Import(n)) => {
                                let result = exported(file, *n, receiver.as_ref().map(|_| name));
                                result.or_else(|| {
                                    receiver.as_ref()?;
                                    let owner = exported(file, *n, None)?;
                                    if units[owner.file].analysis.declarations[owner.local].kind
                                        != "module_declaration"
                                    {
                                        return None;
                                    }
                                    member(units, owner, name, false, |f, imp| {
                                        exported(f, imp, None)
                                    })
                                })
                            }
                            Some(Binding::Local(id)) if receiver.is_some() => {
                                member(units, Site { file, local: *id }, name, false, |f, n| {
                                    exported(f, n, None)
                                })
                            }
                            None if receiver.is_none() => {
                                let mut result = None;
                                let mut ambiguous = false;
                                let mut sc = Some(*scope);
                                while let Some(scope) = sc {
                                    for &(_, imp) in e.wildcards.iter().filter(|(s, _)| *s == scope)
                                    {
                                        let Some(id) = modules[file][imp] else {
                                            continue;
                                        };
                                        let Some(remote) = units[id].analysis.script.as_ref()
                                        else {
                                            continue;
                                        };
                                        if remote.dynamic_all
                                            || !remote
                                                .all
                                                .as_ref()
                                                .map_or(!name.starts_with('_'), |names| {
                                                    names.iter().any(|s| s == name)
                                                })
                                        {
                                            continue;
                                        }
                                        if let Some(t) = exported(file, imp, Some(name)) {
                                            if result.is_some_and(|v| v != t) {
                                                ambiguous = true;
                                            }
                                            result = Some(t);
                                        }
                                    }
                                    if result.is_some() {
                                        break;
                                    }
                                    sc = e.scopes[scope].parent;
                                }
                                if ambiguous { None } else { result }
                            }
                            _ => None,
                        }
                    }
                }
            };
            if let Some(target) = target {
                let mut targets = vec![target];
                if let Some(family) = units[target.file]
                    .analysis
                    .script
                    .as_ref()
                    .and_then(|e| e.families.get(&target.local))
                {
                    targets.extend(family.iter().map(|&local| Site {
                        file: target.file,
                        local,
                    }));
                }
                out.bind(Site { file, local }, targets);
            }
        }
    }
}
