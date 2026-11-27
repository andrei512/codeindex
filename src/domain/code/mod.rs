//! Written declarations and conservative navigation for direct, named calls.
//! This lexical profile has partial coverage. Matching names in unrelated files
//! are never added to the target set of a call.
mod analyzer;
pub use analyzer::Language;
mod edit_proof;
mod project_resolver;
pub(crate) use project_resolver::{retained_files,ProjectView};
pub(crate) use edit_proof::GraphComparison;
pub(crate) use analyzer::{Document, LexedAnalyzer, Measured, ProjectInputs};
pub(crate) use lexer::ScanState;
mod cache;
mod cfamily;
mod comment_evidence;
pub(crate) use cfamily::{project::{CProject, compilation_database}, sdk::Receipt as CReceipt};
mod direct;
mod extended;
mod lexer;
mod make_reference;
mod markup;
pub(crate) mod navigation;
mod rust;
pub(crate) mod score;
pub(crate) mod roles;
mod script;
use lexer::{Kind, Token};
use std::{collections::HashMap, ops::Range, path::Path};


impl Language {
    pub fn for_path(path: &str) -> Option<Self> {
        let p = Path::new(path);
        let name = p.file_name()?.to_str()?;
        const MANIFESTS: &[(&str, Language)] = &[
            ("Dockerfile", Language::Dockerfile),
            ("Containerfile", Language::Dockerfile),
            ("Makefile", Language::Makefile),
            ("makefile", Language::Makefile),
            ("GNUmakefile", Language::Makefile),
            ("Procfile", Language::Procfile),
        ];
        let stem = name.split('.').next()?;
        if let Some((_, language)) = MANIFESTS.iter().find(|(written, _)| *written == stem) {
            return Some(*language);
        }
        if name == "CMakeLists.txt" {
            return Some(Self::CMake);
        }
        // Resolve the last component once. A leading dot belongs to the
        // basename; extensionless build manifests use their whole name.
        let suffix = match name.rsplit_once('.') {
            Some((stem, suffix)) if !stem.is_empty() => suffix,
            _ => name,
        };
        Self::for_suffix(suffix)
    }
    fn comment_style(self) -> lexer::LineComments {
        use lexer::LineComments::*;
        match self {
            Self::Python | Self::Starlark | Self::Shell | Self::Powershell | Self::Ruby | Self::Perl | Self::Nix | Self::Dockerfile | Self::Makefile | Self::CMake => Hash,
            Self::Sql | Self::PlPgSql | Self::Lua => Dash,
            Self::Php | Self::Hack | Self::Hcl => SlashOrHash,
            Self::Css | Self::Html => None,
            _ => Slash,
        }
    }
    fn line_comment(self, text: &str) -> bool { self.comment_style().starts(text) }
    fn block_comments(self) -> bool {
        matches!(
            self,
            Self::Js
                | Self::Rust
                | Self::Csharp
                | Self::Swift
                | Self::C
                | Self::Cpp
                | Self::Proto
                | Self::Css
                | Self::Sql | Self::Java | Self::Kotlin | Self::Dart | Self::Php | Self::Hack
                | Self::ObjectiveC | Self::Go | Self::Hcl | Self::PlPgSql | Self::Nix
        )
    }


}

#[derive(Debug)]
pub struct Declaration {
    pub span: Range<usize>,
    pub kind: &'static str,
}
#[derive(Debug)]
pub struct Call {
    pub is_call: bool,
    pub span: Range<usize>,
    pub owner: Option<usize>,
}
#[derive(Default)]
pub struct Analysis {
    direct: Vec<(usize, Vec<usize>)>,
    direct_evidence: Option<Box<direct::Evidence>>,
    rust: Option<Box<rust::Evidence>>,
    pub declarations: Vec<Declaration>,
    pub calls: Vec<Call>,
    script: Option<Box<script::Evidence>>,
}

fn indentation(text: &str, start: usize) -> usize {
    let begin = text[..start].rfind('\n').map_or(0, |i| i + 1);
    text[begin..]
        .bytes()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count()
}

mod scoring;
mod source_lines;
pub(crate) use scoring::{scored, score_file};

