use super::{Analysis, Call, Kind, Token};
use crate::domain::code::{ForwardNames, Site, Unit};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path},
};
#[derive(Default)]
pub(in crate::domain::code) struct Evidence {
    package: String,
    types: HashMap<String, usize>,
    imports: Vec<(String, bool)>,
    lookups: Vec<(std::ops::Range<usize>, String, String)>,
}
fn word<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |x| x.text)
}
fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}.{name}")
    }
}
fn scalar(name: &str) -> bool {
    matches!(
        name,
        "double"
            | "float"
            | "int32"
            | "int64"
            | "uint32"
            | "uint64"
            | "sint32"
            | "sint64"
            | "fixed32"
            | "fixed64"
            | "sfixed32"
            | "sfixed64"
            | "bool"
            | "string"
            | "bytes"
    )
}
fn type_position(t: &[Token], last: usize, prev: &str) -> bool {
    match t.get(last + 1) {
        Some(v) if v.kind == Kind::Word => word(t, last + 2) == "=",
        Some(v) if v.text == ")" => matches!(prev, "(" | "." | "stream"),
        Some(v) if v.text == ">" => matches!(prev, "," | "."),
        _ => false,
    }
}
impl Evidence {
    // Tag 0 predates persisted package names; its extraction revision is rebuilt.
    pub(super) fn load_legacy(input: &mut &[u8]) -> std::io::Result<Self> {
        use crate::domain::code::cache::Wire;
        Ok(Self {
            package: String::new(),
            types: Wire::load(input)?,
            imports: Wire::load(input)?,
            lookups: Wire::load(input)?,
        })
    }

    fn import(&mut self, t: &[Token], i: usize) {
        let public = word(t, i + 1) == "public";
        let j = i + 1 + usize::from(matches!(word(t, i + 1), "public" | "weak"));
        if t.get(j).is_some_and(|v| v.kind == Kind::String) {
            self.imports
                .push((word(t, j).trim_matches(['\'', '"']).into(), public))
        }
    }

    fn type_use(&mut self, scope: &str, head: TypeHead<'_, '_>, out: &mut Analysis) {
        let t = head.tokens;
        let path = t[head.start].span.start..t[head.last].span.end;
        let components = (head.first + 2..=head.last)
            .step_by(2)
            .map(|k| t[k].span.clone());
        let first = out.calls.len();
        // Publish the path followed by each identifier after the first component.
        out.calls
            .extend(std::iter::once(path).chain(components).map(|span| Call {
                span,
                owner: None,
                is_call: false,
            }));
        let key = t[head.start..=head.last].iter().map(|v| v.text).collect();
        self.lookups
            .push((first..out.calls.len(), scope.to_owned(), key));
    }
}

struct TypeHead<'t, 's> {
    tokens: &'t [Token<'s>],
    first: usize,
    last: usize,
    start: usize,
    previous: &'s str,
}

impl<'t, 's> TypeHead<'t, 's> {
    fn at(tokens: &'t [Token<'s>], first: usize) -> Option<Self> {
        let prev = first.checked_sub(1).map_or("", |j| word(tokens, j));
        if tokens[first].kind != Kind::Word || matches!(prev, "package" | "import") {
            return None;
        }
        let mut last = first;
        while word(tokens, last + 1) == "."
            && tokens.get(last + 2).is_some_and(|s| s.kind == Kind::Word)
        {
            last += 2;
        }
        Some(Self {
            tokens,
            first,
            last,
            start: first - usize::from(prev == "."),
            previous: prev,
        })
    }

    fn is_type(&self) -> bool {
        type_position(self.tokens, self.last, self.previous)
            && !scalar(self.tokens[self.first].text)
    }
}

#[derive(Default)]
struct Nesting {
    scope: String,
    pending: Option<String>,
    parents: Vec<usize>,
}

impl Nesting {
    fn declare(
        &mut self,
        keyword: &str,
        name: &Token,
        ev: &mut Evidence,
        out: &mut Analysis,
    ) -> bool {
        let (kind, opens, exports_type) = match keyword {
            "rpc" => ("rpc_name", false, false),
            "service" => ("service_name", true, false),
            "message" => ("message_name", true, true),
            "enum" => ("enum_name", true, true),
            _ => return false,
        };
        if opens {
            let key = join(&self.scope, name.text);
            if exports_type {
                ev.types.insert(key.clone(), out.declarations.len());
            }
            self.pending = Some(key);
        }
        out.declare_at(name.span.clone(), kind);
        true
    }

    fn enter(&mut self) {
        self.parents.push(self.scope.len());
        if let Some(child) = self.pending.take() {
            self.scope = child;
        }
    }
    fn leave(&mut self) {
        if let Some(len) = self.parents.pop() {
            self.scope.truncate(len);
        }
        self.pending = None;
    }

    fn delimiter(&mut self, text: &str) -> bool {
        match text {
            "{" => self.enter(),
            "}" => self.leave(),
            ";" => self.pending = None,
            _ => return false,
        }
        true
    }
}

