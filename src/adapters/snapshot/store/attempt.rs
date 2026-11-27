//! Prepare one candidate from one observed checkout under the writer lease.
use super::*;
use crate::application::index::same_observation;

/// A failed metadata probe or build can retry only after proving checkout churn.
/// Errors opening, staging, and publishing a candidate propagate directly.
pub(super) enum Failure {
    VerifyOrBuild(io::Error),
    Storage(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Storage(error)
    }
}

pub(super) struct Prepared<'a> {
    pub candidate: Candidate<'a>,
    pub provenance: Checkpoint,
    pub generation: u64,
    pub retirement: Option<Worker<()>>,
}

pub(super) struct Verified<'a> {
    pub prepared: Prepared<'a>,
    pub selector: selector::Selector,
    pub commit: Option<String>,
    pub unchanged: usize,
}

pub(super) struct Staging<'a, 'p> {
    pub root: &'a Path,
    pub cache: &'a Path,
    pub directory: &'a File,
    pub old: Option<&'p Pin>,
}

enum SelectorCheck {
    Current(selector::Selector),
    Changed(Vec<ObservedPath>),
}

struct AttemptInputs {
    observations: Vec<ObservedPath>,
    commit: Option<String>,
    unchanged: usize,
}

enum Attempt<'p> {
    Ready(Verified<'p>),
    Retry(Vec<ObservedPath>),
}

