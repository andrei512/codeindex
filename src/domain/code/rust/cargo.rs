//! Local manifest evidence only. Dependencies without a captured path declaration
//! cannot introduce a Rust crate into name lookup.
use super::{HashMap, Path, PathBuf, Unit};
use crate::domain::code::relative_path;
#[derive(Default)]
struct Package<'a> {
    name: String,
    package_name: String,
    version: Option<String>,
    inherited_version: bool,
    workspace_version: Option<String>,
    workspace_root: bool,
    designated_workspace: Option<String>,
    members: Vec<MemberPattern<'a>>,
    exclude: Vec<MemberPattern<'a>>,
    targets: Targets,
    explicit: HashMap<String, String>,
    inherited: Vec<String>,
    workspace: HashMap<String, String>,
}
/// Cargo punctuation in the supported inline manifest forms is significant
/// only outside quoted strings. Preserve source slices for the field readers.
fn unquoted(text: &str, separator: u8) -> Option<usize> {
    // Most manifest lines contain no comment. Avoid tracking quote state
    // unless the line actually contains the delimiter we need.
    if !text.as_bytes().contains(&separator) {
        return None;
    }
    let mut bytes = text.bytes().enumerate();
    while let Some((at, byte)) = bytes.next() {
        match byte {
            b'\'' | b'"' => skip_quoted(&mut bytes, byte),
            byte if byte == separator => return Some(at),
            _ => {}
        }
    }
    None
}

fn skip_quoted(bytes: &mut impl Iterator<Item = (usize, u8)>, quote: u8) {
    while let Some((_, byte)) = bytes.next() {
        match byte {
            b'\\' if quote == b'"' => {
                bytes.next();
            }
            byte if byte == quote => break,
            _ => {}
        }
    }
}

fn fields(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let end = unquoted(text, b',').unwrap_or(text.len());
        let field = &text[..end];
        text = text.get(end + 1..).unwrap_or("");
        Some(field)
    })
}

fn atom(s: &str) -> Option<&str> {
    let value = s.trim();
    let q = value.chars().next()?;
    if !matches!(q, '"' | '\'') {
        return None;
    }
    let text = value[1..].split_once(q)?.0;
    (!text.contains('\\')).then_some(text)
}
/// A quoted simple key and a bare key name the same manifest field.
fn entry(raw: &str) -> Option<(&str, &str)> {
    let raw = raw.trim_start();
    let Some(quote @ (b'\'' | b'"')) = raw.bytes().next() else {
        let (key, value) = raw.split_once('=')?;
        return Some((key.trim_end(), value.trim()));
    };
    let end = unquoted(raw, b'=')?;
    let quoted = raw[..end].trim_end();
    let key = quoted
        .strip_prefix(char::from(quote))?
        .strip_suffix(char::from(quote))?;
    // Escaped and compound keys need a full TOML key decoder.
    if key.contains([char::from(quote), '\\', '.']) {
        return None;
    }
    Some((key, raw[end + 1..].trim()))
}
#[derive(Default)]
struct DependencyFields<'a> {
    path: Option<&'a str>,
    workspace: Option<&'a str>,
}
impl<'a> DependencyFields<'a> {
    fn read(value: &'a str) -> Self {
        let mut selected = Self::default();
        let body = value.trim().trim_start_matches('{').trim_end_matches('}');
        for (key, value) in fields(body).filter_map(entry) {
            let entry = match key {
                "path" => &mut selected.path,
                "workspace" => &mut selected.workspace,
                _ => continue,
            };
            entry.get_or_insert(value);
        }
        selected
    }
}
impl Package<'_> {
    // Unsupported exclusion globs cannot prove a member is included.
    fn includes(&self, relative: &str) -> bool {
        self.exclude
            .iter()
            .all(|rule| rule.matches(relative) == Some(false))
            && self
                .members
                .iter()
                .any(|rule| rule.matches(relative) == Some(true))
    }

    fn externs(
        &self,
        dir: &Path,
        packages: &HashMap<PathBuf, Package>,
        libraries: &HashMap<&Path, usize>,
    ) -> ExternMap {
        let mut map = HashMap::new();
        let mut link = |alias: &str, base: &Path, value: &str| {
            let root = relative_path(base, value)?;
            map.insert(alias.into(), *libraries.get(root.as_path())?);
            Some(())
        };
        for (alias, rel) in &self.explicit {
            link(alias, dir, rel);
        }
        if !self.inherited.is_empty() {
            // Resolve every inherited name from the same nearest-first manifest
            // chain. A nearer declaration still wins even if its path is absent.
            let ancestors: Vec<_> = dir
                .ancestors()
                .filter_map(|up| packages.get_key_value(up))
                .filter(|(_, package)| !package.workspace.is_empty())
                .collect();
            for alias in &self.inherited {
                if let Some((root, path)) = ancestors
                    .iter()
                    .find_map(|(up, package)| Some((*up, package.workspace.get(alias)?)))
                {
                    link(alias, root, path);
                }
            }
        }
        if !self.name.is_empty()
            && let Some(&library) = libraries.get(dir)
        {
            map.insert(self.name.clone(), library);
        }
        map
    }
    fn dependency(&mut self, key: &str, value: &str, is_workspace: bool) {
        let value = value.trim();
        if value == "true"
            && let Some(alias) = key.strip_suffix(".workspace")
        {
            self.inherited.push(alias.replace('-', "_"));
            return;
        }
        if !value.starts_with('{') {
            return; // A registry version supplies no local path.
        }
        let fields = DependencyFields::read(value);
        match (fields.path.and_then(atom), fields.workspace, is_workspace) {
            (Some(path), _, true) => {
                self.workspace.insert(key.replace('-', "_"), path.into());
            }
            (Some(path), _, false) => {
                self.explicit.insert(key.replace('-', "_"), path.into());
            }
            (None, Some("true"), false) => self
                .inherited
                .push(key.trim_end_matches(".workspace").replace('-', "_")),
            _ => {}
        }
    }
}

