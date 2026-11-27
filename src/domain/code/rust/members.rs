use super::*;
#[derive(Default)]
pub(super) struct Members {
    pub bounds: HashMap<String, Vec<(std::ops::Range<usize>, Vec<Vec<String>>)>>,
    pub initializers: HashMap<usize, (usize, Projection)>,
    pub arguments: HashMap<usize, (usize, Vec<type_hints::Argument>)>,
    pub definitions: Vec<(usize, usize)>, // nominal declaration, member declaration
    simple_impls: HashMap<usize, Option<String>>,
    pub plain_types: HashSet<usize>,
    pub trait_scopes: HashMap<usize, usize>,
    impls: HashMap<usize, (Vec<String>, bool)>, // scope → type path, inherent
    pub types: HashMap<usize, (usize, Vec<String>)>, // declaration → lexical scope and written type
    pub uses: HashMap<usize, Receiver>,
}
#[derive(Clone, Copy)]
enum ImplIdentity<'a> {
    SimpleInherent,
    SimpleTrait(&'a str),
    Inherent,
    Trait,
}

struct ImplHeader<'a> {
    scope: usize,
    target: &'a [String],
    identity: ImplIdentity<'a>,
}

pub(super) struct SimpleImpl<'a> {
    pub scope: usize,
    pub target: &'a str,
    pub trait_name: Option<&'a str>,
}

impl<'a> ImplHeader<'a> {
    fn simple(self) -> Option<SimpleImpl<'a>> {
        let trait_name = match self.identity {
            ImplIdentity::SimpleInherent => None,
            ImplIdentity::SimpleTrait(name) => Some(name),
            ImplIdentity::Inherent | ImplIdentity::Trait => return None,
        };
        Some(SimpleImpl {
            scope: self.scope,
            target: self.target.first()?.as_str(),
            trait_name,
        })
    }
}

// Only these two written forms have a stable simple impl identity.
fn simple_header<'a>(header: &[Token<'a>]) -> Option<(&'a str, Option<&'a str>)> {
    let (ty, trait_name) = match header {
        [ty] => (ty, None),
        [tr, delimiter, ty] if delimiter.text == "for" && ident(tr.text) => (ty, Some(tr.text)),
        _ => return None,
    };
    ident(ty.text).then(|| (name(ty.text), trait_name))
}

impl Members {
    // These columns retain the historical checkpoint representation. Lookup and
    // identity consume different projections of the same impl header.
    fn impl_headers(&self) -> impl Iterator<Item = ImplHeader<'_>> {
        self.impls.iter().map(|(&scope, (target, inherent))| {
            let identity = match (self.simple_impls.get(&scope), inherent) {
                (Some(None), true) => ImplIdentity::SimpleInherent,
                (Some(Some(name)), false) => ImplIdentity::SimpleTrait(name),
                (_, true) => ImplIdentity::Inherent,
                (_, false) => ImplIdentity::Trait,
            };
            ImplHeader {
                scope,
                target,
                identity,
            }
        })
    }

    pub(super) fn simple_impls(&self) -> impl Iterator<Item = SimpleImpl<'_>> {
        self.impl_headers().filter_map(ImplHeader::simple)
    }

    fn trait_bounds(&self, path: &[String], offset: usize) -> Option<&[Vec<String>]> {
        let [parameter] = path else { return None };
        let (_, traits) = self
            .bounds
            .get(parameter)?
            .iter()
            .rev()
            .find(|(range, _)| range.contains(&offset))?;
        Some(traits)
    }

    pub(super) fn record_impl(&mut self, child: usize, t: &[Token], start: usize, end: usize) {
        let (ty, inherent) = if let Some((ty, spelling)) = simple_header(&t[start..end]) {
            self.simple_impls.insert(child, spelling.map(str::to_owned));
            (vec![ty.to_owned()], spelling.is_none())
        } else {
            impl_path(t, start, end)
        };
        if !ty.is_empty() {
            self.impls.insert(child, (ty, inherent));
        }
    }

    pub fn record_type(&mut self, id: usize, scope: usize, t: &[Token], at: usize) {
        let ty = self.annotated_path(id, scope, t, at);
        if !ty.is_empty() {
            self.types.insert(id, (scope, ty));
        }
    }

    pub fn record_self(&mut self, id: usize, scope: usize, t: &[Token], at: usize) {
        if word(t, at + 1) == ":" {
            self.annotated_path(id, scope, t, at + 2);
        }
        self.types.insert(id, (scope, vec!["Self".into()]));
    }

    fn annotated_path(&mut self, id: usize, scope: usize, t: &[Token], at: usize) -> Vec<String> {
        let (written, path) = written_path(t, at);
        let arguments = type_hints::arguments(t, written, &path);
        if !arguments.is_empty() {
            self.arguments.insert(id, (scope, arguments));
        }
        path
    }
}
enum TypeDependency {
    Alias(Site),
    Initializer(Site),
}