pub fn analyze(lang: Language, text: &str) -> Analysis {
    lang.analyzer().analyze(&Document { path: "", text })
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct Site {
    pub file: usize,
    pub local: usize,
}
/// Resolved names and relationships, keyed by source sites.
///
/// ```
/// use codeindex::domain::code::Names;
///
/// let names = Names { targets: Default::default(), ..Default::default() };
/// assert!(names.targets.is_empty());
/// ```
#[derive(Default)]
pub struct Names {
    pub public_labels: HashMap<Site, String>,
    pub by_name: HashMap<String, Vec<Site>>,
    pub incoming: HashMap<Site, Vec<Site>>,
    pub targets: HashMap<Site, Vec<Site>>,
}

pub(crate) use cfamily::projection::Projection;

#[derive(Default)]
pub(crate) struct ForwardNames {
    pub(crate) context_keys: Vec<String>,
    pub(crate) translation_uses: HashMap<Site, (usize, Option<Site>)>,
    pub(crate) translation_definitions: HashMap<Site, Vec<usize>>,
    pub(crate) public_labels: HashMap<Site, String>,
    pub(crate) by_name: HashMap<String, Vec<Site>>,
    targets: HashMap<Site, Vec<Site>>,
    rows: Vec<Vec<Site>>,
}

pub struct Unit<'a> {
    pub path: &'a str,
    pub text: &'a str,
    pub analysis: &'a Analysis,
}
/// Cache lexical project ownership for this snapshot. A lookup at a shared
/// ancestor settles each previously unvisited directory on the route.
fn nearest_owner<'a, T: Copy>(
    cache: &mut HashMap<&'a Path, Option<T>>,
    directory: &'a Path,
    mut local: impl FnMut(&Path) -> Option<T>,
) -> Option<T> {
    let mut visited = 0;
    let owner = directory.ancestors().find_map(|ancestor| {
        cache.get(ancestor).copied().or_else(|| {
            visited += 1;
            local(ancestor).map(Some)
        })
    }).flatten();
    for ancestor in directory.ancestors().take(visited) {
        cache.insert(ancestor, owner);
    }
    owner
}

/// Resolve a captured relative spelling without accessing the filesystem.
fn relative_path(dir: &Path, spelling: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let mut path = dir.to_owned();
    for part in Path::new(spelling).components() {
        match part {
            Component::Normal(name) => path.push(name),
            Component::CurDir => {}
            Component::ParentDir if path.pop() => {}
            _ => return None,
        }
    }
    Some(path)
}

/// A second declaration of a spelling blocks resolution, including fallback
/// to an outer scope. Keep the occupied key after it becomes ambiguous.
fn record_unique<K: Eq + std::hash::Hash, V>(names: &mut HashMap<K, Option<V>>, key: K, value: V) {
    names
        .entry(key)
        .and_modify(|existing| *existing = None)
        .or_insert(Some(value));
}

impl Declaration {
    pub fn searchable(&self) -> bool {
        !matches!(
            self.kind,
            "embedded_declaration"
                | "css_variable"
                | "shell_assignment"
                | "parameter"
                | "function_overload"
                | "function_expression"
                | "local_binding"
                | "source_file"
                | "field_declaration"
                | "enum_variant"
                | "generic_parameter"
                | "lifetime_parameter"
                | "associated_type"
        )
    }
}

impl Analysis {
    pub(super) fn declare_at(&mut self, span: std::ops::Range<usize>, kind: &'static str) -> usize {
        let id = self.declarations.len();
        self.declarations.push(Declaration { span, kind });
        id
    }

    pub(super) fn direct_edge(
        &mut self,
        target: usize,
        span: std::ops::Range<usize>,
        owner: Option<usize>,
        is_call: bool,
    ) {
        self.direct.push((self.calls.len(), vec![target]));
        self.calls.push(Call {
            span,
            owner,
            is_call,
        });
    }
}

impl Names {
    /// With identical path membership, these are the units whose bytes can
    /// affect binding or exported labels. All other units contribute only paths.
    pub(crate) fn consumes_text(path: &str) -> bool {
        Language::for_path(path).is_some() || compilation_database(path) || project_resolver::consumes_support_text(path)
    }
}

impl ForwardNames {
    const EMPTY: Site = Site { file: usize::MAX, local: 0 };
    const MANY: Site = Site { file: usize::MAX, local: 1 };

    pub(crate) fn row(&self, site: Site) -> &[Site] {
        if self.rows.is_empty() {
            return self.targets.get(&site).map_or(&[], Vec::as_slice);
        }
        let slot = &self.rows[site.file][site.local];
        match *slot {
            Self::EMPTY => &[],
            Self::MANY => &self.targets[&site],
            _ => std::slice::from_ref(slot),
        }
    }

    pub(crate) fn all_rows(&self) -> impl Iterator<Item = &[Site]> {
        self.rows.iter().enumerate().flat_map(move |(file, rows)| {
            rows.iter().enumerate().map(move |(local, _)| self.row(Site {file,local}))
        })
    }

    pub(crate) fn bind_one(&mut self, source: Site, target: Site) {
        if let Some(slot) = self.rows.get_mut(source.file).map(|file| &mut file[source.local])
            && (*slot == Self::EMPTY || *slot == target)
        {
            *slot = target;
            return;
        }
        let Some(row) = self.targets.get_mut(&source) else {
            self.bind(source, vec![target]);
            return;
        };
        if let Err(at) = row.binary_search(&target) {
            row.insert(at, target);
        }
    }

