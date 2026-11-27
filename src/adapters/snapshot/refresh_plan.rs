//! Decide whether an edit changes any bytes consumed by a query product.
mod blob_index;
mod committed;
use blob_index::BlobIndex;

use super::{
    Pin,
    checkpoint::{Checkpoint, Row},
};
use crate::adapters::repository::{self, BlobId, CommitTree, ObservedPath};
use crate::domain::{code::Language, markdown::is_markdown};
use std::{io, path::Path, sync::OnceLock};

pub(super) struct Plan<'a, 'p> {
    root: &'a Path,
    pub(super) prior: Option<&'p Pin>,
    checkpoint: Option<&'p Checkpoint>,
    capture_root: Option<repository::CaptureRoot<'a>>,
    sha256: bool,
    // The metadata probe stops at its first different file. Capture reuses that proof.
    changed: Option<(usize, Option<BlobId>)>,
    // Leading catalog ordinals with the same path and proven unchanged content.
    verified_prefix: usize,
    by_blob: OnceLock<BlobIndex>,
}

struct ContentIdentity<'a> {
    text: Option<&'a str>,
    hash: Option<BlobId>,
}

impl<'a> ContentIdentity<'a> {
    fn capture(text: Option<&'a str>, sha256: bool) -> Self {
        Self { text, hash: text.map(|text| repository::raw_blob_id(text.as_bytes(), sha256)) }
    }
}

enum Compared {
    Retained,
    DifferentPath,
    Changed { text: Option<String>, hash: Option<BlobId> },
}

impl<'a, 'p> Plan<'a, 'p> {
    pub(super) fn new(root: &'a Path, prior: Option<&'p Pin>) -> io::Result<Self> {
        let sha256 = repository::uses_sha256(root)?;
        let checkpoint = prior.and_then(|pin| pin.checkpoint.as_ref()).filter(|cp| cp.sha256 == sha256);
        Ok(Self {
            root, prior, checkpoint,
            capture_root: None,
            sha256,
            by_blob: OnceLock::new(),
            changed: None,
            verified_prefix: 0,
        })
    }