enum WrittenTypes {
    Known(Vec<Site>),
    Declared(Vec<Site>),
}

impl WrittenTypes {
    // Keep nominal declarations in source order. Return their original allocation
    // when value inference encounters no aliases.
    fn project(
        self,
        projection: Projection,
        alias: impl Fn(Site) -> bool,
        mut resolve: impl FnMut(Site) -> Option<Vec<Site>>,
    ) -> Option<Vec<Site>> {
        let declared = match self {
            Self::Known(sites) => return Some(sites),
            Self::Declared(sites) => sites,
        };
        let aliases = declared.iter().enumerate().filter(|(_, site)| alias(**site));
        let mut projected = Vec::new();
        if projection != Projection::Value {
            for (_, &ty) in aliases {
                projected.extend(resolve(ty)?);
            }
            return Some(projected);
        }
        let mut start = 0;
        for (index, &ty) in aliases {
            projected.extend_from_slice(&declared[start..index]);
            projected.extend(resolve(ty)?);
            start = index + 1;
        }
        if start == 0 {
            return Some(declared);
        }
        projected.extend_from_slice(&declared[start..]);
        Some(projected)
    }
}

pub(super) enum Receiver {
    Previous(usize),
    Literal(Vec<String>),
}
pub(super) fn path(t: &[Token], at: usize) -> Vec<String> {
    written_path(t, at).1
}

// The argument list begins after the written name, independent of any
// leading references, lifetime, or trait modifiers.
fn written_path(t: &[Token], mut i: usize) -> (std::ops::Range<usize>, Vec<String>) {
    while matches!(word(t, i), "&" | "mut" | "ref" | "dyn" | "impl") {
        i += 1;
        if word(t, i) == "'" {
            i += 2;
        }
    }
    named_path(t, i)
}

fn named_path(t: &[Token], mut i: usize) -> (std::ops::Range<usize>, Vec<String>) {
    let start = i;
    let mut end = start;
    let mut path = Vec::new();
    while path_segment(word(t, i)) {
        end = i + 1;
        path.push(name(word(t, i)).to_owned());
        if word(t, i + 1) != ":" || word(t, i + 2) != ":" {
            break;
        }
        i += 3;
    }
    (start..end, path)
}

pub(super) fn before(t: &[Token], end: usize) -> Vec<String> {
    if end == 0 {
        return vec![];
    }
    let mut i = end - 1;
    while let Some(previous) = preceding_segment(t, i) {
        i = previous;
    }
    path(t, i)
}
fn impl_path(t: &[Token], start: usize, end: usize) -> (Vec<String>, bool) {
    let mut header = ImplTarget {
        start,
        target: start,
        generics: word(t, start) == "<",
        angles: 0,
        inherent: true,
    };
    for i in start..end {
        match (word(t, i), header.angles) {
            ("<", _) => header.angles += 1,
            (">", _) => header.close_angles(i),
            ("for", 0) => {
                header.target = i + 1;
                header.inherent = false;
            }
            ("where", 0) => break,
            _ => {}
        }
    }
    (path(t, header.target), header.inherent)
}

struct ImplTarget {
    start: usize,
    target: usize,
    generics: bool,
    angles: usize,
    inherent: bool,
}

impl ImplTarget {
    fn close_angles(&mut self, close: usize) {
        self.angles = self.angles.saturating_sub(1);
        if (self.angles, self.generics, self.target) == (0, true, self.start) {
            self.target = close + 1;
        }
    }
}

mod table;
pub(super) use table::{Access, Table};

pub(super) struct PathLookup<'a> {
    pub at: Address,
    pub offset: usize,
    pub namespace: Namespace,
    pub spelling: (&'a str, &'a [String]),
}