impl<'p> Staging<'_, 'p> {
    pub fn verify(
        &self,
        observations: Vec<ObservedPath>,
        commit: Option<String>,
        known_prefix: Option<usize>,
    ) -> io::Result<Verified<'p>> {
        let initial = AttemptInputs { observations, commit, unchanged: known_prefix.unwrap_or(0) };
        match self.verify_once(initial, false)? {
            Attempt::Ready(result) => Ok(result),
            Attempt::Retry(observations) => {
                let commit = repository::head_commit(self.root)?;
                let retry = AttemptInputs { observations, commit, unchanged: 0 };
                let Attempt::Ready(result) = self.verify_once(retry, true)? else {
                    unreachable!("a second attempt turns churn into an error");
                };
                Ok(result)
            }
        }
    }

    fn verify_once(&self, mut inputs: AttemptInputs, second: bool) -> io::Result<Attempt<'p>> {
        let prepared = match self.prepare(&mut inputs) {
            Ok(prepared) => prepared,
            Err(Failure::Storage(error)) => return Err(error),
            Err(Failure::VerifyOrBuild(error)) if second => return Err(error),
            Err(Failure::VerifyOrBuild(error)) => return self.retry_if_changed(&inputs, error),
        };
        self.verify_selector(prepared, inputs, second)
    }

    fn verify_selector(&self, mut prepared: Prepared<'p>, inputs: AttemptInputs, second: bool) -> io::Result<Attempt<'p>> {
        let selector = match self.check_selector(&mut prepared, &inputs)? {
            SelectorCheck::Current(selector) => selector,
            SelectorCheck::Changed(_) if second => return Err(io::Error::other("checkout changed during both refresh attempts")),
            SelectorCheck::Changed(after) => return Ok(Attempt::Retry(after)),
        };
        Ok(Attempt::Ready(Verified { prepared, selector, commit: inputs.commit, unchanged: inputs.unchanged }))
    }

    fn retry_if_changed(&self, inputs: &AttemptInputs, error: io::Error) -> io::Result<Attempt<'p>> {
        let fresh = observe(self.root)?;
        if same_observation(&inputs.observations, &fresh) {
            return Err(error);
        }
        Ok(Attempt::Retry(fresh))
    }

    fn prepare(&self, inputs: &mut AttemptInputs) -> Result<Prepared<'p>, Failure> {
        let obs = &mut inputs.observations;
        let commit = inputs.commit.as_deref();
        let unchanged = inputs.unchanged;
        // Parser or input classification upgrades can invalidate the catalog itself.
        let prior = self.old.filter(|p| {
            !p.migration.requires_rebuild(manifest::FILES)
                && !p.migration.requires_rebuild(manifest::SOURCE)
        });
        let mut plan = Plan::new(self.root, prior)?;
        let verified = match prior.filter(|p| p.migration.reusable()) {
            Some(pin) => plan
                .verify_metadata_only(obs, pin, unchanged)
                .map_err(Failure::VerifyOrBuild)?
                .map(|cp| (pin, cp)),
            _ => None,
        };
        let generation = self
            .old
            .map_or(0, |p| p.generation)
            .checked_add(1)
            .ok_or_else(binary::bad)?;
        if let Some((pin, cp)) = verified
            && pin.sdk_current()?
        {
            let provenance = cp.refresh_metadata(
                self.root,
                (commit, pin.indexed_commit.as_deref()),
                obs,
                unchanged,
            );
            return Ok(Prepared {
                candidate: Candidate::Retained(pin),
                provenance,
                retirement: None,
                generation,
            });
        }
        self.rebuild(inputs, plan, generation)
    }

    /// Stage the private selector while parts sync. Hand it to publication
    /// only after checking all inputs against this attempt's observation.
    fn check_selector(&self, prepared: &mut Prepared<'_>, inputs: &AttemptInputs) -> io::Result<SelectorCheck> {
        let commit = inputs.commit.as_deref();
        let previous = self.old.map_or(0, |p| p.generation);
        let identity = manifest::Identity { root: self.root, previous, commit };
        let (bytes, barrier) = prepared.candidate.manifest(identity, prepared.generation, &prepared.provenance)?;
        let selector = selector::Selector::prepare(self.cache, prepared.generation, bytes, barrier)?;
        let after = observe(self.root)?;
        if !prepared.candidate.sdk_current()?
            || !same_observation(&inputs.observations, &after)
            || repository::head_commit(self.root)?.as_deref() != commit
        {
            return Ok(SelectorCheck::Changed(after));
        }
        Ok(SelectorCheck::Current(selector))
    }

    fn rebuild(&self, inputs: &mut AttemptInputs, mut plan: Plan<'_, 'p>, mut generation: u64) -> Result<Prepared<'p>, Failure> {
        let commit = inputs.commit.as_deref();
        let prior = plan.prior;
        let fresh = self.old.is_none();
        let (mut primary, rank_lane) = staged::Primary::open(
            self.cache,
            self.directory,
            &mut generation,
            (prior.map(|p| &p.segment), fresh),
        )?;
        let capture_root = plan.take_capture_root()?;
        let capture = |_: usize, file: &ObservedPath, id: Option<usize>| {
            capture_source(prior.zip(id), &capture_root, file)
        };
        let mut fresh_provenance = None;
        let provenance_lane: Option<crate::application::index::build::ProvenanceLane<'_>> = if fresh
        {
            Some(Box::new(|observed, blobs| {
                let files = observed
                    .iter()
                    .zip(blobs)
                    .map(|(o, blob)| (o.path.as_str(), o.stamp, *blob));
                fresh_provenance = Some(plan.finish(commit, files, &PathOrder::Fresh));
            }))
        } else {
            None
        };
        let output = Inputs {
            root: self.root,
            read: &capture,
            predecessor: plan.predecessor(inputs.unchanged),
            identify: &|i, o, body, id| plan.identify(i, o, body, id),
        }
        .build(
            &mut inputs.observations,
            &mut primary.writer,
            Lanes {
                rank: rank_lane,
                provenance: provenance_lane,
            },
        )
        .map_err(Failure::VerifyOrBuild)?;
        let (parts, retirement, paths) = primary.stage(output)?;
        let provenance = fresh_provenance.unwrap_or_else(|| {
            let files = parts
                .products
                .files
                .iter()
                .map(|f| (f.path(), f.stamp(), f.blob));
            plan.finish(commit, files, &paths)
        });
        Ok(Prepared {
            candidate: Candidate::Rebuilt(parts),
            provenance,
            generation,
            retirement,
        })
    }
}

fn capture_source(
    reused: Option<(&Pin, usize)>,
    capture: &repository::CaptureRoot<'_>,
    file: &ObservedPath,
) -> io::Result<Option<String>> {
    match reused {
        Some((pin, id)) => pin.files[id].bytes.is_some().then(|| pin.source(id)).transpose(),
        None => capture.capture(&file.path, file.stamp),
    }
}
