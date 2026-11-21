//! Stable descriptor forms from the frozen SCIP producers. Only emit a file
//! owner when the captured package and possible project roots agree.
use super::{Binding, Evidence, HashMap, HashSet, Language, Site, Unit};
use std::path::Path;
mod package;
use package::{Manifest, manifest};
#[cfg(test)]
mod tests;

fn descriptor(s: &str) -> String {
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_$+-".contains(&b))
    {
        s.to_owned()
    } else {
        format!("`{}`", s.replace('`', "``"))
    }
}

#[derive(Default)]
struct Owners<'a> {
    packages: HashMap<&'a Path, Manifest<'a>>,
    configs: HashSet<&'a Path>,
    python_config: bool,
}
impl<'a> Owners<'a> {
    fn read(units: &[Unit<'a>]) -> Self {
        let mut this = Self::default();
        for u in units {
            let p = Path::new(u.path);
            let Some(dir) = p.parent() else { continue };
            match p.file_name().and_then(|s| s.to_str()) {
                Some("package.json") => {
                    this.packages.insert(dir, manifest(u.text));
                }
                Some("tsconfig.json" | "jsconfig.json") => {
                    this.configs.insert(dir);
                }
                Some("pyrightconfig.json") if dir.as_os_str().is_empty() => {
                    this.python_config = true
                }
                Some("pyproject.toml") if dir.as_os_str().is_empty() => {
                    this.python_config |= u.text.contains("pyright")
                }
                _ => {}
            }
        }
        this
    }

    fn js(&self, path: &Path) -> Option<String> {
        let mut prefix = None;
        for dir in path.ancestors().skip(1) {
            match self.packages.get(dir) {
                Some(Manifest::Unknown) => return None,
                Some(Manifest::Named(name, version)) => {
                    let name = if name.is_empty() { "." } else { name };
                    let version = if version.is_empty() { "." } else { version };
                    prefix = Some((dir, name, version));
                    break;
                }
                _ => {}
            }
            // The configured program stops at this directory. The inferred
            // program can walk past it. With a named package above, or with
            // another subdirectory below, their possible owners differ.
            if self.configs.contains(dir)
                && (path.parent() != Some(dir)
                    || dir.ancestors().skip(1).any(|p| {
                        self.configs.contains(p)
                            || matches!(
                                self.packages.get(p),
                                Some(Manifest::Named(_, _) | Manifest::Unknown)
                            )
                    }))
            {
                return None;
            }
        }
        let (rel, name, version) = match prefix {
            Some((dir, name, version)) => (path.strip_prefix(dir).ok()?, name, version),
            None => (Path::new(path.file_name()?), ".", "."),
        };
        let mut owner = format!("scip-typescript  scip-typescript npm {name} {version} ");
        for c in rel.iter() {
            owner.push_str(&descriptor(c.to_str()?));
            owner.push('/');
        }
        Some(owner)
    }

    fn python(&self, path: &str) -> Option<String> {
        if self.python_config || path.starts_with("src/") {
            return None;
        }
        let stem = path.strip_suffix(".py")?;
        let module = stem.strip_suffix("/__init__").unwrap_or(stem);
        if module == "__init__"
            || !module.contains('/')
            || module.split('/').any(|s| s.contains('.'))
        {
            return None;
        }
        Some(format!(
            "scip-python  scip-python python workspace 0 {}/",
            descriptor(&module.replace('/', "."))
        ))
    }
}

pub(in crate::domain::code) fn public_labels(
    units: &[Unit<'_>],
    labels: &mut HashMap<Site, String>,
) {
    let owners = Owners::read(units);
    for (file, u) in units.iter().enumerate() {
        let Some(e) = &u.analysis.script else {
            continue;
        };
        let python = Language::for_path(u.path) == Some(Language::Python);
        let Some(owner) = (if python {
            owners.python(u.path)
        } else {
            owners.js(Path::new(u.path))
        }) else {
            continue;
        };
        let mut scopes = vec![(0, owner)];
        while let Some((scope, owner)) = scopes.pop() {
            for (name, binding) in &e.scopes[scope].bindings {
                let Binding::Local(id) = binding else {
                    continue;
                };
                let d = &u.analysis.declarations[*id];
                if &u.text[d.span.clone()] != name || python && name == "_" {
                    continue;
                }
                if python && scope == 0 && super::super::indentation(u.text, d.span.start) != 0 {
                    continue;
                }
                if d.kind == "parameter" {
                    let idx = e.destructured_parameters.partition_point(|r| r.end <= *id);
                    if e.destructured_parameters
                        .get(idx)
                        .is_some_and(|r| r.contains(id))
                    {
                        continue;
                    }
                }
                let desc = descriptor(name);
                let suffix = match d.kind {
                    "parameter" => format!("({desc})"),
                    "function_definition" | "function_declaration" => format!("{desc}()."),
                    "class_definition"
                    | "class_declaration"
                    | "interface_declaration"
                    | "type_alias_declaration"
                    | "enum_declaration" => format!("{desc}#"),
                    "variable_declarator" if scope == 0 => format!("{desc}."),
                    "local_binding" if python && (scope == 0 || e.scopes[scope].shape) => {
                        format!("{desc}.")
                    }
                    _ => continue,
                };
                // Function-local bindings and nested declarations can receive
                // exporter-local counters. A known signature owns only its parameters.
                if scope != 0 && !e.scopes[scope].shape && d.kind != "parameter" {
                    continue;
                }
                let label = format!("{owner}{suffix}");
                if let Some(child) = child_scope(e, *id, d.kind) {
                    scopes.push((child, label.clone()));
                }
                labels.insert(Site { file, local: *id }, label);
            }
        }
    }
}

fn child_scope(e: &Evidence, id: usize, kind: &str) -> Option<usize> {
    match kind {
        "class_definition" => e.shapes.get(&id).copied(),
        "function_definition" | "function_declaration" => e.functions.get(&id).copied(),
        _ => None,
    }
}