#[derive(Clone, Copy)]
struct TypeLookup {
    site: Site,
    projection: Projection,
}

impl Table<'_> {
    fn self_type(&self, r: &Resolver, mut at: Address) -> Option<Site> {
        loop {
            if let Some(&ty) = self.impl_types.get(&at) {
                return Some(ty);
            }
            at.1 = r.evidence(at.0).scopes[at.1].parent?;
        }
    }
    pub fn path(&self, r: &Resolver, lookup: PathLookup<'_>) -> Vec<Site> {
        let PathLookup {
            at,
            offset,
            namespace,
            spelling: (head, tail),
        } = lookup;
        if head == "Self" {
            let owners = self.self_type(r, at).into_iter().collect();
            return self.follow(owners, tail);
        }
        let bare = r.lookup(at, (head, tail), offset, namespace);
        if tail.is_empty() || !bare.is_empty() {
            return bare;
        }
        // Try the longest written module/type prefix first, then follow only
        // declared fields, variants and inherent items.
        // Every remaining component must occur as a member somewhere before
        // any owner can match it. Stop before retrying impossible prefixes.
        for n in (0..tail.len()).rev().take_while(|&n| {
            self.by_name
                .get(tail[n].as_str())
                .is_some_and(|m| m.has_declarations)
        }) {
            let owners = r.lookup(at, (head, &tail[..n]), offset, Namespace::Type);
            let found = self.follow(owners, &tail[n..]);
            if !found.is_empty() { return found; }
        }
        vec![]
    }

    fn follow(&self, mut owners: Vec<Site>, path: &[String]) -> Vec<Site> {
        let mut next = Vec::new();
        for member in path {
            let members = self.by_name.get(member.as_str());
            let children =
                |owner: &Site| members.map_or(&[][..], |candidates| candidates.declared(owner));
            match owners.as_mut_slice() {
                [] => break,
                [owner] => {
                    let sites = children(owner);
                    owners.clear();
                    owners.extend(sites.iter().map(|m| m.site));
                }
                many => {
                    for owner in many {
                        next.extend(children(owner).iter().map(|m| m.site));
                    }
                    std::mem::swap(&mut owners, &mut next);
                    next.clear();
                }
            }
        }
        owners
    }

    fn follows_alias(r: &Resolver, site: Site, ty: Site) -> bool {
        ty != site && r.units[ty.file].analysis.declarations[ty.local].kind == "type_item"
    }

    fn include_struct(r: &Resolver, site: Site, projection: Projection, types: &mut Vec<Site>) {
        if types.is_empty()
            && projection == Projection::Value
            && r.units[site.file].analysis.declarations[site.local].kind == "struct_item"
        {
            types.push(site);
        }
    }

    fn declared_types(&self, r: &Resolver, at: Address, path: &[String]) -> Vec<Site> {
        self.path(
            r,
            PathLookup {
                at,
                offset: usize::MAX,
                namespace: Namespace::Type,
                spelling: (&path[0], &path[1..]),
            },
        )
    }

    fn written(&self, r: &Resolver, site: Site, projection: Projection) -> WrittenTypes {
        let evidence = &r.evidence(site.file).members;
        if let Some((scope, arguments)) = evidence.arguments.get(&site.local)
            && let Some(path) = type_hints::selected(r, (site.file, *scope), arguments, projection)
        {
            let types = self.declared_types(r, (site.file, *scope), path);
            if !types.is_empty() {
                return WrittenTypes::Known(types);
            }
        }
        let Some((scope, path)) = evidence.types.get(&site.local) else {
            return WrittenTypes::Known(Vec::new());
        };
        let offset = r.units[site.file].analysis.declarations[site.local]
            .span
            .start;
        let at = (site.file, *scope);
        let declared = match evidence.trait_bounds(path, offset) {
            Some(traits) => traits
                .iter()
                .flat_map(|p| r.lookup(at, (&p[0], &p[1..]), offset, Namespace::Type))
                .filter(|t| r.units[t.file].analysis.declarations[t.local].kind == "trait_item")
                .collect(),
            None => self.declared_types(r, at, path),
        };
        WrittenTypes::Declared(declared)
    }

    // The parallel preparer can finish a written type until it encounters an
    // alias or initializer row. Ordered inference supplies those dependencies.
    fn resolve_type(
        r: &Resolver,
        lookup: TypeLookup,
        written: WrittenTypes,
        mut resolve: impl FnMut(TypeDependency, Projection) -> Option<Vec<Site>>,
    ) -> Option<Vec<Site>> {
        let TypeLookup { site, projection } = lookup;
        let mut types = written.project(projection, |ty| Self::follows_alias(r, site, ty), |alias| {
            resolve(TypeDependency::Alias(alias), projection)
        })?;
        if types.is_empty()
            && let Some(&(local, op)) = r.evidence(site.file).members.initializers.get(&site.local)
        {
            let row = Site { file: site.file, local };
            types = resolve(TypeDependency::Initializer(row), projection.through(op)?)?;
        }
        Self::include_struct(r, site, projection, &mut types);
        Some(types)
    }

    pub(super) fn terminal(&self, r: &Resolver, site: Site) -> Option<Vec<Site>> {
        let written = self.written(r, site, Projection::Value);
        Self::resolve_type(
            r,
            TypeLookup {
                site,
                projection: Projection::Value,
            },
            written,
            |_, _| None,
        )
    }
    fn inferred(
        &mut self,
        r: &Resolver,
        names: &ForwardNames,
        lookup: TypeLookup,
        depth: usize,
    ) -> Vec<Site> {
        let TypeLookup { site, projection } = lookup;
        if depth >= 32 {
            return Vec::new();
        }
        if let Some(types) = self.inferred.get(&(site, projection)) {
            return types.clone();
        }
        let written = self.written(r, site, projection);
        let types = Self::resolve_type(r, lookup, written, |dependency, projection| {
            match dependency {
                TypeDependency::Alias(alias) => Some(self.inferred(r, names, TypeLookup { site: alias, projection }, depth + 1)),
                TypeDependency::Initializer(row) => Some(names.row(row).iter().flat_map(|&source| {
                    self.inferred(r, names, TypeLookup { site: source, projection }, depth + 1)
                }).collect()),
            }
        });
        let Some(types) = types else { return Vec::new() };
        self.inferred.insert((site, projection), types.clone());
        types
    }
    // Preparation defers a reference only when its preceding receiver use
    // requires ordered type inference. Literal receiver types resolve up front.
    pub(super) fn previous_members(
        &mut self,
        r: &Resolver,
        names: &ForwardNames,
        source: Site,
        previous: usize,
    ) -> Vec<Site> {
        let mut types = Vec::new();
        for &site in names.row(Site {
            file: source.file,
            local: previous,
        }) {
            types.extend(self.inferred(
                r,
                names,
                TypeLookup {
                    site,
                    projection: Projection::Value,
                },
                0,
            ));
        }
        self.select(
            r,
            source,
            &types,
            &Receiver::Previous(previous),
        )
    }

    pub(super) fn select(
        &self,
        r: &Resolver,
        source: Site,
        types: &[Site],
        receiver: &Receiver,
    ) -> Vec<Site> {
        let u = &r.units[source.file];
        let evidence = r.evidence(source.file);
        let lookup = &evidence.uses[source.local];
        let call = &u.analysis.calls[source.local];
        let (key, _) = lookup.spelling(&u.text[call.span.clone()]);
        let at = (source.file, lookup.scope);
        let offset = call.span.start;
        let access = Access::for_call(call.is_call && matches!(receiver, Receiver::Previous(_)));
        let Some(candidates) = self.by_name.get(key) else {
            return Vec::new();
        };
        candidates.select(types, access, |item| trait_visible(r, at, offset, item))
    }
}

