//! Local manifest evidence only. Dependencies without a captured path declaration
//! cannot introduce a Rust crate into name lookup.
use super::{HashMap, Path, PathBuf, Unit};
use std::path::Component;
#[derive(Default)]
struct Package {
    lib: String,
    name: String,
    package_name: String,
    version: Option<String>,
    inherited_version: bool,
    workspace_version: Option<String>,
    workspace_root: bool,
    designated_workspace: Option<String>,
    members: Vec<String>,
    exclude: Vec<String>,
    explicit_targets: Vec<String>,
    custom_auto: bool,
    explicit: HashMap<String, String>,
    inherited: Vec<String>,
    workspace: HashMap<String, String>,
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
fn property<'a>(table: &'a str, key: &str) -> Option<&'a str> {
    table
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .find_map(|part| {
            let (k, v) = part.split_once('=')?;
            (k.trim() == key).then_some(v.trim())
        })
}
pub(super) fn join(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut p = dir.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !p.pop() {
                    return None;
                }
            }
            Component::Normal(n) => p.push(n),
            _ => return None,
        }
    }
    Some(p)
}
fn parse(text: &str) -> Package {
    let mut section = "";
    let mut p = Package {
        lib: "src/lib.rs".into(),
        ..Default::default()
    };
    let mut list = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(member) = list {
            list = append_patterns(line, member, &mut p);
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(&['[', ']'][..]);
            if section == "workspace" {
                p.workspace_root = true;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let alias = key.replace('-', "_");
        match (section, key) {
            ("package", "name") => {
                if let Some(name) = atom(value) {
                    p.name = name.replace('-', "_");
                    p.package_name = name.into();
                }
            }
            ("package", "version") => p.version = atom(value).map(str::to_owned),
            ("package", "version.workspace") => p.inherited_version = value.trim() == "true",
            ("workspace.package", "version") => {
                p.workspace_version = atom(value).map(str::to_owned)
            }
            ("package", "workspace") => p.designated_workspace = atom(value).map(str::to_owned),
            ("workspace", "members" | "exclude") => {
                list = append_patterns(value, key == "members", &mut p)
            }
            ("bin" | "test" | "example" | "bench", "path") => {
                if let Some(path) = atom(value) {
                    p.explicit_targets.push(path.into());
                }
            }
            ("package", "autotests" | "autobins" | "autoexamples" | "autobenches")
                if value.trim() == "false" =>
            {
                p.custom_auto = true
            }
            ("lib", "path") => {
                if let Some(path) = atom(value) {
                    p.lib = path.into();
                }
            }
            ("dependencies" | "dev-dependencies" | "workspace.dependencies", _) => {
                let is_workspace = section == "workspace.dependencies";
                if let Some(path) = property(value, "path").and_then(atom) {
                    if is_workspace {
                        p.workspace.insert(alias, path.into());
                    } else {
                        p.explicit.insert(alias, path.into());
                    }
                } else if (!is_workspace && property(value, "workspace") == Some("true"))
                    || key.ends_with(".workspace") && value.trim() == "true"
                {
                    p.inherited
                        .push(alias.trim_end_matches(".workspace").into());
                }
            }
            _ => {}
        }
    }
    p
}

fn append_patterns(value: &str, members: bool, p: &mut Package) -> Option<bool> {
    let out = if members {
        &mut p.members
    } else {
        &mut p.exclude
    };
    for part in value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
    {
        if let Some(glob) = atom(part.trim()) {
            out.push(glob.to_owned());
        }
    }
    (!value.contains(']')).then_some(members)
}

fn pattern(pattern: &str, relative: &str) -> Option<bool> {
    if pattern.contains(['?', '[', ']', '\\']) || pattern.contains("**") {
        return None;
    }
    let mut glob = pattern
        .trim_start_matches("./")
        .trim_end_matches('/')
        .split('/');
    let mut path = relative.split('/');
    loop {
        match (glob.next(), path.next()) {
            (None, None) => return Some(true),
            (Some(component), Some(value)) => {
                if component == "*" {
                    continue;
                }
                if let Some((left, right)) = component.split_once('*') {
                    if right.contains('*') {
                        return None;
                    }
                    if !(value.len() >= left.len() + right.len()
                        && value.starts_with(left)
                        && value.ends_with(right))
                    {
                        return Some(false);
                    }
                } else if component != value {
                    return Some(false);
                }
            }
            _ => return Some(false),
        }
    }
}

fn prefix(packages: &HashMap<PathBuf, Package>, dir: &Path, p: &Package) -> Option<String> {
    let designated = p.designated_workspace.as_ref().and_then(|d| join(dir, d));
    let ancestor = if p.designated_workspace.is_some() {
        Some(designated.as_deref()?)
    } else {
        dir.ancestors()
            .find(|d| packages.get(*d).is_some_and(|p| p.workspace_root))
    };
    let (workspace, version) = if let Some(root) = ancestor {
        let ws = packages.get(root)?;
        if root != dir && p.designated_workspace.is_none() {
            let rel = dir.strip_prefix(root).ok()?.to_str()?;
            if ws
                .exclude
                .iter()
                .any(|rule| pattern(rule, rel) != Some(false))
                || !ws
                    .members
                    .iter()
                    .any(|rule| pattern(rule, rel) == Some(true))
            {
                return None;
            }
        }
        (
            root,
            if p.inherited_version {
                ws.workspace_version.as_deref()?
            } else {
                p.version.as_deref()?
            },
        )
    } else {
        (dir, p.version.as_deref()?)
    };
    if p.package_name.is_empty()
        || !p
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
        workspace.display(),
        p.package_name,
        version
    ))
}

