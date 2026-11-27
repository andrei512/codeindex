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

fn item_label(r: &Resolver, site: Site, prefix: &str) -> Option<String> {
    let Site { file, local } = site;
    let u = &r.units[file];
    let d = &u.analysis.declarations[local];
    let spelling = &u.text[d.span.clone()];
    if !r.active_declaration(site) || !simple(spelling) {
        return None;
    }
    Some(format!("{prefix}{spelling}{}", suffix(d.kind)?))
}

fn item(r: &Resolver, site: Site, prefix: &str, labels: &mut HashMap<Site, String>) {
    if let Some(label) = item_label(r, site, prefix) {
        labels.insert(site, label);
    }
}

struct ModuleIdentity {
    address: Address,
    prefix: String,
    root: bool,
    publish_items: bool,
}

impl ModuleIdentity {
    fn module_label(&self) -> String {
        if self.root {
            format!("{}crate/", self.prefix)
        } else {
            self.prefix.clone()
        }
    }

    fn publish(&self, resolver: &Resolver, labels: &mut HashMap<Site, String>) {
        if let Some(site) = resolver.module_target(self.address) {
            labels.insert(site, self.module_label());
        }
        if self.publish_items {
            module_items(resolver, self.address, &self.prefix, labels);
        }
    }
}

struct ModuleCensus<'a, 'u, 's> {
    resolver: &'a Resolver<'u, 's>,
    files: &'a [usize],
    targets: &'a HashMap<usize, Arc<str>>,
}

impl ModuleCensus<'_, '_, '_> {
    fn includes_items(&self, file: usize) -> bool {
        self.files.binary_search(&file).is_ok() && !self.resolver.evidence(file).conditional_crate
    }

    fn roots(&self) -> impl Iterator<Item = ModuleIdentity> + '_ {
        self.targets.iter().filter_map(|(&file, prefix)| {
            let address = (file, 0);
            if self.files.binary_search(&file).is_err()
                || self.resolver.parents.contains_key(&address)
            {
                return None;
            }
            Some(ModuleIdentity {
                address,
                prefix: prefix.to_string(),
                root: true,
                publish_items: !self.resolver.evidence(file).conditional_crate,
            })
        })
    }

    fn children<'a>(
        &'a self,
        parent: &'a ModuleIdentity,
    ) -> impl Iterator<Item = ModuleIdentity> + 'a {
        self.resolver
            .modules
            .get(&parent.address)
            .into_iter()
            .flatten()
            .filter_map(move |(name, &child)| {
                if !self.admits_child(name, child) {
                    return None;
                }
                let publish_items = if child.0 == parent.address.0 {
                    parent.publish_items
                } else {
                    self.includes_items(child.0)
                };
                Some(ModuleIdentity {
                    address: child,
                    prefix: format!("{}{name}/", parent.prefix),
                    root: false,
                    publish_items,
                })
            })
    }

    fn admits_child(&self, name: &str, child: Address) -> bool {
        simple(name)
            && !self.resolver.aliased_modules.contains(&child)
            && !(child.1 == 0 && self.targets.contains_key(&child.0))
    }

    fn publish(&self, labels: &mut HashMap<Site, String>) -> HashMap<Address, String> {
        use std::collections::hash_map::Entry;
        let mut prefixes = HashMap::new();
        let mut pending: Vec<_> = self.roots().collect();
        while let Some(module) = pending.pop() {
            let Entry::Vacant(slot) = prefixes.entry(module.address) else {
                continue;
            };
            pending.extend(self.children(&module));
            module.publish(self.resolver, labels);
            slot.insert(module.prefix);
        }
        prefixes
    }
}

fn module_items(r: &Resolver, address: Address, prefix: &str, labels: &mut HashMap<Site, String>) {
    let (file, scope) = address;
    for (name, bindings) in &r.evidence(file).scopes[scope].names {
        for b in bindings {
            if b.local {
                continue;
            }
            let d = &r.units[file].analysis.declarations[b.decl];
            if d.kind == "mod_item"
                && !r
                    .child(address, name)
                    .is_some_and(|child| child.0 == file && child.1 != 0)
            {
                continue;
            }
            item(
                r,
                Site {
                    file,
                    local: b.decl,
                },
                prefix,
                labels,
            );
        }
    }
}

