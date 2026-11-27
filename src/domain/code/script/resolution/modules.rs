//! Resolve imports against the file set and project configuration of one snapshot.
use super::super::projects::Projects;
use super::*;
use std::{
    cell::OnceCell,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(super) struct ModuleLinks {
    pub(super) imports: Vec<Option<usize>>,
    pub(super) stars: Vec<usize>,
    pub(super) python: bool,
}

pub(super) fn link(units: &[Unit], paths: &HashMap<&str, usize>) -> Vec<ModuleLinks> {
    let projects = OnceCell::new();
    let mut owners = HashMap::new();
    let mut directories = HashMap::<_, HashMap<&str, Option<usize>>>::new();
    units
        .iter()
        .map(|unit| {
            let Some(e) = &unit.analysis.script else {
                return ModuleLinks::default();
            };
            if e.imports.is_empty() && e.stars.is_empty() {
                return ModuleLinks::default();
            }
            let (Some(lang), Some(parent)) =
                (Language::for_path(unit.path), Path::new(unit.path).parent())
            else {
                return ModuleLinks {
                    imports: vec![None; e.imports.len()],
                    ..Default::default()
                };
            };
            let names = directories.entry((parent, lang)).or_default();
            let mut lookup = |name| {
                *names.entry(name).or_insert_with(|| {
                    module_in(parent, lang, name, paths).or_else(|| {
                        if lang != Language::Js {
                            return None;
                        }
                        let projects = projects.get_or_init(|| Projects::read(units));
                        crate::domain::code::nearest_owner(&mut owners, parent, |dir| {
                            projects.at(dir)
                        })?
                        .lookup(name, paths)
                    })
                })
            };
            ModuleLinks {
                imports: e
                    .imports
                    .iter()
                    .map(|imp| lookup(imp.module.as_str()))
                    .collect(),
                stars: e.stars.iter().filter_map(|m| lookup(m.as_str())).collect(),
                python: lang == Language::Python,
            }
        })
        .collect()
}

pub(in crate::domain::code::script) fn module_in(
    parent: &Path,
    lang: Language,
    module: &str,
    paths: &HashMap<&str, usize>,
) -> Option<usize> {
    if lang == Language::Python {
        return python_module(parent, module, paths);
    }
    if !(module.starts_with("./") || module.starts_with("../")) {
        return None;
    }
    let base = crate::domain::code::relative_path(parent, module)?;
    probe_js_replacement(&base, paths).or_else(|| probe_suffixes(base, JS_PATHS, paths))
}

fn probe_js_replacement(base: &Path, paths: &HashMap<&str, usize>) -> Option<usize> {
    let replacements: &[&str] = match base.extension().and_then(|v| v.to_str()) {
        Some("js") => &["ts", "tsx", "d.ts", "js", "jsx"],
        Some("mjs") => &["mts", "d.mts", "mjs"],
        Some("cjs") => &["cts", "d.cts", "cjs"],
        _ => &[],
    };
    if replacements.is_empty() {
        return None;
    }
    let mut stem = base.to_owned();
    stem.set_extension("");
    let mut prefix = stem.into_os_string();
    prefix.push(".");
    probe_suffixes(prefix.into(), replacements, paths)
}

const JS_PATHS: &[&str] = &[
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

fn probe_suffixes(base: PathBuf, suffixes: &[&str], paths: &HashMap<&str, usize>) -> Option<usize> {
    let mut candidate = base.into_os_string().into_string().ok()?;
    let len = candidate.len();
    for suffix in suffixes {
        candidate.truncate(len);
        candidate.push_str(suffix);
        if let Some(id) = paths.get(candidate.as_str()) {
            return Some(*id);
        }
    }
    None
}

fn python_module(parent: &Path, module: &str, paths: &HashMap<&str, usize>) -> Option<usize> {
    let dots = module.bytes().take_while(|&c| c == b'.').count();
    let mut base = python_base(parent, dots)?;
    for part in module[dots..].split('.').filter(|p| !p.is_empty()) {
        base.push(part);
    }
    const PYTHON: &[&str] = &[".pyi", ".py", "/__init__.pyi", "/__init__.py"];
    if dots > 0 || is_python_package(parent, paths) {
        return probe_suffixes(base, PYTHON, paths);
    }
    // Directly launched scripts include their own directory on the import path.
    // Leave conflicting roots unresolved without an explicit launch context.
    let script = probe_suffixes(parent.join(&base), PYTHON, paths);
    let root = probe_suffixes(base, PYTHON, paths);
    match (root, script) {
        (Some(a), Some(b)) if a != b => None,
        (a, b) => a.or(b),
    }
}

fn python_base(parent: &Path, dots: usize) -> Option<PathBuf> {
    if dots == 0 {
        return Some(PathBuf::new());
    }
    let mut base = parent.to_owned();
    (1..dots).all(|_| base.pop()).then_some(base)
}

fn is_python_package(parent: &Path, paths: &HashMap<&str, usize>) -> bool {
    parent.as_os_str().is_empty()
        || ["__init__.py", "__init__.pyi"].iter().any(|name| {
            parent
                .join(name)
                .to_str()
                .is_some_and(|p| paths.contains_key(p))
        })
}
