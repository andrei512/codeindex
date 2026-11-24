//! Decide whether an edit changes any bytes consumed by a query product.
use super::{
    Pin,
    checkpoint::{Checkpoint, Row},
};
use crate::adapters::repository::{self, BlobId, CommitTree, ObservedPath};
use crate::domain::{code::Language, markdown::is_markdown};
use std::{collections::HashMap, io, path::Path, sync::OnceLock};

pub(super) struct Plan {
    sha256: bool,
    // One producer per slot. A populated slot can contain a path-only classification.
    hashes: Vec<OnceLock<Option<BlobId>>>,
    verified: Vec<Option<usize>>,
    by_blob: OnceLock<HashMap<BlobId, Vec<usize>>>,
}

impl Plan {
    pub(super) fn new(count: usize, sha256: bool) -> Self {
        Self {
            sha256,
            by_blob: OnceLock::new(),
            hashes: (0..count).map(|_| OnceLock::new()).collect(),
            verified: vec![None; count],
        }
    }
    pub(super) fn can_forward(&self, pin: &Pin) -> bool {
        pin.checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.sha256 == self.sha256)
    }
    fn prior_hash(&self, pin: &Pin, id: usize) -> Option<Option<BlobId>> {
        let previous = pin.checkpoint.as_ref()?;
        (previous.sha256 == self.sha256).then_some(previous.rows[id].blob)
    }
    pub(super) fn verify_metadata_only(
        &mut self,
        root: &Path,
        obs: &mut [ObservedPath],
        pin: &Pin,
    ) -> io::Result<bool> {
        if pin
            .checkpoint
            .as_ref()
            .is_none_or(|c| c.sha256 != self.sha256)
            || obs.len() != pin.files.len()
            || obs.iter().zip(&pin.files).any(|(o, f)| o.path != f.path())
        {
            return Ok(false);
        }
        for (id, o) in obs.iter_mut().enumerate() {
            if o.stamp == pin.files[id].stamp() {
                continue;
            }
            let text = match o.captured.take() {
                Some(body) => body,
                None => repository::capture_observed(root, &o.path, o.stamp)?,
            };
            let hash = text
                .as_ref()
                .map(|s| repository::raw_blob_id(s.as_bytes(), self.sha256));
            self.hashes[id].set(hash).expect("one digest per input");
            if self.equal(pin, id, text.as_deref(), hash)? {
                self.verified[id] = Some(id);
            } else {
                o.captured = Some(text);
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn equal(
        &self,
        pin: &Pin,
        id: usize,
        text: Option<&str>,
        hash: Option<BlobId>,
    ) -> io::Result<bool> {
        if self.prior_hash(pin, id) != Some(hash) {
            return Ok(false);
        }
        match text {
            Some(text) => Ok(pin.source(id)? == text),
            None => Ok(pin.files[id].bytes.is_none()),
        }
    }
    pub(super) fn verified(&self, id: usize) -> bool {
        self.verified.get(id).is_some_and(Option::is_some)
    }
    pub(super) fn identify(
        &self,
        index: usize,
        o: &ObservedPath,
        text: Option<&str>,
        prior: Option<usize>,
        pin: Option<&Pin>,
    ) -> io::Result<(Option<usize>, Option<BlobId>)> {
        let unchanged = prior.or(self.verified[index]);
        let hash = *self.hashes[index].get_or_init(|| {
            unchanged
                .and_then(|id| self.prior_hash(pin?, id))
                .unwrap_or_else(|| text.map(|t| repository::raw_blob_id(t.as_bytes(), self.sha256)))
        });
        if unchanged.is_some() {
            return Ok((unchanged, hash));
        }
        if let Some(p) = pin {
            if let Ok(id) = p.files.binary_search_by(|e| e.path().cmp(&o.path)) {
                return Ok((self.equal(p, id, text, hash)?.then_some(id), hash));
            }
            if let Some(blob) = hash
                && let Some(cp) = p.checkpoint.as_ref()
                && cp.sha256 == self.sha256
            {
                let candidates = self.by_blob.get_or_init(|| {
                    let mut map = HashMap::<BlobId, Vec<usize>>::new();
                    for (id, row) in cp.rows.iter().enumerate() {
                        if let Some(blob) = row.blob {
                            map.entry(blob).or_default().push(id)
                        }
                    }
                    map
                });
                for &id in candidates.get(&blob).into_iter().flatten() {
                    let from = p.files[id].path();
                    if is_markdown(from) == is_markdown(&o.path)
                        && Language::for_path(from) == Language::for_path(&o.path)
                        && self.equal(p, id, text, hash)?
                    {
                        return Ok((Some(id), hash));
                    }
                }
            }
        }
        Ok((None, hash))
    }
    pub(super) fn finish(
        &self,
        root: &Path,
        commit: Option<&str>,
        obs: &[ObservedPath],
        old: Option<&Pin>,
    ) -> io::Result<Checkpoint> {
        let mut tree = None;
        let mut opened = false;
        let mut changed_paths = None;
        let old_commit = old.and_then(|p| p.indexed_commit.as_deref());
        let mut rows = Vec::with_capacity(obs.len());
        for (i, (o, cell)) in obs.iter().zip(&self.hashes).enumerate() {
            let prev = old.and_then(|pin| {
                let cp = pin.checkpoint.as_ref()?;
                let id = if pin.files.get(i).is_some_and(|entry| entry.path() == o.path) {
                    i
                } else {
                    pin.files.binary_search_by(|e| e.path().cmp(&o.path)).ok()?
                };
                (self.sha256 == cp.sha256).then_some(cp.rows[id])
            });
            let blob = match cell.get().copied() {
                Some(blob) => blob,
                None => {
                    prev.ok_or_else(|| {
                        io::Error::other(format!("missing checkpoint identity for row {i}"))
                    })?
                    .blob
                }
            };
            let committed = if blob.is_none() || commit.is_none() {
                false
            } else if let Some(p) = prev
                && p.blob == blob
                && old_commit == commit
            {
                p.committed
            } else {
                if !opened {
                    tree = CommitTree::open(root, commit.unwrap()).ok();
                    if let Some(previous) = old_commit.filter(|old| Some(*old) != commit)
                        && let Some(t) = tree.as_mut()
                    {
                        changed_paths = t.changed_paths(previous).ok();
                    }
                    opened = true
                }
                if let Some(new_id) = changed_paths
                    .as_ref()
                    .and_then(|changes| changes.get(&o.path))
                {
                    *new_id == blob
                } else if changed_paths.is_some()
                    && let Some(prior) = prev
                    && prior.blob == blob
                {
                    prior.committed
                } else {
                    tree.as_mut().and_then(|t| t.blob(&o.path).ok().flatten()) == blob
                }
            };
            rows.push(Row {
                blob,
                stamp: o.stamp,
                committed,
            });
        }
        Ok(Checkpoint {
            sha256: self.sha256,
            rows,
        })
    }
}
