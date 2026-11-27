//! Stable descriptor forms from the frozen SCIP producers. Only emit a file
//! owner when the captured package and possible project roots agree.
use super::{Binding, Evidence, HashMap, HashSet, Language, Site, Unit};
use std::path::Path;
mod package;
use package::{Manifest, manifest};
#[cfg(test)]
mod tests;

fn push_escaped(label: &mut String, text: &str) {
    for part in text.split_inclusive('`') {
        label.push_str(part);
        if part.ends_with('`') {
            label.push('`');
        }
    }
}

fn push_descriptor(label: &mut String, text: &str) {
    if text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_$+-".contains(&b))
    {
        label.push_str(text);
    } else {
        label.push('`');
        push_escaped(label, text);
        label.push('`');
    }
}

#[derive(Default)]
struct Owners<'a> {
    packages: HashMap<&'a Path, Manifest<'a>>,
    configs: HashSet<&'a Path>,
    python_config: bool,
    last_js: Option<(&'a Path, Option<JsDirectory<'a>>)>,
}
impl<'a> Owners<'a> {
    fn read(units: &[Unit<'a>]) -> Self {
        let mut this = Self::default();
        for unit in units {
            this.observe(unit);
        }
        this
    }

    fn observe(&mut self, unit: &Unit<'a>) {
        // Only configuration files can change the producer's package or project root.
        if !unit.path.ends_with(".json") && !unit.path.ends_with(".toml") {
            return;
        }
        let path = Path::new(unit.path);
        let Some(directory) = path.parent() else {
            return;
        };
        let filename = path.file_name().and_then(|name| name.to_str());
        let root = directory.as_os_str().is_empty();
        match (filename, root) {
            (Some("pyrightconfig.json"), true) => self.python_config = true,
            (Some("pyproject.toml"), true) => self.python_config |= unit.text.contains("pyright"),
            (Some("package.json"), _) => {
                self.packages.insert(directory, manifest(unit.text));
            }
            (Some("tsconfig.json" | "jsconfig.json"), _) => {
                self.configs.insert(directory);
            }
            _ => {}
        }
    }

    fn publication<'u>(&mut self, unit: &'u Unit<'a>) -> Option<(Written<'u, 'a>, String)> {
        let evidence = unit.analysis.script.as_ref()?;
        let python = Language::for_path(unit.path) == Some(Language::Python);
        let owner = if python {
            self.python(unit.path)?
        } else {
            self.js(Path::new(unit.path))?
        };
        Some((
            Written {
                unit,
                evidence,
                python,
            },
            owner,
        ))
    }

    fn js_directory(&self, directory: &'a Path) -> Option<JsDirectory<'a>> {
        let mut configured = false;
        for dir in directory.ancestors() {
            match self.js_ancestor(directory, dir, configured) {
                JsAncestor::Package(package) => return package,
                JsAncestor::Configuration => configured = true,
                JsAncestor::Unknown => return None,
                JsAncestor::Outer => {}
            }
        }
        Some(JsDirectory::new(directory, ".", "."))
    }

    fn js_ancestor(
        &self,
        directory: &Path,
        ancestor: &'a Path,
        configured: bool,
    ) -> JsAncestor<'a> {
        match self.packages.get(ancestor) {
            Some(Manifest::Unknown) => return JsAncestor::Unknown,
            Some(Manifest::Named(name, version)) => {
                let public = (!configured).then(|| JsDirectory::new(ancestor, name, version));
                return JsAncestor::Package(public);
            }
            _ => {}
        }
        match (self.configs.contains(ancestor), directory == ancestor) {
            (true, true) => JsAncestor::Configuration,
            (true, false) => JsAncestor::Unknown,
            _ => JsAncestor::Outer,
        }
    }

    fn js(&mut self, path: &'a Path) -> Option<String> {
        path.file_name()?;
        let directory = path.parent()?;
        if self
            .last_js
            .as_ref()
            .is_none_or(|(last, _)| *last != directory)
        {
            self.last_js = Some((directory, self.js_directory(directory)));
        }
        let (_, scope) = self.last_js.as_ref()?;
        scope.as_ref()?.label(path)
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
        // Every accepted module has multiple components, so its dotted
        // descriptor always requires quotes.
        const PREFIX: &str = "scip-python  scip-python python workspace 0 `";
        let mut label = String::with_capacity(PREFIX.len() + module.len() + 2);
        label.push_str(PREFIX);
        let mut components = module.split('/');
        push_escaped(&mut label, components.next()?);
        for component in components {
            label.push('.');
            push_escaped(&mut label, component);
        }
        label.push_str("`/");
        Some(label)
    }
}

enum JsAncestor<'a> {
    Package(Option<JsDirectory<'a>>),
    Configuration,
    Unknown,
    Outer,
}

struct JsDirectory<'a> {
    root: &'a Path,
    prefix: String,
}

impl<'a> JsDirectory<'a> {
    fn new(root: &'a Path, name: &str, version: &str) -> Self {
        let name = if name.is_empty() { "." } else { name };
        let version = if version.is_empty() { "." } else { version };
        Self {
            root,
            prefix: format!("scip-typescript  scip-typescript npm {name} {version} "),
        }
    }

