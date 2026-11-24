//! Persist the inspected include paths in the first compilation database's
//! evidence row. Missing earlier search candidates are dependencies as well.
use super::{
    Language, ObservedPath, Previous,
    code::{CProject, CReceipt},
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io,
};
pub(super) fn is_c(path: &str) -> bool {
    matches!(Language::for_path(path), Some(Language::C | Language::Cpp))
}
pub(super) fn database(path: &str) -> bool {
    path == "compile_commands.json" || path.ends_with("/compile_commands.json")
}
pub(super) struct Refresh {
    pub catalog: Option<usize>,
    dependencies: HashMap<String, Vec<String>>,
    pub receipts: Vec<CReceipt>,
    prune: bool,
    refresh_all: bool,
    affected: HashSet<String>,
}
impl Refresh {
    pub fn rewrite_catalog(&self) -> bool {
        self.catalog.is_some() && (self.prune || self.refresh_all)
    }
    pub fn invalidates_graph(&self) -> bool {
        self.refresh_all || !self.affected.is_empty()
    }
    pub fn reparse(&self, path: &str, prior: Option<usize>) -> bool {
        self.refresh_all
            || self.affected.contains(path)
            || (self.catalog.is_some() && prior.is_none())
    }
    pub fn merge(
        &mut self,
        current: (HashMap<String, Vec<String>>, Vec<CReceipt>),
        files: &[ObservedPath],
    ) -> HashMap<String, Vec<String>> {
        if self.refresh_all {
            self.dependencies.clear();
        }
        self.dependencies
            .retain(|p, _| files.binary_search_by(|f| f.path.as_str().cmp(p)).is_ok());
        let (paths, new_receipts) = current;
        // Only a reparsed translation unit can replace its own observations.
        // Another unit may have observed the same directory after a mutation.
        self.dependencies.retain(|tu, _| !paths.contains_key(tu));
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
    pub fn plan(
        previous: Option<&dyn Previous>,
        files: &[ObservedPath],
        reuse: &[Option<usize>],
    ) -> io::Result<Self> {
        let catalog = files.iter().position(|f| database(&f.path));
        let old = previous.map_or(&[][..], Previous::entries);
        let old_catalog = old.iter().position(|f| database(f.path()));
        let mut plan = Self {
            catalog,
            dependencies: HashMap::new(),
            receipts: Vec::new(),
            prune: false,
            refresh_all: false,
            affected: HashSet::new(),
        };
        if catalog.is_none() && old_catalog.is_none() {
            return Ok(plan);
        }
        let changed: HashSet<_> = files
            .iter()
            .zip(reuse)
            .filter(|(_, id)| id.is_none())
            .map(|(f, _)| f.path.as_str())
            .chain(
                old.iter()
                    .map(|f| f.path())
                    .filter(|p| files.binary_search_by(|f| f.path.as_str().cmp(p)).is_err()),
            )
            .collect();
        if changed.iter().any(|p| database(p)) {
            plan.refresh_all = true;
            return Ok(plan);
        }
        if let Some((p, id)) = previous.zip(old_catalog) {
            if let Some((deps, receipts)) =
                CProject::take_dependencies(p.local(id, true, false)?.code)
            {
                let stale: HashSet<_> = receipts
                    .iter()
                    .filter(|r| !r.valid())
                    .map(|r| r.key())
                    .collect();
                for (tu, includes) in &deps {
                    if files.binary_search_by(|f| f.path.as_str().cmp(tu)).is_err() {
                        plan.prune = true;
                    }
                    if includes.iter().any(|p| stale.contains(p.as_str())) {
                        plan.affected.insert(tu.clone());
                    }

                    if includes.iter().any(|p| changed.contains(p.as_str())) {
                        plan.affected.insert(tu.clone());
                    }
                }
                plan.receipts = receipts;
                plan.dependencies = deps;
            } else {
                plan.refresh_all = true;
            }
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
            let mut refresh = Refresh {
                catalog: Some(0),
                dependencies: HashMap::from([
                    ("a.c".into(), vec![key.clone()]),
                    ("b.c".into(), vec![key.clone()]),
                ]),
                receipts: vec![old.clone()],
                prune: false,
                refresh_all: false,
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
            let mut current = HashMap::from([("a.c".into(), vec![key.clone()])]);
            if !reuse_second {
                current.insert("b.c".into(), vec![key.clone()]);
            }
            refresh.merge((current, new_receipts), &files);
            assert_eq!(refresh.receipts.iter().all(CReceipt::valid), all_valid);
            assert_eq!(refresh.receipts.len(), if all_valid { 1 } else { 2 });
        }
    }
}
