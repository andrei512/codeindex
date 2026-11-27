//! Resolve public names and namespace forwarding within one linked snapshot.
use super::super::Forward;
use super::*;

pub(super) struct Cache<'a> {
    units: &'a [Unit<'a>],
    modules: &'a [ModuleLinks],
    paths: &'a HashMap<&'a str, usize>,
    submodules: HashMap<(usize, usize), Option<usize>>,
    names: HashMap<usize, HashMap<&'a str, Option<Site>>>,
    namespaces: HashMap<(usize, &'a str), Option<usize>>,
}
impl<'a> Cache<'a> {
    pub(super) fn new(
        units: &'a [Unit<'a>],
        modules: &'a [ModuleLinks],
        paths: &'a HashMap<&'a str, usize>,
    ) -> Self {
        Self {
            units,
            modules,
            paths,
            submodules: HashMap::new(),
            names: HashMap::new(),
            namespaces: HashMap::new(),
        }
    }
    pub(super) fn get(
        &mut self,
        file: usize,
        import: usize,
        member: Option<&'a str>,
    ) -> Option<Site> {
        let (units, modules) = (self.units, self.modules);
        let imp = &units[file].analysis.script.as_ref()?.imports[import];
        let (id, name) = match (&imp.name, member) {
            (Some(name), None) => (modules[file].imports[import]?, name.as_str()),
            (None, Some(name)) => (modules[file].imports[import]?, name),
            (Some(primary), Some(name)) => (self.member_module(file, import, primary)?, name),
            _ => return None,
        };
        *self
            .names
            .entry(id)
            .or_default()
            .entry(name)
            .or_insert_with(|| resolve_export(units, modules, id, name))
    }

    fn member_module(&mut self, file: usize, import: usize, primary: &'a str) -> Option<usize> {
        let remote = self.modules[file].imports[import];
        let python = self.modules[file].python;
        let submodule = python
            .then(|| self.python_member_module(file, import, primary))
            .flatten();
        submodule.or_else(|| self.namespace(remote?, primary))
    }

    fn python_member_module(&mut self, file: usize, import: usize, primary: &str) -> Option<usize> {
        *self.submodules.entry((file, import)).or_insert_with(|| {
            let remote = self.modules[file].imports[import];
            let script = remote.and_then(|id| self.units[id].analysis.script.as_ref());
            if script
                .is_some_and(|e| e.exports.contains_key(primary) || e.forward.contains_key(primary))
            {
                return None;
            }
            let unit = &self.units[file];
            let module = &unit.analysis.script.as_ref()?.imports[import].module;
            python_submodule(
                std::path::Path::new(unit.path).parent()?,
                module,
                primary,
                self.paths,
            )
        })
    }

    fn namespace(&mut self, file: usize, name: &'a str) -> Option<usize> {
        *self
            .namespaces
            .entry((file, name))
            .or_insert_with(|| namespace(self.units, self.modules, file, name))
    }
}
fn namespace<'a>(
    units: &'a [Unit],
    modules: &[ModuleLinks],
    mut file: usize,
    mut name: &'a str,
) -> Option<usize> {
    let mut seen = std::collections::HashSet::new();
    while seen.insert((file, name)) {
        let ev = units[file].analysis.script.as_ref()?;
        let (target, member) = namespace_target(ev, &modules[file], name)?;
        let Some(member) = member else {
            return Some(target);
        };
        (file, name) = (target, member);
    }
    None
}

fn namespace_target<'a>(
    evidence: &'a Evidence,
    links: &ModuleLinks,
    name: &str,
) -> Option<(usize, Option<&'a str>)> {
    let forward = evidence.forward.get(name)?;
    if let Forward::Namespace(import) = forward {
        return Some((links.imports[*import]?, None));
    }
    let import = forwarded_binding(evidence, forward)?.imported()?;
    Some((
        links.imports[import]?,
        evidence.imports[import].name.as_deref(),
    ))
}

fn export_binding(ev: &Evidence, name: &str) -> Option<Binding> {
    if let Some(&id) = ev.exports.get(name) {
        return Some(Binding::Local(id));
    }
    forwarded_binding(ev, ev.forward.get(name)?)
}

fn forwarded_binding(ev: &Evidence, forward: &Forward) -> Option<Binding> {
    match forward {
        Forward::Local(alias) => ev.scopes[0].bindings.get(alias).copied(),
        Forward::Import(id) => Some(Binding::Import(*id)),
        Forward::Namespace(_) => None,
    }
}

type ExportName<'a> = (usize, &'a str);

enum ExportRoute<'a> {
    Local(Site),
    Forward(Option<ExportName<'a>>),
    Stars(&'a [usize]),
    Missing,
}

impl<'a> ExportRoute<'a> {
    fn select(unit: &'a Unit<'a>, links: &'a ModuleLinks, file: usize, name: &str) -> Self {
        unit.analysis
            .script
            .as_deref()
            .map_or(Self::Missing, |evidence| {
                Self::from_binding(unit, evidence, links, (file, name))
            })
    }

    fn from_binding(
        unit: &Unit,
        evidence: &'a Evidence,
        links: &'a ModuleLinks,
        (file, name): (usize, &str),
    ) -> Self {
        let binding = export_binding(evidence, name);
        match binding {
            Some(Binding::Local(local)) => Self::local(unit, evidence, Site { file, local }),
            _ => Self::nonlocal(evidence, links, name, binding),
        }
    }

    fn local(unit: &Unit, evidence: &Evidence, site: Site) -> Self {
        local_export(unit, evidence, site.file, site.local).map_or(Self::Missing, Self::Local)
    }

    fn nonlocal(
        evidence: &'a Evidence,
        links: &'a ModuleLinks,
        name: &str,
        binding: Option<Binding>,
    ) -> Self {
        if let Some(imported) = binding.and_then(Binding::imported) {
            return Self::Forward(
                links.imports[imported].zip(evidence.imports[imported].name.as_deref()),
            );
        }
        if name == "default" || links.stars.is_empty() {
            return Self::Missing;
        }
        Self::Stars(&links.stars)
    }
}

fn local_export(unit: &Unit, evidence: &Evidence, file: usize, local: usize) -> Option<Site> {
    let declaration = &unit.analysis.declarations[local];
    let spelling = &unit.text[declaration.span.clone()];
    let scope = &evidence.scopes[0];
    matches!(scope.bindings.get(spelling), Some(Binding::Local(id)) if *id == local)
        .then_some(Site { file, local })
}

/// Walk only forwarding edges. A local export needs no worklist allocation.
fn resolve_export<'a>(
    units: &'a [Unit<'a>],
    modules: &'a [ModuleLinks],
    file: usize,
    name: &'a str,
) -> Option<Site> {
    let mut next = Some((file, name));
    let mut pending = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut found = None;
    while let Some((file, name)) = next.take().or_else(|| pending.pop()) {
        match ExportRoute::select(&units[file], &modules[file], file, name) {
            ExportRoute::Local(site) => {
                let previous = found.replace(site).unwrap_or(site);
                (previous == site).then_some(())?;
            }
            ExportRoute::Forward(route) => {
                let unseen = seen.insert((file, name));
                next = route.filter(|_| unseen);
            }
            ExportRoute::Stars(destinations) if seen.insert((file, name)) => {
                pending.extend(destinations.iter().map(|&target| (target, name)));
            }
            _ => {}
        }
    }
    found
}