fn trait_visible(r: &Resolver, at: Address, offset: usize, candidate: &table::Override) -> bool {
    let sites = r.lookup(at, (candidate.spelling, &[]), offset, Namespace::Type);
    sites.as_slice() == [candidate.trait_site]
}

/// The use carrying the written result of an unadorned call or field.
pub fn operand(t: &[Token], pairs: &[usize], mut i: usize) -> Option<usize> {
    if word(t, i) == ")" {
        i = pairs[i].checked_sub(1)?;
    }
    (i < t.len() && (ident(word(t, i)) || word(t, i) == "self")).then_some(i)
}
/// A grouping parenthesis contributes no type projection when its matching
/// close bounds exactly this expression. A comma still makes a tuple, and is
/// rejected by the path reader below.
pub(super) fn ungroup(
    t: &[Token],
    pairs: &[usize],
    mut rhs: std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    while !rhs.is_empty() {
        match word(t, rhs.start) {
            "&" | "mut" => rhs.start += 1,
            "(" if pairs[rhs.start] == rhs.end - 1 => {
                rhs.start += 1;
                rhs.end -= 1;
            }
            _ => break,
        }
    }
    rhs
}

pub(super) fn literal_type(
    t: &[Token],
    pairs: &[usize],
    rhs: std::ops::Range<usize>,
) -> Option<Vec<String>> {
    let close = rhs.end.checked_sub(1)?;
    if word(t, close) != "}" {
        return None;
    }
    let (written, ty) = named_path(t, rhs.start);
    (!ty.is_empty() && expression_arguments(t, pairs, written.end) == pairs[close]).then_some(ty)
}