    fn label(&self, file: &Path) -> Option<String> {
        let relative = file.strip_prefix(self.root).ok()?;
        let mut label = String::with_capacity(self.prefix.len() + relative.as_os_str().len() + 3);
        label.push_str(&self.prefix);
        for component in relative {
            push_descriptor(&mut label, component.to_str()?);
            label.push('/');
        }
        Some(label)
    }
}

pub(in crate::domain::code) fn public_labels(
    files: &[usize],
    units: &[Unit<'_>],
    labels: &mut HashMap<Site, String>,
) {
    let mut owners = Owners::read(units);
    for &file in files {
        let Some((written, owner)) = owners.publication(&units[file]) else {
            continue;
        };
        written.publish(file, owner, labels);
    }
}

enum Descriptor {
    Parameter,
    Callable,
    Type,
    Term,
}

impl Descriptor {
    fn qualify(&self, owner: &str, name: &str) -> String {
        let (open, close) = match self {
            Self::Parameter => ("(", ")"),
            Self::Callable => ("", "()."),
            Self::Type => ("", "#"),
            Self::Term => ("", "."),
        };
        let mut label = String::with_capacity(owner.len() + open.len() + name.len() + close.len());
        label.push_str(owner);
        label.push_str(open);
        push_descriptor(&mut label, name);
        label.push_str(close);
        label
    }
}

struct Written<'a, 's> {
    unit: &'a Unit<'s>,
    evidence: &'a Evidence,
    python: bool,
}

impl Written<'_, '_> {
    fn publish(&self, file: usize, owner: String, labels: &mut HashMap<Site, String>) {
        // A pending scope owns its finished label while children borrow it as
        // their prefix. Publish the same allocation once its children are named.
        let mut scopes = vec![(0, owner, None)];
        while let Some((scope, owner, site)) = scopes.pop() {
            self.publish_scope((file, scope, &owner), &mut scopes, labels);
            if let Some(site) = site {
                labels.insert(site, owner);
            }
        }
    }

    fn publish_scope(
        &self,
        (file, scope, owner): (usize, usize, &str),
        pending: &mut Vec<(usize, String, Option<Site>)>,
        labels: &mut HashMap<Site, String>,
    ) {
        let bindings = self.evidence.scopes[scope]
            .bindings
            .iter()
            .filter_map(|(name, binding)| self.label(scope, owner, name, binding));
        for (id, child, label) in bindings {
            self.publish_binding((Site { file, local: id }, child, label), pending, labels);
        }
    }

    fn publish_binding(
        &self,
        (site, child, label): (Site, Option<usize>, String),
        pending: &mut Vec<(usize, String, Option<Site>)>,
        labels: &mut HashMap<Site, String>,
    ) {
        if let Some(child) = child.filter(|&i| !self.evidence.scopes[i].bindings.is_empty()) {
            pending.push((child, label, Some(site)));
        } else {
            labels.insert(site, label);
        }
    }

    fn public_scope(&self, scope: usize, kind: &str, id: usize) -> bool {
        if kind == "parameter" {
            let groups = &self.evidence.destructured_parameters;
            let at = groups.partition_point(|range| range.end <= id);
            return groups.get(at).is_none_or(|range| !range.contains(&id));
        }
        scope == 0 || self.evidence.scopes[scope].shape
    }

    fn label(
        &self,
        scope: usize,
        owner: &str,
        name: &str,
        binding: &Binding,
    ) -> Option<(usize, Option<usize>, String)> {
        let Binding::Local(id) = *binding else {
            return None;
        };
        let d = &self.unit.analysis.declarations[id];
        if &self.unit.text[d.span.clone()] != name {
            return None;
        }
        if self.python
            && (name == "_"
                || scope == 0 && super::super::indentation(self.unit.text, d.span.start) != 0)
        {
            return None;
        }
        let kind = d.kind;
        if !self.public_scope(scope, kind, id) {
            return None;
        }
        let (descriptor, child) = self.descriptor(scope, kind, id)?;
        Some((id, child, descriptor.qualify(owner, name)))
    }

    fn descriptor(
        &self,
        scope: usize,
        kind: &str,
        id: usize,
    ) -> Option<(Descriptor, Option<usize>)> {
        Some(match kind {
            "parameter" => (Descriptor::Parameter, None),
            "function_definition" | "function_declaration" => (
                Descriptor::Callable,
                self.evidence.functions.get(&id).copied(),
            ),
            "class_definition" => (Descriptor::Type, self.evidence.shapes.get(&id).copied()),
            "class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => (Descriptor::Type, None),
            _ => return self.term_descriptor(scope, kind).map(|d| (d, None)),
        })
    }
    fn term_descriptor(&self, scope: usize, kind: &str) -> Option<Descriptor> {
        match (kind, scope == 0, self.python) {
            ("variable_declarator", true, _) | ("local_binding", _, true) => Some(Descriptor::Term),
            _ => None,
        }
    }
}
