//! Resolve shell references against local declarations and sourced scripts.
use super::{Declaration, Evidence, Owner, path_for};
use crate::domain::code::{ForwardNames, Site, Unit};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

fn evidence<'a>(u: &Unit<'a>) -> Option<&'a Evidence> {
    match u.analysis.direct_evidence.as_deref()? {
        crate::domain::code::direct::Evidence::Shell(s) => Some(s),
        _ => None,
    }
}
pub(in crate::domain::code) type SourceEdges = HashMap<usize, Vec<(usize, Owner, usize)>>;
#[derive(Clone, Copy)]
struct Export {
    available_at: usize,
    target: Site,
}
type Exports<'a> = HashMap<(bool, &'a str), Export>;

struct Lookup<'a> {
    owner: Owner,
    region: usize,
    position: usize,
    key: (bool, &'a str),
}

fn local_exports<'a>(file: usize, e: &'a Evidence, declarations: &[Declaration]) -> Exports<'a> {
    [false, true].into_iter().flat_map(|function| {
        e.declarations.get(&(None, function)).into_iter().flatten().filter_map(move |(name, ids)| {
            let &local = ids.iter().rev().find(|id| !e.transient.contains(id))?;
            let export = Export { available_at: declarations[local].span.start, target: Site { file, local } };
            Some(((function, name.as_str()), export))
        })
    }).collect()
}

struct Libraries<'a, 's> {
    units: &'a [Unit<'s>],
    sources: &'a SourceEdges,
    visiting: HashSet<usize>,
    exports: HashMap<usize, Rc<Exports<'s>>>,
}

impl<'s> Libraries<'_, 's> {
    fn load(&mut self, file: usize) -> Option<Rc<Exports<'s>>> {
        if let Some(table) = self.exports.get(&file) {
            return Some(Rc::clone(table));
        }
        if self.visiting.len() >= 80 || !self.visiting.insert(file) {
            return None;
        }
        let table = self.collect(file);
        self.visiting.remove(&file);
        let table = table?;
        self.exports.insert(file, Rc::clone(&table));
        Some(table)
    }

    fn target(&self, file: usize, lookup: &Lookup<'_>, local: Option<Export>) -> Option<Site> {
        let imports = self.sources.get(&file).into_iter().flatten().rev();
        let imported = imports
            .filter(|(position, owner, _)| {
                *position < lookup.position && (owner.is_none() || *owner == lookup.owner)
            })
            .take_while(|(position, _, _)| local.is_none_or(|local| local.available_at <= *position))
            .find_map(|(_, _, target)| self.exports.get(target)?.get(&lookup.key))
            .copied();
        imported.or(local).map(|export| export.target)
    }

    fn collect(&mut self, file: usize) -> Option<Rc<Exports<'s>>> {
        let unit = &self.units[file];
        let mut table = local_exports(file, evidence(unit)?, &unit.analysis.declarations);
        let mut imports = self
            .sources
            .get(&file)
            .into_iter()
            .flatten()
            .filter(|(_, owner, _)| owner.is_none())
            .peekable();
        while let Some(&(pos, _, target)) = imports.next() {
            let imported = self.load(target)?;
            // A pure forwarding script shares its final imported targets.
            if table.is_empty() && imports.peek().is_none() {
                return Some(imported);
            }
            inherit_exports(&mut table, &imported, pos);
        }
        Some(Rc::new(table))
    }
}
fn inherit_exports<'a>(table: &mut Exports<'a>, imported: &Exports<'a>, position: usize) {
    for (&key, inherited) in imported {
        let next = Export { available_at: position, target: inherited.target };
        let current = table.entry(key).or_insert(next);
        if current.available_at < position {
            *current = next;
        }
    }
}

struct LocalBindings<'a> {
    declarations: &'a [Declaration],
    evidence: &'a Evidence,
    // Each binding list has a unique first declaration ID. Nested assignments
    // can be recorded inside-out, so check order once before binary search.
    ordered: HashMap<usize, bool>,
}