pub(super) fn terminal_operand(
    t: &[Token],
    pairs: &[usize],
    start: usize,
    end: usize,
) -> Option<usize> {
    let expression = t.get(..end)?;
    let mut segment = OperandSegment::read(expression, pairs, start)?;
    while let OperandSuffix::Next(next) = segment.suffix {
        segment = OperandSegment::read(expression, pairs, next)?;
    }
    Some(segment.at)
}

struct OperandSegment {
    at: usize,
    suffix: OperandSuffix,
}

impl OperandSegment {
    fn read(expression: &[Token], pairs: &[usize], at: usize) -> Option<Self> {
        if !path_segment(expression.get(at)?.text) {
            return None;
        }
        let after_args = expression_arguments(expression, pairs, at + 1);
        let suffix = OperandSuffix::read(expression, pairs, after_args)?;
        Some(Self { at, suffix })
    }
}

enum OperandSuffix {
    Terminal,
    Next(usize),
}

impl OperandSuffix {
    fn read(expression: &[Token], pairs: &[usize], at: usize) -> Option<Self> {
        let after_call = if word(expression, at) == "(" {
            pairs[at].checked_add(1)?
        } else {
            at
        };
        match expression.get(after_call..)? {
            [] => Some(Self::Terminal),
            [dot, ..] if dot.text == "." => Some(Self::Next(after_call + 1)),
            [left, right, ..] if (left.text, right.text) == (":", ":") => {
                Some(Self::Next(after_call + 2))
            }
            _ => None,
        }
    }
}

pub fn initializer(
    t: &[Token],
    pairs: &[usize],
    rhs: std::ops::Range<usize>,
    expected: Projection,
) -> Option<(usize, Projection)> {
    let (end, suffix) = result_suffix(t, rhs.end);
    let projection = expected.through(suffix)?;
    let end = if projection == Projection::Element && iteration(t, end) {
        end - 4
    } else {
        end
    };
    Some((terminal_operand(t, pairs, rhs.start, end)?, projection))
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
        || !iteration(t, method - 1)
    {
        return None;
    }
    operand(t, pairs, method - 6)
}

fn nullary_method<'a>(t: &[Token<'a>], end: usize) -> Option<&'a str> {
    let suffix = t.get(end.checked_sub(4)?..end)?;
    match suffix {
        [dot, name, open, close] if (dot.text, open.text, close.text) == (".", "(", ")") => Some(name.text),
        _ => None,
    }
}
fn iteration(t: &[Token], end: usize) -> bool {
    matches!(nullary_method(t, end), Some("iter" | "iter_mut" | "into_iter"))
}
fn result_suffix(t: &[Token], mut end: usize) -> (usize, Projection) {
    let mut projection = Projection::Value;
    if end > 0 && word(t, end - 1) == "?" {
        projection = Projection::Unwrap;
        end -= 1;
    }
    if nullary_method(t, end) == Some("unwrap") {
        projection = Projection::Unwrap;
        end -= 4;
    }
    (before_await(t, end), projection)
}

fn before_await(tokens: &[Token], end: usize) -> usize {
    let Some(at) = end.checked_sub(2) else { return end };
    if (word(tokens, at), word(tokens, at + 1)) == (".", "await") {
        return at;
    }
    end
}

crate::domain::code::cache::record!(Members {
    bounds,
    initializers,
    arguments,
    definitions,
    simple_impls,
    plain_types,
    trait_scopes,
    impls,
    types,
    uses
});

crate::domain::code::cache::tagged!(Receiver {
    0 => Previous(row),
    1 => Literal(name),
});
