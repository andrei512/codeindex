//! Lookup tables shared by Rust member resolution for one linked snapshot.
use super::*;

pub(super) struct Override<'a> {
    pub(super) spelling: &'a str,
    pub(super) trait_site: Site,
    pub(super) implementation: Site,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::domain::code::rust) enum Access {
    Field,
    Method,
}

impl Access {
    pub(in crate::domain::code::rust) fn for_call(call: bool) -> Self {
        if call { Self::Method } else { Self::Field }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Member {
    pub(super) site: Site,
    access: Option<Access>,
}
impl Member {
    pub(super) fn selected(self, access: Access) -> Option<Site> {
        (self.access == Some(access)).then_some(self.site)
    }
}

/// An inherent name shadows all trait overrides for the same written name and
/// receiver, even if the inherent declaration is inactive for this operation.
enum Origins<'a> {
    Declared(Vec<Member>),
    Traits(Vec<Override<'a>>),
}

#[derive(Default)]
pub(super) struct Candidates<'a> {
    owners: HashMap<Site, Origins<'a>>,
    pub(super) has_declarations: bool,
}
impl<'a> Candidates<'a> {
    fn declare(&mut self, owner: Site, member: Member) {
        self.has_declarations = true;
        let origin = self
            .owners
            .entry(owner)
            .or_insert_with(|| Origins::Declared(Vec::new()));
        match origin {
            Origins::Declared(items) => items.push(member),
            Origins::Traits(_) => *origin = Origins::Declared(vec![member]),
        }
    }
    fn add_override(&mut self, owner: Site, item: Override<'a>) {
        if let Origins::Traits(items) = self
            .owners
            .entry(owner)
            .or_insert_with(|| Origins::Traits(Vec::new()))
        {
            items.push(item);
        }
    }
    pub(super) fn declared(&self, owner: &Site) -> &[Member] {
        match self.owners.get(owner) {
            Some(Origins::Declared(items)) => items,
            _ => &[],
        }
    }
    pub(super) fn select(
        &self,
        types: &[Site],
        access: Access,
        visible: impl Fn(&Override) -> bool,
    ) -> Vec<Site> {
        let mut found = Vec::new();
        for ty in types {
            self.extend(ty, access, &mut found, &visible);
        }
        found
    }

    fn extend(
        &self,
        ty: &Site,
        access: Access,
        found: &mut Vec<Site>,
        visible: &impl Fn(&Override) -> bool,
    ) {
        match self.owners.get(ty) {
            Some(Origins::Declared(items)) => {
                found.extend(items.iter().filter_map(|item| item.selected(access)))
            }
            Some(Origins::Traits(items)) if access == Access::Method => {
                found.extend(
                    items
                        .iter()
                        .filter(|item| visible(item))
                        .map(|item| item.implementation),
                );
            }
            _ => {}
        }
    }
}

#[derive(Default)]
pub(in crate::domain::code::rust) struct Table<'src> {
    pub(super) inferred: HashMap<(Site, Projection), Vec<Site>>,
    pub(super) by_name: HashMap<&'src str, Candidates<'src>>,
    pub(super) impl_types: HashMap<Address, Site>,
}

impl<'src> Table<'src> {
    pub fn build(r: &'src Resolver, files: &[usize]) -> Self {
        let mut table = Self::default();
        let evidence = files
            .iter()
            .filter_map(|&file| r.units[file].analysis.rust.as_deref().map(|e| (file, e)));
        for (file, evidence) in evidence {
            table.add_file(r, file, &evidence.members);
        }
        table
    }

    fn add_file(&mut self, r: &'src Resolver, file: usize, members: &super::Members) {
        let site = |local| Site { file, local };
        for &(owner, member) in &members.definitions {
            self.add(r, site(owner), site(member));
        }
        for (&scope, &trait_id) in &members.trait_scopes {
            self.add_scope(r, site(trait_id), (file, scope));
        }
        for header in members.impl_headers() {
            self.add_impl(r, file, header);
        }
    }

    fn add_impl(&mut self, r: &'src Resolver, file: usize, header: ImplHeader<'_>) {
        let ImplHeader {
            scope,
            target: path,
            identity,
        } = header;
        let addr = (file, scope);
        let origin = (file, r.evidence(file).scopes[scope].parent.unwrap_or(0));
        let found = r.lookup(origin, (&path[0], &path[1..]), usize::MAX, Namespace::Type);
        let [ty] = found.as_slice() else { return };
        self.impl_types.insert(addr, *ty);
        match identity {
            ImplIdentity::SimpleInherent | ImplIdentity::Inherent => self.add_scope(r, *ty, addr),
            ImplIdentity::SimpleTrait(spelling) => {
                self.add_overrides(r, *ty, (addr, origin), spelling)
            }
            ImplIdentity::Trait => {}
        }
    }