struct ProtoScan<'t, 's> {
    tokens: &'t [Token<'s>],
    nesting: Nesting,
    evidence: Evidence,
    output: Analysis,
}

impl ProtoScan<'_, '_> {
    fn package(&mut self, i: usize) -> usize {
        let mut end = i + 1;
        while let Some(token) = self.tokens.get(end).filter(|token| token.text != ";") {
            self.nesting.scope.push_str(token.text);
            end += 1;
        }
        self.evidence.package.clone_from(&self.nesting.scope);
        end
    }

    fn named_declaration(&mut self, i: usize) -> bool {
        let Some(name) = self
            .tokens
            .get(i + 1)
            .filter(|name| name.kind == Kind::Word)
        else {
            return false;
        };
        self.nesting.declare(
            self.tokens[i].text,
            name,
            &mut self.evidence,
            &mut self.output,
        )
    }

    fn type_head(&mut self, i: usize) -> usize {
        let Some(head) = TypeHead::at(self.tokens, i) else {
            return i;
        };
        let end = head.last;
        if head.is_type() {
            self.evidence
                .type_use(&self.nesting.scope, head, &mut self.output);
        }
        end
    }

    fn step(&mut self, i: usize) -> usize {
        let text = self.tokens[i].text;
        match text {
            "package" => return self.package(i),
            "import" => self.evidence.import(self.tokens, i),
            delimiter if self.nesting.delimiter(delimiter) => {}
            _ if self.named_declaration(i) => return i + 1,
            _ => return self.type_head(i),
        }
        i
    }
}

pub(super) fn analyze(tokens: &[Token]) -> Analysis {
    let mut scan = ProtoScan {
        tokens,
        nesting: Nesting::default(),
        evidence: Evidence::default(),
        output: Analysis::default(),
    };
    let mut i = 0;
    while i < tokens.len() {
        i = scan.step(i) + 1;
    }
    scan.output.direct_evidence = Some(Box::new(super::Evidence::Proto(scan.evidence)));
    scan.output
}
fn evidence<'a>(unit: &'a Unit) -> Option<&'a Evidence> {
    match unit.analysis.direct_evidence.as_deref()? {
        super::Evidence::Proto(e) => Some(e),
        _ => None,
    }
}
fn locate_import(path: &str, import: &str, paths: &HashMap<&str, usize>) -> Option<usize> {
    let input = Path::new(import);
    if input
        .components()
        .any(|s| !matches!(s, Component::Normal(_)))
    {
        return None;
    }
    Path::new(path)
        .parent()?
        .ancestors()
        .find_map(|base| paths.get(base.join(input).to_str()?).copied())
}
#[derive(Default)]
pub(in crate::domain::code) struct ProtoGraph<'a> {
    edges: HashMap<usize, Vec<(usize, bool)>>,
    // A type name is usually unique across the project. Keep additional
    // declarations only for colliding names, filtering them by import visibility.
    types: HashMap<&'a str, Site>,
    namespaces: HashMap<&'a str, HashSet<usize>>,
    collisions: HashMap<&'a str, Vec<Site>>,
}

impl<'a> ProtoGraph<'a> {
    fn register(&mut self, file: usize, evidence: &'a Evidence) {
        // Even an otherwise empty imported file declares its entire package.
        let mut package = evidence.package.as_str();
        while !package.is_empty() {
            self.namespaces.entry(package).or_default().insert(file);
            package = package.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        for (key, &local) in &evidence.types {
            let site = Site { file, local };
            match self.types.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(site);
                }
                std::collections::hash_map::Entry::Occupied(_) => {
                    self.collisions.entry(key).or_default().push(site)
                }
            }
        }
    }

    fn candidates(&self, name: &str) -> impl Iterator<Item = Site> + '_ {
        self.types
            .get(name)
            .into_iter()
            .chain(self.collisions.get(name).into_iter().flatten())
            .copied()
    }
    fn contains(&self, key: &str, visible: &HashSet<usize>) -> bool {
        self.candidates(key)
            .any(|site| visible.contains(&site.file))
            || self
                .namespaces
                .get(key)
                .is_some_and(|files| !files.is_disjoint(visible))
    }
    fn visible(&self, file: usize) -> VisibleTypes<'_, 'a> {
        let mut visible = HashSet::from([file]);
        let mut todo: Vec<_> = self.edges[&file].iter().map(|&(id, _)| id).collect();
        while let Some(id) = todo.pop() {
            if visible.insert(id)
                && let Some(edges) = self.edges.get(&id)
            {
                todo.extend(edges.iter().filter_map(|&(f, public)| public.then_some(f)));
            }
        }
        VisibleTypes {
            imports: self,
            files: visible,
            key: String::new(),
        }
    }
}

