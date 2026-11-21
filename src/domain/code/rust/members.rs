use super::*;
#[derive(Default)]
pub(super) struct Members {
    pub bounds: HashMap<String, Vec<(std::ops::Range<usize>, Vec<Vec<String>>)>>,
    pub initializers: HashMap<usize, (usize, Projection)>,
    pub arguments: HashMap<usize, (usize, Vec<type_hints::Argument>)>,
    pub definitions: Vec<(usize, usize)>, // nominal declaration, member declaration
    pub simple_impls: HashMap<usize, Option<String>>,
    pub plain_types: HashSet<usize>,
    pub trait_scopes: HashMap<usize, usize>,
    pub impls: HashMap<usize, (Vec<String>, bool)>, // scope → type path, inherent
    pub types: HashMap<usize, (usize, Vec<String>)>, // declaration → lexical scope and written type
    pub uses: HashMap<usize, Receiver>,
}
impl Members {
    pub fn note_arguments(&mut self, id: usize, scope: usize, t: &[Token], at: usize) {
        let arguments = type_hints::arguments(t, at);
        if !arguments.is_empty() {
            self.arguments.insert(id, (scope, arguments));
        }
    }
}
pub(super) enum Receiver {
    Previous(usize),
    Literal(Vec<String>),
}
pub(super) fn path(t: &[Token], mut i: usize) -> Vec<String> {
    while matches!(word(t, i), "&" | "mut" | "ref" | "dyn" | "impl") {
        i += 1;
        if word(t, i) == "'" {
            i += 2;
        }
    }
    let mut path = Vec::new();
    while ident(word(t, i)) || matches!(word(t, i), "self" | "super" | "Self" | "crate") {
        path.push(name(word(t, i)).to_owned());
        if word(t, i + 1) != ":" || word(t, i + 2) != ":" {
            break;
        }
        i += 3;
    }
    path
}

pub(super) fn before(t: &[Token], end: usize) -> Vec<String> {
    if end == 0 {
        return vec![];
    }
    let mut i = end - 1;
    while i >= 3 && word(t, i - 1) == ":" && word(t, i - 2) == ":" {
        i -= 3;
    }
    path(t, i)
}
pub(super) fn impl_path(t: &[Token], start: usize, end: usize) -> (Vec<String>, bool) {
    let mut depth = 0usize;
    let mut after = start;
    let mut inherent = true;
    for i in start..end {
        match word(t, i) {
            "<" => depth += 1,
            ">" => {
                depth = depth.saturating_sub(1);
                if depth == 0 && word(t, start) == "<" && after == start {
                    after = i + 1;
                }
            }
            "for" if depth == 0 => {
                after = i + 1;
                inherent = false;
            }
            "where" if depth == 0 => break,
            _ => {}
        }
    }
    (path(t, after), inherent)
}