    fn bind(&mut self, site: Site, mut targets: Vec<Site>) {
        if targets.is_empty() {
            return;
        }
        if !self.rows.is_empty() {
            let slot = &mut self.rows[site.file][site.local];
            if *slot != Self::MANY {
                if *slot != Self::EMPTY {
                    targets.push(*slot);
                }
                targets.sort_unstable();
                targets.dedup();
                if let [target] = targets.as_slice() {
                    *slot = *target;
                } else {
                    *slot = Self::MANY;
                    self.targets.insert(site, targets);
                }
                return;
            }
        }
        let stored = self.targets.entry(site).or_default();
        if stored.is_empty() {
            *stored = targets;
        } else {
            stored.extend(targets);
        }
        stored.sort_unstable();
        stored.dedup();
    }

    pub(crate) fn forward(units: &[Unit<'_>]) -> Self {
        Self::bind_project(&ProjectView::new(units))
    }

    pub(crate) fn bind_project(project:&ProjectView) -> Self {
        let mut names=Self::with_rows(project.units);
        names.complete_partitions(project,&[]);
        names
    }

    pub(crate) fn with_rows(units: &[Unit<'_>]) -> Self {
        let rows = units.iter().map(|u| vec![Self::EMPTY; u.analysis.calls.len()]).collect();
        Self { rows, ..Self::default() }
    }

    pub(crate) fn declarations(units: &[Unit<'_>]) -> Self {
        let mut names = Self::default();
        names.collect_declarations(units);
        names
    }

    fn collect_declarations(&mut self, units: &[Unit<'_>]) {
        for (file, u) in units.iter().enumerate() {
            for (local, d) in u.analysis.declarations.iter().enumerate() {
                if !d.searchable() {
                    continue;
                }
                let name = &u.text[d.span.clone()];
                let site = Site { file, local };
                if let Some(sites) = self.by_name.get_mut(name) {
                    sites.push(site);
                } else {
                    self.by_name.insert(name.to_owned(), vec![site]);
                }
            }
        }
    }

    pub(crate) fn complete_partitions(&mut self, project:&ProjectView, retained: &[bool]) {
        self.collect_declarations(project.units);
        project_resolver::resolve(project, self, retained);
    }

    fn resolve(units: &[Unit<'_>], mut names: Self) -> Self {
        names.complete_partitions(&ProjectView::new(units), &[]);
        names
    }
}

impl Names {
    pub fn build(units: &[Unit<'_>]) -> Self {
        // Public callers may edit the call list after parsing. Sparse targets
        // can retain embedded bindings whose source index is outside that list.
        let forward = ForwardNames::resolve(units, ForwardNames::default());
        let mut names = Self {
            public_labels: forward.public_labels,
            by_name: forward.by_name,
            targets: forward.targets,
            incoming: HashMap::new(),
        };
        // Derive the reverse directory from the complete forward relation.
        // Site order is canonical across worker orders and randomized hash seeds.
        let valid_source = |site: &Site| {
            units.get(site.file).is_some_and(|unit| site.local < unit.analysis.calls.len())
        };
        for (&source, targets) in names.targets.iter().filter(|(site, _)| valid_source(site)) {
            for &target in targets {
                names.incoming.entry(target).or_default().push(source);
            }
        }
        for sources in names.incoming.values_mut() {
            sources.sort_unstable();
        }
        names
    }
}

crate::domain::code::cache::record!(Analysis { direct, direct_evidence, rust, declarations, calls, script });

crate::domain::code::cache::record!(Call { is_call, span, owner });

crate::domain::code::cache::record!(Declaration { span, kind });

impl Analysis{
pub(crate) fn encode(&self,w:&mut Vec<u8>){ use cache::Wire;self.save(w); }
pub(crate) fn encode_bounded(&self, w: &mut cache::BoundedOutput) {
    use cache::Wire;
    self.save(w);
}
pub(crate) fn decode(r:&mut &[u8])->std::io::Result<Self>{use cache::Wire; Self::load(r)}
}

pub(crate) use cache::BoundedOutput;

#[cfg(test)]
mod public_bindings_tests {
    use super::*;

    #[test]
    fn filtered_public_calls_preserve_embedded_targets() {
        let text = ":root { --x: red; } a { color: var(--x); }";
        let mut analysis = analyze(Language::Css, text);
        let build = |analysis: &Analysis| Names::build(&[Unit { path: "style.css", text, analysis }]);
        let original = build(&analysis);
        assert!(!original.targets.is_empty());
        analysis.calls.clear();
        let filtered = build(&analysis);
        assert_eq!(filtered.targets, original.targets);
        assert!(filtered.incoming.is_empty());
    }
}

#[cfg(test)]
mod directory_owners {
    use super::*;

    #[test]
    fn caches_shared_ancestors_and_missing_projects() {
        let mut cache = HashMap::new();
        let mut probes = 0;
        let mut resolve = |dir| {
            nearest_owner(&mut cache, Path::new(dir), |candidate| {
                probes += 1;
                (candidate == Path::new("app")).then_some(1)
            })
        };
        for dir in ["app/a/b/c", "app/a/b/d", "app/a/b/d"] {
            assert_eq!(resolve(dir), Some(1));
        }
        for dir in ["other/a/b", "other/a/c", "other/a/c"] {
            assert_eq!(resolve(dir), None);
        }
        assert_eq!(probes, 10);
    }
}