pub(super) enum ManifestLine<'a> {
    Section(&'a str),
    Entry(&'a str, &'a str),
    Other,
}
pub(super) fn manifest_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(|line| line[..unquoted(line, b'#').unwrap_or(line.len())].trim())
}

impl<'a> ManifestLine<'a> {
    pub(super) fn parse(raw: &'a str) -> Self {
        if raw.starts_with('[') {
            Self::Section(raw)
        } else if let Some((key, value)) = entry(raw) {
            Self::Entry(key, value)
        } else {
            Self::Other
        }
    }
}
#[derive(Default)]
struct ManifestReader<'a> {
    package: Package<'a>,
    section: &'a str,
    open_list: Option<bool>,
}

fn parse(text: &str) -> Package<'_> {
    let mut reader = ManifestReader::default();
    for line in manifest_lines(text) {
        reader.line(line);
    }
    reader.package
}

impl<'a> ManifestReader<'a> {
    fn line(&mut self, raw: &'a str) {
        if let Some(member) = self.open_list {
            self.open_list = append_patterns(raw, member, &mut self.package);
            return;
        }
        match ManifestLine::parse(raw) {
            ManifestLine::Section(header) => {
                self.section = header.trim_matches(&['[', ']'][..]);
                self.package.workspace_root |= self.section == "workspace";
            }
            ManifestLine::Entry(key, value) => {
                self.open_list = self.package.setting(self.section, key, value)
            }
            ManifestLine::Other => {}
        }
    }
}
impl<'a> Package<'a> {
    fn setting(&mut self, section: &str, key: &str, value: &'a str) -> Option<bool> {
        match (section, key) {
            ("package", _) => self.package_field(key, value),
            ("workspace.package", "version") => {
                self.workspace_version = atom(value).map(str::to_owned)
            }
            ("workspace", "members" | "exclude") => {
                return append_patterns(value, key == "members", self);
            }
            ("dependencies" | "dev-dependencies" | "workspace.dependencies", _) => {
                self.dependency(key, value, section == "workspace.dependencies");
            }
            (target, "path") => self.targets.path(target, value),
            _ => {}
        }
        None
    }

    fn set_package_name(&mut self, value: &str) {
        let Some(name) = atom(value) else { return };
        self.name = name.replace('-', "_");
        self.package_name = name.into();
    }