fn links<'p, 'a>(project: &'p crate::domain::code::ProjectView<'a, '_>) -> &'p ProtoGraph<'a> {
    project.proto.get_or_init(|| {
        let mut graph = ProtoGraph::default();
        for (file, u) in project.units.iter().enumerate() {
            let Some(ev) = evidence(u) else { continue };
            let edges = ev
                .imports
                .iter()
                .filter_map(|(p, public)| {
                    Some((locate_import(u.path, p, project.paths())?, *public))
                })
                .collect();
            graph.edges.insert(file, edges);
            graph.register(file, ev);
        }
        graph
    })
}
// Each source file searches the same set of visible imports. Reuse the lookup
// key while walking out through its containing type and package names.
struct VisibleTypes<'graph, 'src> {
    imports: &'graph ProtoGraph<'src>,
    files: HashSet<usize>,
    key: String,
}
enum Lookup {
    Unique(Site),
    Blocked,
    Outer,
}

impl VisibleTypes<'_, '_> {
    fn within(&mut self, prefix: &str, spelling: &str, head: Option<usize>) -> Lookup {
        self.key.clear();
        if !prefix.is_empty() {
            self.key.push_str(prefix);
            self.key.push('.');
        }
        let relative = self.key.len();
        self.key.push_str(spelling);
        let mut matches = self
            .imports
            .candidates(&self.key)
            .filter(|site| self.files.contains(&site.file));
        match (matches.next(), matches.next()) {
            (Some(site), None) => Lookup::Unique(site),
            (Some(_), Some(_)) => Lookup::Blocked,
            _ if !prefix.is_empty()
                && head.is_some_and(|h| {
                    self.imports
                        .contains(&self.key[..relative + h], &self.files)
                }) =>
            {
                Lookup::Blocked
            }
            _ => Lookup::Outer,
        }
    }

    fn target(&mut self, scope: &str, spelling: &str) -> Option<Site> {
        let mut prefix = if spelling.starts_with('.') { "" } else { scope };
        let spelling = spelling.trim_start_matches('.');
        let head = spelling.find('.');
        loop {
            match self.within(prefix, spelling, head) {
                Lookup::Unique(site) => return Some(site),
                Lookup::Blocked => return None,
                Lookup::Outer if prefix.is_empty() => return None,
                Lookup::Outer => prefix = prefix.rsplit_once('.').map_or("", |(p, _)| p),
            }
        }
    }

    fn bind(&mut self, file: usize, evidence: &Evidence, names: &mut ForwardNames) {
        let mut resolved = HashMap::new();
        for (sites, scope, spelling) in &evidence.lookups {
            let target = resolved
                .entry((scope.as_str(), spelling.as_str()))
                .or_insert_with(|| self.target(scope, spelling));
            if let Some(target) = *target {
                sites
                    .clone()
                    .for_each(|local| names.bind_one(Site { file, local }, target));
            }
        }
    }
}

fn resolve(files: &[usize], units: &[Unit], imports: &ProtoGraph, names: &mut ForwardNames) {
    for &file in files {
        let e = evidence(&units[file]).unwrap();
        if !e.lookups.is_empty() {
            imports.visible(file).bind(file, e, names);
        }
    }
}

crate::domain::code::cache::record!(Evidence {
    package,
    types,
    imports,
    lookups
});

pub(in crate::domain::code) struct ProtoResolver;
impl crate::domain::code::project_resolver::ProjectResolver for ProtoResolver {
    fn refine(
        &self,
        project: &crate::domain::code::ProjectView,
        prior: &[Option<usize>],
        keep: &mut [bool],
        _: &mut dyn FnMut(usize) -> std::io::Result<crate::domain::code::Analysis>,
    ) -> std::io::Result<()> {
        let edges = &links(project).edges;
        crate::domain::code::project_resolver::retain_import_clients(
            prior,
            keep,
            edges
                .iter()
                .flat_map(|(&file, imps)| imps.iter().map(move |&(target, _)| (file, target))),
            edges.keys().copied(),
        );
        Ok(())
    }
    fn reads(&self, _: &str, lang: Option<crate::domain::code::Language>) -> bool {
        use crate::domain::code::Language;
        lang == Some(Language::Proto)
    }
    fn bind(
        &self,
        files: &[usize],
        project: &crate::domain::code::project_resolver::ProjectView,
        output: &mut crate::domain::code::ForwardNames,
    ) {
        resolve(files, project.units, links(project), output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_visible_empty_package_shadows_a_relative_type_prefix() {
        let outer = Evidence {
            package: "p".into(),
            types: HashMap::from([("p.Branch.Target".into(), 0)]),
            ..Evidence::default()
        };
        let inner = Evidence {
            package: "p.deep.Branch".into(),
            ..Evidence::default()
        };
        let mut graph = ProtoGraph::default();
        graph.register(0, &outer);
        graph.register(1, &inner);
        let mut visible = VisibleTypes {
            imports: &graph,
            files: HashSet::from([0, 1]),
            key: String::new(),
        };
        assert!(visible.target("p.deep.Use", "Branch.Target").is_none());
        assert!(visible.target("p.deep.Use", ".p.Branch.Target").is_some());
        visible.files.remove(&1);
        assert!(visible.target("p.deep.Use", "Branch.Target").is_some());
    }
}