impl LocalBindings<'_> {
    fn preceding(&mut self, items: &[usize], position: usize, region: usize) -> Option<usize> {
        let &first = items.first()?;
        let start = |id: usize| self.declarations[id].span.start;
        let e = self.evidence;
        let visible = |id: usize| {
            let target = e.decl_scope[id];
            let mut scope = region;
            while scope != target && scope != 0 {
                scope = e.parent[scope];
            }
            scope == target
        };
        let ordered = items.len() == 1
            || *self
                .ordered
                .entry(first)
                .or_insert_with(|| items.is_sorted_by_key(|&id| start(id)));
        if ordered {
            let end = items.partition_point(|&id| start(id) <= position);
            let mut candidates = items[..end]
                .iter()
                .rev()
                .copied()
                .filter(|id| !e.transient.contains(id));
            let fallback = candidates.next()?;
            return std::iter::once(fallback)
                .chain(candidates)
                .find(|&id| visible(id))
                .or(Some(fallback));
        }
        let mut candidates = items
            .iter()
            .copied()
            .filter(|&id| start(id) <= position && !e.transient.contains(&id));
        candidates
            .clone()
            .filter(|&id| visible(id))
            .max_by_key(|&id| start(id))
            .or_else(|| candidates.next_back())
    }

    fn lookup(&mut self, request: &Lookup<'_>) -> Option<usize> {
        let (function, name) = request.key;
        let Lookup { owner, region, position, .. } = *request;
        let names = &self.evidence.declarations;
        if let Some(items) = owner
            .and_then(|id| names.get(&(Some(id), function)))
            .and_then(|m| m.get(name))
            && let Some(local) = self.preceding(items, position, region)
        {
            return Some(local);
        }
        let items = names.get(&(None, function))?.get(name)?;
        self.preceding(items, position, region).or_else(|| {
            if function && owner.is_some() {
                items.first().copied()
            } else {
                None
            }
        })
    }
}

fn links<'a>(project: &'a crate::domain::code::ProjectView) -> &'a SourceEdges {
    project.shell.get_or_init(|| {
        let units = project.units;
        let paths = project.paths();
        units
            .iter()
            .enumerate()
            .filter_map(|(f, u)| {
                let e = evidence(u)?;
                if e.sources.is_empty() {
                    return None;
                }
                Some((
                    f,
                    e.sources
                        .iter()
                        .filter_map(|(pos, owner, p)| {
                            Some((*pos, *owner, path_for(u.path, p, paths)?))
                        })
                        .collect(),
                ))
            })
            .collect()
    })
}
fn resolve(files: &[usize], units: &[Unit], sources: &SourceEdges, names: &mut ForwardNames) {
    let mut libraries = Libraries {
        units,
        sources,
        visiting: HashSet::new(),
        exports: HashMap::new(),
    };
    for (_, _, f) in files.iter().filter_map(|f| sources.get(f)).flatten() {
        libraries.load(*f);
    }
    let active = files.iter().filter_map(|&file| {
        let unit = &units[file];
        Some((file, unit, evidence(unit)?))
    });
    for (file, unit, evidence) in active {
        libraries.bind_file(file, (unit, evidence), names);
    }
}

impl Libraries<'_, '_> {
    fn bind_file(
        &self,
        file: usize,
        (unit, evidence): (&Unit, &Evidence),
        names: &mut ForwardNames,
    ) {
        let mut locals = LocalBindings {
            declarations: &unit.analysis.declarations,
            evidence,
            ordered: HashMap::new(),
        };
        for (id, key, region, function) in &evidence.uses {
            let call = &unit.analysis.calls[*id];
            let request = Lookup {
                owner: call.owner,
                region: *region,
                position: call.span.start,
                key: (*function, key),
            };
            let local = locals.lookup(&request).map(|local| Export {
                available_at: unit.analysis.declarations[local].span.start,
                target: Site { file, local },
            });
            let Some(target) = self.target(file, &request, local) else {
                continue;
            };
            names.bind_one(Site { file, local: *id }, target);
        }
    }
}

pub(in crate::domain::code) struct ShellResolver;
impl crate::domain::code::project_resolver::ProjectResolver for ShellResolver {
    fn refine(
        &self,
        project: &crate::domain::code::ProjectView,
        prior: &[Option<usize>],
        keep: &mut [bool],
        _: &mut dyn FnMut(usize) -> std::io::Result<crate::domain::code::Analysis>,
    ) -> std::io::Result<()> {
        crate::domain::code::project_resolver::retain_import_clients(
            prior,
            keep,
            links(project)
                .iter()
                .flat_map(|(&file, imps)| imps.iter().map(move |&(_, _, target)| (file, target))),
            project
                .units
                .iter()
                .enumerate()
                .filter_map(|(file, u)| evidence(u).map(|_| file)),
        );
        Ok(())
    }
    fn reads(&self, _: &str, lang: Option<crate::domain::code::Language>) -> bool {
        use crate::domain::code::Language;
        lang == Some(Language::Shell)
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
