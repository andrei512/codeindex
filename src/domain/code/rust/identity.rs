//! Public SCIP identities for written items in a proven Cargo target/module.
//! The exporter assigns local ordinals after macro expansion and type analysis.
//! Leave local identities and unsupported descriptors to the native fallback.
use super::*;
use std::sync::Arc;

fn simple(s: &str) -> bool {
    s != "_" && !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn suffix(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "function_item" | "function_signature_item" => "().",
        "type_item" | "struct_item" | "trait_item" | "enum_item" | "associated_type"
        | "enum_variant" => "#",
        "const_item" | "static_item" | "field_declaration" => ".",
        "macro_definition" => "!",
        "mod_item" => "/",
        _ => return None,
    })
}

fn item(r: &Resolver, file: usize, local: usize, prefix: &str, labels: &mut HashMap<Site, String>) {
    let u = &r.units[file];
    let d = &u.analysis.declarations[local];
    let spelling = &u.text[d.span.clone()];
    if !r.active(file, d.span.start) || !simple(spelling) {
        return;
    }
    if let Some(suffix) = suffix(d.kind) {
        labels.insert(Site { file, local }, format!("{prefix}{spelling}{suffix}"));
    }
}

pub(super) fn collect(
    r: &Resolver,
    targets: &HashMap<usize, Arc<str>>,
    labels: &mut HashMap<Site, String>,
) {
    // Each canonical module needs one physical edge from its parent. A shared
    // file used under two different names has no single public identity.
    let mut parents: HashMap<Address, Option<(Address, &str)>> = HashMap::new();
    for (&parent, children) in &r.modules {
        for (name, &child) in children {
            let entry = (parent, name.as_str());
            parents
                .entry(child)
                .and_modify(|old| {
                    if *old != Some(entry) {
                        *old = None;
                    }
                })
                .or_insert(Some(entry));
        }
    }
    let mut prefixes = HashMap::new();
    let mut todo = Vec::new();
    for (&file, prefix) in targets {
        if !parents.contains_key(&(file, 0)) {
            todo.push(((file, 0), prefix.to_string(), true));
        }
    }
    while let Some((address, prefix, is_root)) = todo.pop() {
        let (file, scope) = address;
        if prefixes.contains_key(&address) {
            continue;
        }
        if scope == 0
            && let Some((local, _)) = r.units[file]
                .analysis
                .declarations
                .iter()
                .enumerate()
                .next_back()
                .filter(|(_, d)| d.kind == "source_file")
        {
            let label = if is_root {
                format!("{prefix}crate/")
            } else {
                prefix.clone()
            };
            labels.insert(Site { file, local }, label);
        }
        if let Some(children) = r.modules.get(&address) {
            for (name, &child) in children {
                if simple(name)
                    && parents.get(&child) == Some(&Some((address, name)))
                    && !(child.1 == 0 && targets.contains_key(&child.0))
                {
                    todo.push((child, format!("{prefix}{name}/"), false));
                }
            }
        }
        prefixes.insert(address, prefix);
    }
    for (file, u) in r.units.iter().enumerate() {
        let Some(e) = &u.analysis.rust else {
            continue;
        };
        if e.conditional_crate {
            continue;
        }
        for (scope, s) in e.scopes.iter().enumerate() {
            if scope != s.module {
                continue;
            }
            let Some(prefix) = prefixes.get(&(file, scope)) else {
                continue;
            };
            for (name, bindings) in &s.names {
                for b in bindings {
                    if b.local {
                        continue;
                    }
                    let d = &u.analysis.declarations[b.decl];
                    if d.kind == "mod_item"
                        && !r
                            .modules
                            .get(&(file, scope))
                            .and_then(|m| m.get(name))
                            .is_some_and(|&child| child.0 == file && child.1 != 0)
                    {
                        continue;
                    }
                    item(r, file, b.decl, prefix, labels);
                }
            }
        }
        for (&scope, trait_spelling) in &e.members.simple_impls {
            let owner = &e.scopes[scope];
            if owner.parent != Some(owner.module) {
                continue;
            }
            let Some(module) = prefixes.get(&(file, owner.module)) else {
                continue;
            };
            let address = (file, owner.module);
            let Some((ty, _)) = e.members.impls.get(&scope) else {
                continue;
            };
            let Some(type_name) = nominal(r, address, &ty[0], false) else {
                continue;
            };
            // The self-type printer may choose an in-scope alias while the
            // trait descriptor uses its canonical name. Require one spelling.
            if ty[0] != type_name {
                continue;
            }
            let mut prefix = format!("{module}impl#[{type_name}]");
            if let Some(spelling) = trait_spelling {
                let Some(trait_name) = nominal(r, address, spelling, true) else {
                    continue;
                };
                prefix.push('[');
                prefix.push_str(trait_name);
                prefix.push(']');
            }
            for b in owner.names.values().flatten() {
                item(r, file, b.decl, &prefix, labels);
            }
        }
        for (&scope, &owner) in &e.members.trait_scopes {
            let Some(prefix) = labels.get(&Site { file, local: owner }).cloned() else {
                continue;
            };
            for b in e.scopes[scope].names.values().flatten() {
                item(r, file, b.decl, &prefix, labels);
            }
        }
        // Aggregate declarations precede their fields. Enum variant labels
        // become owners for fields of that variant in the same pass.
        for &(parent, member) in &e.members.definitions {
            let Some(prefix) = labels
                .get(&Site {
                    file,
                    local: parent,
                })
                .cloned()
            else {
                continue;
            };
            item(r, file, member, &prefix, labels);
        }
    }
}

fn nominal<'a>(r: &'a Resolver, at: Address, spelling: &str, is_trait: bool) -> Option<&'a str> {
    let target = r.lookup(at, spelling, &[], usize::MAX, Namespace::Type, &mut vec![]);
    let [target] = target.as_slice() else {
        return None;
    };
    let u = &r.units[target.file];
    let d = &u.analysis.declarations[target.local];
    if !r
        .evidence(target.file)
        .members
        .plain_types
        .contains(&target.local)
        || (is_trait && d.kind != "trait_item")
        || (!is_trait && !matches!(d.kind, "struct_item" | "enum_item"))
    {
        return None;
    }
    let name = &u.text[d.span.clone()];
    simple(name).then_some(name)
}