    fn package_field(&mut self, key: &str, value: &str) {
        match key {
            "name" => self.set_package_name(value),
            "version" => self.version = atom(value).map(str::to_owned),
            "version.workspace" => self.inherited_version = value.trim() == "true",
            "workspace" => self.designated_workspace = atom(value).map(str::to_owned),
            switch if value.trim() == "false" => self.targets.disable(switch),
            _ => {}
        }
    }
}

fn append_patterns<'a>(value: &'a str, members: bool, p: &mut Package<'a>) -> Option<bool> {
    let out = if members {
        &mut p.members
    } else {
        &mut p.exclude
    };
    let list = value.trim().trim_start_matches('[');
    let close = unquoted(list, b']');
    for part in fields(&list[..close.unwrap_or(list.len())]) {
        if let Some(glob) = atom(part.trim()) {
            out.push(MemberPattern::parse(glob));
        }
    }
    close.is_none().then_some(members)
}

/// Workspace membership tests share the same immutable manifest for every package.
enum MemberPattern<'a> {
    Literal(&'a str),
    Glob(&'a str),
    Unsupported,
}

impl<'a> MemberPattern<'a> {
    fn parse(raw: &'a str) -> Self {
        if raw.contains(['?', '[', ']', '\\']) || raw.contains("**") {
            return Self::Unsupported;
        }
        let normalized = raw.trim_start_matches("./").trim_end_matches('/');
        if normalized.contains('*') {
            Self::Glob(normalized)
        } else {
            Self::Literal(normalized)
        }
    }

    fn matches(&self, relative: &str) -> Option<bool> {
        match self {
            Self::Literal(path) => Some(*path == relative),
            Self::Glob(pattern) => match_components(pattern, relative),
            Self::Unsupported => None,
        }
    }
}

fn match_components(pattern: &str, relative: &str) -> Option<bool> {
    let mut path = relative.split('/');
    let first_mismatch = pattern.split('/').find_map(|component| {
        match path
            .next()
            .map_or(Some(false), |value| match_component(component, value))
        {
            Some(true) => None,
            result => Some(result),
        }
    });
    first_mismatch.unwrap_or_else(|| Some(path.next().is_none()))
}

fn match_component(component: &str, value: &str) -> Option<bool> {
    let Some((left, right)) = component.split_once('*') else {
        return Some(component == value);
    };
    if right.contains('*') {
        return None;
    }
    Some(
        value
            .strip_prefix(left)
            .is_some_and(|tail| tail.ends_with(right)),
    )
}

fn prefix(packages: &HashMap<PathBuf, Package>, dir: &Path, p: &Package) -> Option<String> {
    if let Some(designated) = &p.designated_workspace {
        let (root, workspace) = packages.get_key_value(&relative_path(dir, designated)?)?;
        return p.label(root, Some(workspace));
    }
    let ancestor = dir
        .ancestors()
        .filter_map(|up| packages.get_key_value(up))
        .find(|(_, ws)| ws.workspace_root);
    let Some((root, workspace)) = ancestor else {
        return p.label(dir, None);
    };
    if root != dir && !workspace.includes(dir.strip_prefix(root).ok()?.to_str()?) {
        return None;
    }
    p.label(root, Some(workspace))
}
impl Package<'_> {
    fn label(&self, root: &Path, workspace: Option<&Package>) -> Option<String> {
        let version = workspace
            .filter(|_| self.inherited_version)
            .map_or(&self.version, |ws| &ws.workspace_version)
            .as_deref()?;
        if self.package_name.is_empty()
            || !self
                .package_name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            || !version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
        {
            return None;
        }
        Some(format!(
            "rust-analyzer {} rust-analyzer cargo {} {} ",
            root.display(),
            self.package_name,
            version
        ))
    }
}

// Each Cargo autodiscovery switch controls one conventional target family.
// Explicit targets are independent of all four switches.
const AUTO_TARGETS: &[(&str, &str)] = &[
    ("autobins", "src/bin/"),
    ("autotests", "tests/"),
    ("autoexamples", "examples/"),
    ("autobenches", "benches/"),
];