    // The first changed file opens the repository. If proof turns into a
    // rebuild, transfer the same descriptor to the parallel capture workers.
    pub(super) fn take_capture_root(&mut self) -> io::Result<repository::CaptureRoot<'a>> {
        self.capture_root.take().map_or_else(|| repository::CaptureRoot::new(self.root), Ok)
    }
    fn capture(&mut self, row: &mut ObservedPath) -> io::Result<Option<String>> {
        if let Some(body) = row.captured.take() {
            return Ok(body);
        }
        let root = match self.capture_root {
            Some(ref root) => root,
            None => self.capture_root.insert(repository::CaptureRoot::new(self.root)?),
        };
        root.capture(&row.path, row.stamp)
    }
    fn prior_hash(&self, id: usize) -> Option<Option<BlobId>> {
        Some(self.checkpoint?.rows[id].blob)
    }
    pub(super) fn verify_metadata_only(
        &mut self,
        obs: &mut [ObservedPath],
        pin: &Pin,
        unchanged: usize,
    ) -> io::Result<Option<&'p Checkpoint>> {
        let Some(checkpoint) = self.checkpoint else { return Ok(None) };
        if obs.len() != pin.files.len() {
            return Ok(None);
        }
        for (id, observed) in obs.iter_mut().enumerate().skip(unchanged) {
            let comparison = self.compare(pin, id, observed)?;
            if !self.retain_observation(id, observed, comparison) {
                return Ok(None);
            }
        }
        // External dependencies can still force a rebuild after every source
        // has been proven identical. Forward those bytes through that build.
        self.verified_prefix = obs.len();
        Ok(Some(checkpoint))
    }
    fn compare(&mut self, pin: &Pin, id: usize, observed: &mut ObservedPath) -> io::Result<Compared> {
        if observed.path != pin.files[id].path() {
            return Ok(Compared::DifferentPath);
        }
        if observed.stamp == pin.files[id].stamp() {
            return Ok(Compared::Retained);
        }
        let text = self.capture(observed)?;
        let identity = ContentIdentity::capture(text.as_deref(), self.sha256);
        if self.equal(pin, id, &identity)? {
            return Ok(Compared::Retained);
        }
        let hash = identity.hash;
        Ok(Compared::Changed { text, hash })
    }

    fn retain_observation(&mut self, id: usize, observed: &mut ObservedPath, comparison: Compared) -> bool {
        match comparison {
            Compared::Retained => return true,
            Compared::DifferentPath => {}
            Compared::Changed { text, hash } => {
                self.changed = Some((id, hash));
                observed.captured = Some(text);
            }
        }
        self.verified_prefix = id;
        false
    }

    fn equal(&self, pin: &Pin, id: usize, identity: &ContentIdentity<'_>) -> io::Result<bool> {
        if self.prior_hash(id) != Some(identity.hash) {
            return Ok(false);
        }
        match identity.text {
            Some(text) => pin.same_source(id, text),
            None => Ok(pin.files[id].bytes.is_none()),
        }
    }
    pub(super) fn predecessor(&self, unchanged: usize) -> crate::application::index::build::Predecessor<'p> {
        crate::application::index::build::Predecessor {
            previous: self.prior.map(|p| p as &dyn crate::application::index::build::Previous),
            forward_known: self.checkpoint.is_some(),
            verified_prefix: unchanged.max(self.verified_prefix),
        }
    }
    pub(super) fn identify(
        &self,
        index: usize,
        o: &ObservedPath,
        text: Option<&str>,
        row: crate::application::index::build::PriorRow,
    ) -> io::Result<(Option<usize>, Option<BlobId>)> {
        if let Some((id, hash)) = self.changed && id == index {
            debug_assert_eq!(row.ordinal(), Some(id));
            return Ok((None, hash));
        }
        let unchanged = row.unchanged();
        let hash = unchanged
            .and_then(|id| self.prior_hash(id))
            .unwrap_or_else(|| ContentIdentity::capture(text, self.sha256).hash);
        let identity = ContentIdentity { text, hash };
        let reused = match (unchanged, self.prior) {
            (Some(id), _) => Some(id),
            (None, Some(pin)) => self.find_previous(pin, row.ordinal(), &o.path, identity)?,
            (None, None) => None,
        };
        Ok((reused, hash))
    }

    fn find_previous(
        &self,
        pin: &Pin,
        same_path: Option<usize>,
        path: &str,
        identity: ContentIdentity<'_>,
    ) -> io::Result<Option<usize>> {
        if let Some(id) = same_path {
            return Ok(self.equal(pin, id, &identity)?.then_some(id));
        }
        let Some((blob, text)) = identity.hash.zip(identity.text) else {
            return Ok(None);
        };
        let Some(checkpoint) = self.checkpoint else { return Ok(None) };
        let candidates = self
            .by_blob
            .get_or_init(|| BlobIndex::new(checkpoint))
            .paths(blob);
        if candidates.is_empty() {
            return Ok(None);
        }
        let markdown = is_markdown(path);
        let language = Language::for_path(path);
        for &id in candidates {
            let from = pin.files[id].path();
            if is_markdown(from) == markdown
                && Language::for_path(from) == language
                && pin.same_source(id, text)?
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    pub(super) fn finish<'r>(
        &self,
        commit: Option<&str>,
        files: impl ExactSizeIterator<Item = (&'r str, [u64; 7], Option<BlobId>)>,
        paths: &crate::application::index::build::PathOrder,
    ) -> Checkpoint {
        let mut provenance =
            committed::Provenance::new(self.root, commit, self.prior.and_then(|p| p.indexed_commit.as_deref()));
        let mut rows = Vec::with_capacity(files.len());
        for (i, (path, stamp, blob)) in files.enumerate() {
            let prev = self.checkpoint.and_then(|cp| Some(&cp.rows[paths.previous_at(i)?]));
            let committed = provenance.committed(path, blob, prev);
            rows.push(Row {
                blob,
                stamp,
                committed,
            });
        }
        Checkpoint { sha256: self.sha256, rows }
    }
}