pub(super) fn fields(
    a: &mut Analysis,
    e: &mut Evidence,
    t: &[Token],
    pairs: &[usize],
    excluded: &mut [bool],
    scope: usize,
    owner: usize,
    open: usize,
    is_enum: bool,
) {
    let close = pairs[open];
    let mut i = open + 1;
    while i < close && i < t.len() {
        match word(t, i) {
            "#" if word(t, i + 1) == "[" && pairs[i + 1] != usize::MAX => {
                i = pairs[i + 1] + 1;
                continue;
            }
            "pub" => {
                i += 1;
                if word(t, i) == "(" && pairs[i] != usize::MAX {
                    i = pairs[i] + 1;
                }
                continue;
            }
            _ => {}
        }
        if ident(word(t, i)) && (is_enum || word(t, i + 1) == ":") {
            let id = a.declarations.len();
            a.declarations.push(Declaration {
                span: t[i].span.clone(),
                kind: if is_enum {
                    "enum_variant"
                } else {
                    "field_declaration"
                },
            });
            e.members.definitions.push((owner, id));
            excluded[i] = true;
            if !is_enum {
                e.members.note_arguments(id, scope, t, i + 2);
                let ty = path(t, i + 2);
                if !ty.is_empty() {
                    e.members.types.insert(id, (scope, ty));
                }
            }
            if is_enum && word(t, i + 1) == "{" {
                fields(a, e, t, pairs, excluded, scope, id, i + 1, false);
            }
        }
        let mut depth = 0usize;
        while i < close && i < t.len() {
            if matches!(word(t, i), "(" | "[" | "{") && pairs[i] != usize::MAX {
                i = pairs[i] + 1;
                continue;
            }
            match word(t, i) {
                "<" => depth += 1,
                ">" => depth = depth.saturating_sub(1),
                "," if depth == 0 => {
                    i += 1;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

#[derive(Default)]
pub(super) struct Table {
    inferred: HashMap<(Site, Projection), Vec<Site>>,
    by_type: HashMap<Site, HashMap<String, Vec<Site>>>,
    impl_types: HashMap<Address, Site>,
    trait_overrides: HashMap<Site, HashMap<String, Vec<(Site, Site)>>>,
}
impl Table {
    pub fn build(r: &Resolver) -> Self {
        let mut table = Self::default();
        for (f, u) in r.units.iter().enumerate() {
            let Some(e) = &u.analysis.rust else {
                continue;
            };
            for &(owner, member) in &e.members.definitions {
                table.add(
                    r,
                    Site {
                        file: f,
                        local: owner,
                    },
                    Site {
                        file: f,
                        local: member,
                    },
                );
            }
            for (&scope, &trait_id) in &e.members.trait_scopes {
                for b in e.scopes[scope].names.values().flatten() {
                    table.add(
                        r,
                        Site {
                            file: f,
                            local: trait_id,
                        },
                        Site {
                            file: f,
                            local: b.decl,
                        },
                    );
                }
            }
            for (&scope, (path, inherent)) in &e.members.impls {
                let origin = (f, e.scopes[scope].parent.unwrap_or(0));
                let found = r.lookup(
                    origin,
                    &path[0],
                    &path[1..],
                    usize::MAX,
                    Namespace::Type,
                    &mut vec![],
                );
                let [ty] = found.as_slice() else {
                    continue;
                };
                table.impl_types.insert((f, scope), *ty);
                if let Some(Some(trait_name)) = e.members.simple_impls.get(&scope) {
                    let traits = r.lookup(
                        origin,
                        trait_name,
                        &[],
                        usize::MAX,
                        Namespace::Type,
                        &mut vec![],
                    );
                    if let [trait_site] = traits.as_slice()
                        && r.units[trait_site.file].analysis.declarations[trait_site.local].kind
                            == "trait_item"
                        && r.evidence(ty.file).members.plain_types.contains(&ty.local)
                    {
                        for (key, bindings) in &e.scopes[scope].names {
                            for b in bindings {
                                let decl = &u.analysis.declarations[b.decl];
                                if decl.kind == "function_item" && r.active(f, decl.span.start) {
                                    table
                                        .trait_overrides
                                        .entry(*ty)
                                        .or_default()
                                        .entry(key.clone())
                                        .or_default()
                                        .push((
                                            *trait_site,
                                            Site {
                                                file: f,
                                                local: b.decl,
                                            },
                                        ));
                                }
                            }
                        }
                    }
                }
                if *inherent {
                    for b in e.scopes[scope].names.values().flatten() {
                        table.add(
                            r,
                            *ty,
                            Site {
                                file: f,
                                local: b.decl,
                            },
                        );
                    }
                }
            }
        }
        table
    }
    fn add(&mut self, r: &Resolver, ty: Site, member: Site) {
        let u = &r.units[member.file];
        let d = &u.analysis.declarations[member.local];
        self.by_type
            .entry(ty)
            .or_default()
            .entry(name(&u.text[d.span.clone()]).to_owned())
            .or_default()
            .push(member);
    }
    fn self_type(&self, r: &Resolver, mut at: Address) -> Option<Site> {
        loop {
            if let Some(&ty) = self.impl_types.get(&at) {
                return Some(ty);
            }
            at.1 = r.evidence(at.0).scopes[at.1].parent?;
        }
    }
    pub fn path(
        &self,
        r: &Resolver,
        at: Address,
        head: &str,
        tail: &[String],
        offset: usize,
        ns: Namespace,
    ) -> Vec<Site> {
        let bare = if head == "Self" {
            self.self_type(r, at).into_iter().collect()
        } else {
            r.lookup(at, head, tail, offset, ns, &mut vec![])
        };
        if tail.is_empty() || head != "Self" && !bare.is_empty() {
            return bare;
        }
        // Resolve the longest written module/type prefix with a proven nominal type,
        // then walk only its declared fields, variants and inherent items.
        for n in (0..tail.len()).rev() {
            let mut owners = if head == "Self" {
                if n > 0 {
                    continue;
                }
                bare.clone()
            } else {
                r.lookup(at, head, &tail[..n], offset, Namespace::Type, &mut vec![])
            };
            for member in &tail[n..] {
                owners = owners
                    .into_iter()
                    .flat_map(|ty| {
                        self.by_type
                            .get(&ty)
                            .and_then(|m| m.get(member))
                            .into_iter()
                            .flatten()
                            .copied()
                    })
                    .collect();
                if owners.is_empty() {
                    break;
                }
            }
            if !owners.is_empty() {
                return owners;
            }
        }
        vec![]
    }
    fn inferred(
        &mut self,
        r: &Resolver,
        names: &Names,
        site: Site,
        projection: Projection,
        depth: usize,
    ) -> Vec<Site> {
        if depth >= 32 {
            return Vec::new();
        }
        if let Some(types) = self.inferred.get(&(site, projection)) {
            return types.clone();
        }
        let evidence = &r.evidence(site.file).members;
        let mut types = Vec::new();
        if let Some((scope, arguments)) = evidence.arguments.get(&site.local) {
            let at = (site.file, *scope);
            for arg in arguments {
                if type_hints::permits(r, at, arg, projection) {
                    types.extend(self.path(
                        r,
                        at,
                        &arg.inner[0],
                        &arg.inner[1..],
                        usize::MAX,
                        Namespace::Type,
                    ));
                }
            }
        }
        if types.is_empty()
            && let Some((scope, path)) = evidence.types.get(&site.local)
        {
            let offset = r.units[site.file].analysis.declarations[site.local]
                .span
                .start;
            let bound = (path.len() == 1)
                .then(|| evidence.bounds.get(&path[0]))
                .flatten()
                .and_then(|entries| {
                    entries
                        .iter()
                        .rev()
                        .find(|(range, _)| range.contains(&offset))
                });
            let at = (site.file, *scope);
            let declared = if let Some((_, traits)) = bound {
                traits
                    .iter()
                    .flat_map(|p| {
                        r.lookup(at, &p[0], &p[1..], offset, Namespace::Type, &mut vec![])
                    })
                    .filter(|t| r.units[t.file].analysis.declarations[t.local].kind == "trait_item")
                    .collect()
            } else {
                self.path(r, at, &path[0], &path[1..], usize::MAX, Namespace::Type)
            };
            for ty in declared {
                if r.units[ty.file].analysis.declarations[ty.local].kind == "type_item"
                    && ty != site
                {
                    types.extend(self.inferred(r, names, ty, projection, depth + 1));
                } else if projection == Projection::Value {
                    types.push(ty);
                }
            }
        }
        if types.is_empty()
            && let Some(&(local, op)) = evidence.initializers.get(&site.local)
        {
            let next = if projection == Projection::Value {
                op
            } else if op == Projection::Value {
                projection
            } else {
                return types;
            };
            for &s in names
                .targets
                .get(&Site {
                    file: site.file,
                    local,
                })
                .into_iter()
                .flatten()
            {
                types.extend(self.inferred(r, names, s, next, depth + 1));
            }
        }
        let kind = r.units[site.file].analysis.declarations[site.local].kind;
        if types.is_empty() && projection == Projection::Value && kind == "struct_item" {
            types.push(site);
        }
        self.inferred.insert((site, projection), types.clone());
        types
    }
    pub fn receiver(
        &mut self,
        r: &Resolver,
        names: &Names,
        at: Address,
        receiver: &Receiver,
        key: &str,
        offset: usize,
    ) -> Vec<Site> {
        let types = match receiver {
            Receiver::Literal(path) => {
                self.path(r, at, &path[0], &path[1..], offset, Namespace::Type)
            }
            Receiver::Previous(prev) => {
                let mut types = Vec::new();
                for site in names
                    .targets
                    .get(&Site {
                        file: at.0,
                        local: *prev,
                    })
                    .into_iter()
                    .flatten()
                {
                    types.extend(self.inferred(r, names, *site, Projection::Value, 0));
                }
                types
            }
        };
        let mut found = Vec::new();
        for ty in types {
            if let Some(items) = self.by_type.get(&ty).and_then(|m| m.get(key)) {
                found.extend_from_slice(items);
                continue;
            }
            if let Some(overrides) = self.trait_overrides.get(&ty).and_then(|m| m.get(key)) {
                for &(trait_site, item) in overrides {
                    let unit = &r.units[trait_site.file];
                    let decl = &unit.analysis.declarations[trait_site.local];
                    let spelling = name(&unit.text[decl.span.clone()]);
                    if r.lookup(at, spelling, &[], offset, Namespace::Type, &mut vec![])
                        .as_slice()
                        == [trait_site]
                    {
                        found.push(item);
                    }
                }
            }
        }
        found
    }
}

/// The use carrying the written result of an unadorned call or field.
pub fn operand(t: &[Token], pairs: &[usize], mut i: usize) -> Option<usize> {
    if word(t, i) == ")" {
        i = pairs[i].checked_sub(1)?;
    }
    (i < t.len() && (ident(word(t, i)) || word(t, i) == "self")).then_some(i)
}
pub fn simple_expression(t: &[Token], pairs: &[usize], mut i: usize, end: usize) -> Option<usize> {
    while matches!(word(t, i), "&" | "mut") {
        i += 1;
    }
    let mut result = None;
    while i < end {
        if !(ident(word(t, i)) || matches!(word(t, i), "Self" | "self" | "crate" | "super")) {
            return None;
        }
        result = Some(i);
        i += 1;
        if word(t, i) == "(" {
            if pairs[i] == usize::MAX {
                return None;
            }
            i = pairs[i] + 1;
        }
        if i == end {
            break;
        }
        if word(t, i) == "." {
            i += 1;
        } else if word(t, i) == ":" && word(t, i + 1) == ":" {
            i += 2;
        } else {
            return None;
        }
    }
    result
}

/// Recognize a single element argument of a direct standard collection iterator.
pub fn closure_element(t: &[Token], pairs: &[usize], bar: usize) -> Option<usize> {
    let call = bar.checked_sub(1 + usize::from(bar > 0 && word(t, bar - 1) == "move"))?;
    let method = call.checked_sub(1)?;
    if method < 6
        || word(t, call) != "("
        || word(t, method - 1) != "."
        || !matches!(
            word(t, method),
            "map"
                | "filter"
                | "filter_map"
                | "find"
                | "find_map"
                | "any"
                | "all"
                | "position"
                | "for_each"
                | "take_while"
                | "skip_while"
        )
        || word(t, method - 2) != ")"
        || word(t, method - 3) != "("
        || !matches!(word(t, method - 4), "iter" | "iter_mut" | "into_iter")
        || word(t, method - 5) != "."
    {
        return None;
    }
    operand(t, pairs, method - 6)
}

pub fn strip_iteration(t: &[Token], end: usize, projection: Projection) -> usize {
    if projection == Projection::Element
        && end >= 4
        && word(t, end - 4) == "."
        && matches!(word(t, end - 3), "iter" | "iter_mut" | "into_iter")
        && word(t, end - 2) == "("
        && word(t, end - 1) == ")"
    {
        end - 4
    } else {
        end
    }
}
pub fn result_suffix(t: &[Token], mut end: usize) -> (usize, Projection) {
    let mut projection = Projection::Value;
    if end > 0 && word(t, end - 1) == "?" {
        projection = Projection::Unwrap;
        end -= 1;
    }
    if end >= 4
        && word(t, end - 4) == "."
        && word(t, end - 3) == "unwrap"
        && word(t, end - 2) == "("
        && word(t, end - 1) == ")"
    {
        projection = Projection::Unwrap;
        end -= 4;
    }
    if end >= 2 && word(t, end - 2) == "." && word(t, end - 1) == "await" {
        end -= 2;
    }
    (end, projection)
}

crate::domain::code::cache::record!(Members { bounds, initializers, arguments, definitions, simple_impls, plain_types, trait_scopes, impls, types, uses });

impl crate::domain::code::cache::Wire for Receiver{fn save(&self,w:&mut Vec<u8>){match self{Self::Previous(a)=>{ w.push(0); a.save(w);},Self::Literal(a)=>{ w.push(1); a.save(w);}}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Previous(Wire::load(r)?)),1=>Ok(Self::Literal(Wire::load(r)?)),_=>Err(crate::domain::code::cache::bad())}}}