fn scope_items(r: &Resolver, at: Address, prefix: &str, labels: &mut HashMap<Site, String>) {
    for binding in r.evidence(at.0).scopes[at.1].names.values().flatten() {
        item(
            r,
            Site {
                file: at.0,
                local: binding.decl,
            },
            prefix,
            labels,
        );
    }
}

pub(super) fn collect(
    r: &Resolver,
    files: &[usize],
    targets: &HashMap<usize, Arc<str>>,
    labels: &mut HashMap<Site, String>,
) {
    let census = ModuleCensus {
        resolver: r,
        files,
        targets,
    };
    let prefixes = census.publish(labels);
    let eligible = files.iter().filter_map(|&file| {
        let evidence = r.units[file].analysis.rust.as_deref()?;
        (!evidence.conditional_crate).then_some((file, evidence))
    });
    for (file, evidence) in eligible {
        let items = FileItems {
            resolver: r,
            file,
            evidence,
        };
        items.impl_items(&prefixes, labels);
        items.trait_items(labels);
        aggregate_items(r, file, evidence, labels);
    }
}

struct FileItems<'a, 'u, 's> {
    resolver: &'a Resolver<'u, 's>,
    file: usize,
    evidence: &'a Evidence,
}

impl FileItems<'_, '_, '_> {
    fn trait_items(&self, labels: &mut HashMap<Site, String>) {
        for (&scope, &owner) in &self.evidence.members.trait_scopes {
            let site = Site {
                file: self.file,
                local: owner,
            };
            self.publish_trait(scope, site, labels);
        }
    }

    fn publish_trait(&self, scope: usize, site: Site, labels: &mut HashMap<Site, String>) {
        if let Some(prefix) = labels.get(&site).cloned() {
            scope_items(self.resolver, (self.file, scope), &prefix, labels);
        }
    }

    fn impl_items(&self, prefixes: &HashMap<Address, String>, labels: &mut HashMap<Site, String>) {
        let supported = self.evidence.members.simple_impls().filter_map(|header| {
            let scope = header.scope;
            let owner = &self.evidence.scopes[scope];
            let module = owner.parent.filter(|&parent| parent == owner.module)?;
            let prefix = impl_prefix(self.resolver, (self.file, module), header, prefixes)?;
            Some((scope, prefix))
        });
        for (scope, prefix) in supported {
            scope_items(self.resolver, (self.file, scope), &prefix, labels);
        }
    }
}

fn aggregate_items(
    r: &Resolver,
    file: usize,
    evidence: &Evidence,
    labels: &mut HashMap<Site, String>,
) {
    // An enum variant published here can own a following field.
    for &(parent, member) in &evidence.members.definitions {
        let member = Site {
            file,
            local: member,
        };
        let label = labels
            .get(&Site {
                file,
                local: parent,
            })
            .and_then(|prefix| item_label(r, member, prefix));
        if let Some(label) = label {
            labels.insert(member, label);
        }
    }
}

fn impl_prefix(
    r: &Resolver,
    address: Address,
    header: members::SimpleImpl<'_>,
    prefixes: &HashMap<Address, String>,
) -> Option<String> {
    let module = prefixes.get(&address)?;
    let type_name = nominal(r, address, header.target, false)?;
    // The self-type printer may choose an in-scope alias while the
    // trait descriptor uses its canonical name. Require one spelling.
    if header.target != type_name {
        return None;
    }
    let mut prefix = format!("{module}impl#[{type_name}]");
    if let Some(spelling) = header.trait_name {
        let trait_name = nominal(r, address, spelling, true)?;
        prefix.push('[');
        prefix.push_str(trait_name);
        prefix.push(']');
    }
    Some(prefix)
}

fn nominal<'a>(r: &'a Resolver, at: Address, spelling: &str, is_trait: bool) -> Option<&'a str> {
    let target = r.lookup(at, (spelling, &[]), usize::MAX, Namespace::Type);
    let [target] = target.as_slice() else {
        return None;
    };
    let u = &r.units[target.file];
    let d = &u.analysis.declarations[target.local];
    let accepted = match d.kind {
        "trait_item" => is_trait,
        "struct_item" | "enum_item" => !is_trait,
        _ => false,
    };
    if !accepted
        || !r
            .evidence(target.file)
            .members
            .plain_types
            .contains(&target.local)
    {
        return None;
    }
    let name = &u.text[d.span.clone()];
    simple(name).then_some(name)
}
