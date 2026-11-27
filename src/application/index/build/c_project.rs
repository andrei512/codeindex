//! Persist the inspected include paths in the first compilation database's
//! evidence row. Missing earlier search candidates are dependencies as well.
use super::{
    ObservedPath, PathOrder, Previous,
    code::{CProject, CReceipt},
};
pub(super) use crate::domain::code::compilation_database as database;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io,
};
pub(super) struct Refresh {
    pub catalog: Option<usize>,
    dependencies: HashMap<String, Vec<String>>,
    pub receipts: Vec<CReceipt>,
    prune: bool,
    refresh_all: bool,
    catalog_changed: bool,
    commands: Option<HashMap<String, Vec<String>>>,
    affected: HashSet<String>,
}
impl Refresh {
    pub fn prepare<'a>(
        &self,
        root: &std::path::Path,
        files: &'a [ObservedPath],
        sources: &'a [Option<super::Source>],
        backing: Option<&'a super::source::Mapping>,
    ) -> Option<CProject<'a>> {
        self.catalog.is_some().then(move || {
            CProject::new(
                root,
                files
                    .iter()
                    .zip(sources)
                    .filter(|(o, _)| database(&o.path))
                    .filter_map(|(o, s)| Some((o.path.as_str(), s.as_ref()?.text(backing)))),
                move |path| {
                    let i = files.binary_search_by(|o| o.path.as_str().cmp(path)).ok()?;
                    Some((files[i].path.as_str(), sources[i].as_ref()?.text(backing)))
                },
            )
        })
    }
    pub fn save_observations(
        &mut self,
        project: Option<CProject>,
        files: &[ObservedPath],
        parsed: &mut [Option<super::Local>],
        evidence_reuse: &mut std::borrow::Cow<'_, [Option<usize>]>,
    ) {
        if let Some((id, project)) = self.catalog.zip(project) {
            let commands = self
                .commands
                .take()
                .unwrap_or_else(|| project.command_keys());
            let dependencies = self.merge(project.consulted(), files);
            CProject::put_dependencies(
                &mut parsed[id].as_mut().unwrap().code,
                dependencies,
                self.receipts.clone(),
                commands,
            );
            evidence_reuse.to_mut()[id] = None;
        }
    }
    /// Discovery deferred C rows until all databases were captured. Restore
    /// rows whose source, command set, includes and external receipts survived.
    pub fn refine(&mut self, project: Option<&CProject>) -> bool {
        if !self.catalog_changed {
            return false;
        }
        let (Some(project), Some(old)) = (project, self.commands.take()) else {
            return false;
        };
        let current = project.command_keys();
        for path in old.keys().chain(current.keys()) {
            if old.get(path) != current.get(path) {
                self.affected.insert(path.clone());
                self.dependencies.remove(path);
            }
        }
        self.commands = Some(current);
        self.refresh_all = false;
        true
    }
    pub fn observes(&self, path: &str) -> bool {
        CProject::recognizes(path)
    }

    pub fn rewrite_catalog(&self) -> bool {
        self.catalog.is_some() && (self.prune || self.refresh_all)
    }
    pub fn invalidates_graph(&self) -> bool {
        self.refresh_all || !self.affected.is_empty()
    }
    pub fn requires_analysis(&self, path: &str, prior: Option<usize>) -> bool {
        (self.refresh_all
            || self.affected.contains(path)
            || (self.catalog.is_some() && prior.is_none()))
            && self.observes(path)
    }
    pub fn merge(
        &mut self,
        current: (HashMap<String, Vec<String>>, Vec<CReceipt>),
        files: &[ObservedPath],
    ) -> HashMap<String, Vec<String>> {
        if self.refresh_all {
            self.dependencies.clear();
        }
        let (paths, new_receipts) = current;
        // Only a reparsed translation unit can replace its own observations.
        // Another unit may have observed the same directory after a mutation.
        self.dependencies.retain(|tu, _| {
            !paths.contains_key(tu)
                && (!self.prune || files.binary_search_by(|f| f.path.as_str().cmp(tu)).is_ok())
        });
        let reused_keys: HashSet<_> = self
            .dependencies
            .values()
            .flatten()
            .filter(|key| key.starts_with('/'))
            .map(String::as_str)
            .collect();
        let mut receipts: BTreeSet<_> = std::mem::take(&mut self.receipts)
            .into_iter()
            .filter(|receipt| reused_keys.contains(receipt.key()))
            .collect();
        // Fresh receipts were produced exclusively by this build's configured
        // units. Distinct stamps for one key must both reach publication: they
        // cannot both validate, so the publisher retries the mixed observation.
        receipts.extend(new_receipts);
        self.receipts = receipts.into_iter().collect();
        self.dependencies.extend(paths);
        std::mem::take(&mut self.dependencies)
    }
    fn mark_affected(
        &mut self,
        unit: &str,
        includes: &[String],
        (stale, changed): (&HashSet<&str>, &HashSet<&str>),
    ) {
        if includes.iter().any(|path| stale.contains(path.as_str()) || changed.contains(path.as_str())) {
            self.affected.insert(unit.to_owned());
        }
    }

    fn restore_observations(
        &mut self,
        code: crate::domain::code::Analysis,
        files: &[ObservedPath],
        changed: &HashSet<&str>,
    ) {
        let Some(inputs) = CProject::take_dependencies(code) else {
            self.refresh_all = true;
            return;
        };
        let stale: HashSet<_> = inputs
            .receipts
            .iter()
            .filter(|r| !r.valid())
            .map(|r| r.key())
            .collect();
        for (tu, includes) in &inputs.paths {
            self.prune |= files.binary_search_by(|f| f.path.as_str().cmp(tu)).is_err();
            self.mark_affected(tu, includes, (&stale, changed));
        }
        self.commands = inputs.commands;
        self.receipts = inputs.receipts;
        self.dependencies = inputs.paths;
    }

    pub fn plan(
        previous: Option<&dyn Previous>,
        files: &[ObservedPath],
        reuse: &[Option<usize>],
        paths: &PathOrder,
    ) -> io::Result<Self> {
        let old_catalog = previous.and_then(Previous::compilation_database);
        // A stable path order preserves both the first database ordinal and its absence.
        let catalog = if paths.stable() {
            old_catalog
        } else {
            files.iter().position(|f| database(&f.path))
        };
        let mut plan = Self {
            catalog,
            dependencies: HashMap::new(),
            receipts: Vec::new(),
            prune: false,
            refresh_all: false,
            commands: None,
            catalog_changed: false,
            affected: HashSet::new(),
        };
        let Some(previous) = previous else {
            plan.refresh_all = catalog.is_some();
            return Ok(plan);
        };
        let old = previous.entries();
        if catalog.is_none() && old_catalog.is_none() {
            return Ok(plan);
        }
        let mut changed: HashSet<_> = files
            .iter()
            .zip(reuse)
            .filter(|(_, id)| id.is_none())
            .map(|(f, _)| f.path.as_str())
            .collect();
        if let PathOrder::Changed(current) = paths {
            // The inventory merge identified every retained ordinal in order.
            let mut retained = current.iter().flatten().copied().peekable();
            for (id, file) in old.iter().enumerate() {
                if retained.next_if_eq(&id).is_none() {
                    changed.insert(file.path());
                }
            }
        }
        plan.catalog_changed = changed.iter().any(|p| database(p));
        plan.refresh_all = plan.catalog_changed;
        if let Some(id) = old_catalog {
            plan.restore_observations(previous.local(id, true, false)?.code, files, &changed);
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_sdk_observations_preserves_conflicts_until_publication() {
        let (old, new) = CReceipt::test_versions();
        let key = new.key().to_owned();
        assert!(!old.valid() && new.valid());
        for (reuse_second, new_receipts, all_valid) in [
            (false, vec![old.clone(), new.clone()], false),
            (true, vec![new.clone()], false),
            (false, vec![new.clone(), new.clone()], true),
        ] {
            check_receipt_merge(&old, &key, (reuse_second, new_receipts, all_valid));
        }
    }

    fn check_receipt_merge(
        old: &CReceipt,
        key: &str,
        (reuse_second, new_receipts, all_valid): (bool, Vec<CReceipt>, bool),
    ) {
        let mut refresh = Refresh {
            catalog: Some(0),
            dependencies: HashMap::from([
                ("a.c".into(), vec![key.to_owned()]),
                ("b.c".into(), vec![key.to_owned()]),
            ]),
            receipts: vec![old.clone()],
            prune: false,
            refresh_all: false,
            commands: None,
            catalog_changed: false,
            affected: HashSet::new(),
        };
        let files: Vec<_> = ["a.c", "b.c"]
            .into_iter()
            .map(|path| ObservedPath {
                path: path.into(),
                stamp: [0; 7],
                captured: None,
            })
            .collect();
        let mut current = HashMap::from([("a.c".into(), vec![key.to_owned()])]);
        if !reuse_second {
            current.insert("b.c".into(), vec![key.to_owned()]);
        }
        refresh.merge((current, new_receipts), &files);
        assert_eq!(refresh.receipts.iter().all(CReceipt::valid), all_valid);
        assert_eq!(refresh.receipts.len(), if all_valid { 1 } else { 2 });

    }
}