fn conventional_target(path: &str) -> Option<&'static str> {
    if path == "src/main.rs" {
        return Some("src/bin/");
    }
    AUTO_TARGETS.iter().find_map(|&(_, folder)| {
        let rest = path.strip_prefix(folder)?;
        let target = match rest.split_once('/') {
            Some((_, entry)) => entry == "main.rs",
            None => rest.ends_with(".rs"),
        };
        target.then_some(folder)
    })
}
struct Targets {
    lib: String,
    explicit: Vec<PathBuf>,
    disabled: Vec<&'static str>,
}
impl Default for Targets {
    fn default() -> Self {
        Self {
            lib: "src/lib.rs".into(),
            explicit: Vec::new(),
            disabled: Vec::new(),
        }
    }
}
impl Targets {
    fn path(&mut self, section: &str, value: &str) {
        let Some(path) = atom(value) else { return };
        match section {
            "lib" => self.lib = path.into(),
            "bin" | "test" | "example" | "bench" => {
                self.explicit.push(path.into());
            }
            _ => {}
        }
    }

    fn disable(&mut self, switch: &str) {
        if let Some(&(_, folder)) = AUTO_TARGETS.iter().find(|(name, _)| *name == switch) {
            self.disabled.push(folder);
        }
    }
    fn contains(&self, path: &Path) -> bool {
        path == Path::new(&self.lib)
            || self.explicit.iter().any(|p| p == path)
            || path
                .to_str()
                .and_then(conventional_target)
                .is_some_and(|family| !self.disabled.contains(&family))
    }
}
pub(super) struct Inputs {
    pub externs: Vec<Option<std::sync::Arc<ExternMap>>>,
    pub entry_files: std::collections::HashSet<usize>,
    pub target_prefixes: HashMap<usize, std::sync::Arc<str>>,
}
pub(super) fn inputs(units: &[Unit], paths: &HashMap<&str, usize>) -> Inputs {
    let packages: HashMap<PathBuf, Package> = units
        .iter()
        .filter(|u| {
            Path::new(u.path)
                .file_name()
                .is_some_and(|s| s == "Cargo.toml")
        })
        .map(|u| {
            (
                Path::new(u.path).parent().unwrap().to_owned(),
                parse(u.text),
            )
        })
        .collect();
    // Resolve each package's library target once. Dependency aliases can point
    // at the same package from many manifests and through workspace inheritance.
    let libraries: HashMap<_, _> = packages
        .iter()
        .filter_map(|(dir, package)| {
            let path = relative_path(dir, &package.targets.lib)?;
            let &file = paths.get(path.to_str()?)?;
            units[file].analysis.rust.as_ref()?;
            Some((dir.as_path(), file))
        })
        .collect();
    let mut contexts = HashMap::new();
    for (dir, p) in &packages {
        let map = p.externs(dir, &packages, &libraries);
        let label = prefix(&packages, dir, p).map(std::sync::Arc::<str>::from);
        contexts.insert(dir.as_path(), (p, std::sync::Arc::new(map), label));
    }
    let mut target_prefixes = HashMap::new();
    let mut entry_files = std::collections::HashSet::new();
    let mut active_packages = std::collections::HashSet::new();
    let mut owners = HashMap::new();
    let externs = units
        .iter()
        .enumerate()
        .map(|(id, u)| {
            u.analysis.rust.as_ref()?;
            let path = Path::new(u.path);
            let parent = path.parent()?;
            let (dir, (package, aliases, prefix)) =
                crate::domain::code::nearest_owner(&mut owners, parent, |dir| {
                    contexts
                        .get_key_value(dir)
                        .map(|(&key, value)| (key, value))
                })?;
            if active_packages.insert(dir) {
                entry_files.extend(aliases.values().copied());
            }
            if let Some(prefix) = prefix
                && package.targets.contains(path.strip_prefix(dir).ok()?)
            {
                entry_files.insert(id);
                target_prefixes.insert(id, prefix.clone());
            }
            Some(aliases.clone())
        })
        .collect();
    Inputs {
        externs,
        entry_files,
        target_prefixes,
    }
}
type ExternMap = HashMap<String, usize>;