    fn add_overrides(
        &mut self,
        r: &'src Resolver,
        ty: Site,
        (addr, origin): (Address, Address),
        trait_name: &str,
    ) {
        if !r.evidence(ty.file).members.plain_types.contains(&ty.local) {
            return;
        }
        let found = r.lookup(origin, (trait_name, &[]), usize::MAX, Namespace::Type);
        let [trait_site] = found.as_slice() else {
            return;
        };
        let unit = &r.units[trait_site.file];
        let declaration = &unit.analysis.declarations[trait_site.local];
        if declaration.kind != "trait_item" {
            return;
        }
        let spelling = name(&unit.text[declaration.span.clone()]);
        let (file, scope) = addr;
        for (key, bindings) in &r.evidence(file).scopes[scope].names {
            for binding in bindings {
                let decl = &r.units[file].analysis.declarations[binding.decl];
                if decl.kind == "function_item"
                    && r.active_declaration(Site {
                        file,
                        local: binding.decl,
                    })
                {
                    self.by_name.entry(key).or_default().add_override(
                        ty,
                        Override {
                            spelling,
                            trait_site: *trait_site,
                            implementation: Site {
                                file,
                                local: binding.decl,
                            },
                        },
                    );
                }
            }
        }
    }

    fn add_scope(&mut self, r: &'src Resolver, owner: Site, (file, scope): Address) {
        for binding in r.evidence(file).scopes[scope].names.values().flatten() {
            self.add(
                r,
                owner,
                Site {
                    file,
                    local: binding.decl,
                },
            );
        }
    }

    fn add(&mut self, r: &'src Resolver, owner: Site, member: Site) {
        let unit = &r.units[member.file];
        let declaration = &unit.analysis.declarations[member.local];
        let access = match declaration.kind {
            "field_declaration" => Some(Access::Field),
            kind if kind.starts_with("function") => Some(Access::Method),
            _ => None,
        }
        .filter(|_| r.active_declaration(member));
        let span = &declaration.span;
        self.by_name
            .entry(name(&unit.text[span.clone()]))
            .or_default()
            .declare(
                owner,
                Member {
                    site: member,
                    access,
                },
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ineligible_inherent_name_hides_an_override_in_either_build_order() {
        let owner = Site { file: 0, local: 0 };
        let inherent = Site { file: 0, local: 1 };
        let declare = |names: &mut Candidates| {
            names.declare(
                owner,
                Member {
                    site: inherent,
                    access: None,
                },
            )
        };
        let add_override = |names: &mut Candidates| {
            names.add_override(
                owner,
                Override {
                    spelling: "Speak",
                    trait_site: Site { file: 0, local: 2 },
                    implementation: Site { file: 0, local: 3 },
                },
            )
        };
        let mut inherited_first = Candidates::default();
        add_override(&mut inherited_first);
        declare(&mut inherited_first);
        let mut declared_first = Candidates::default();
        declare(&mut declared_first);
        add_override(&mut declared_first);

        for names in [inherited_first, declared_first] {
            let found = names.select(&[owner], Access::Method, |_| {
                panic!("hidden trait was consulted")
            });
            assert!(found.is_empty());
            assert_eq!(names.declared(&owner)[0].site, inherent);
        }
    }

    #[test]
    fn paths_preserve_all_candidates_through_shared_children() {
        let site = |local| Site { file: 0, local };
        let mut table = Table::default();
        let member = |local| Member {
            site: site(local),
            access: None,
        };
        let outer = table.by_name.entry("outer").or_default();
        outer.declare(site(0), member(1));
        outer.declare(site(0), member(2));
        let inner = table.by_name.entry("inner").or_default();
        inner.declare(site(1), member(3));
        inner.declare(site(1), member(4));
        inner.declare(site(2), member(4));
        let path = ["outer".to_string(), "inner".to_string()];
        assert_eq!(
            table.follow(vec![site(0)], &path),
            [site(3), site(4), site(4)]
        );
        assert!(table.follow(vec![site(2)], &path).is_empty());
    }
}