fn target(p: &Package, path: &Path) -> bool {
    if path == Path::new(&p.lib) || p.explicit_targets.iter().any(|s| Path::new(s) == path) {
        return true;
    }
    if p.custom_auto {
        return false;
    }
    let Some(s) = path.to_str() else {
        return false;
    };
    if s == "src/main.rs" {
        return true;
    }
    ["tests/", "benches/", "examples/", "src/bin/"]
        .iter()
        .filter_map(|base| s.strip_prefix(base))
        .any(|f| {
            !f.contains('/') && f.ends_with(".rs")
                || f.split_once('/').is_some_and(|(_, rest)| rest == "main.rs")
        })
}
pub(super) struct Inputs {
    pub externs: Vec<Option<std::sync::Arc<ExternMap>>>,
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
    let mut aliases = HashMap::new();
    for (dir, p) in &packages {
        let mut map = HashMap::new();
        let mut link = |alias: &str, base: &Path, value: &str| {
            let root = join(base, value)?;
            let target = packages.get(&root)?;
            let lib = join(&root, &target.lib)?;
            let f = *paths.get(lib.to_str()?)?;
            if units[f].analysis.rust.is_some() {
                map.insert(alias.into(), f);
            }
            Some(())
        };
        for (alias, rel) in &p.explicit {
            link(alias, dir, rel);
        }
        for alias in &p.inherited {
            if let Some((root, rel)) = dir
                .ancestors()
                .find_map(|up| packages.get(up)?.workspace.get(alias).map(|p| (up, p)))
            {
                link(alias, root, rel);
            }
        }
        if !p.name.is_empty() {
            link(&p.name, dir, ".");
        }
        aliases.insert(dir.as_path(), std::sync::Arc::new(map));
    }
    let prefixes: HashMap<_, _> = packages
        .iter()
        .filter_map(|(dir, p)| {
            Some((
                dir.as_path(),
                std::sync::Arc::<str>::from(prefix(&packages, dir, p)?),
            ))
        })
        .collect();
    let mut target_prefixes = HashMap::new();
    let externs = units
        .iter()
        .enumerate()
        .map(|(id, u)| {
            u.analysis.rust.as_ref()?;
            let path = Path::new(u.path);
            let dir = path
                .parent()?
                .ancestors()
                .find(|d| packages.contains_key(*d))?;
            if let Some(prefix) = prefixes.get(dir)
                && target(&packages[dir], path.strip_prefix(dir).ok()?)
            {
                target_prefixes.insert(id, prefix.clone());
            }
            aliases.get(dir).cloned()
        })
        .collect();
    Inputs {
        externs,
        target_prefixes,
    }
}
type ExternMap = HashMap<String, usize>;
