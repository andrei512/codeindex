//! Readers validate the selected generation before returning. A writer may reuse
//! an optimistic observation only if it acquired the lease without waiting and
//! the generation it observed is still selected.
use super::{
    File, ObservedPath, OpenOptions, OpenOptionsExt, PUBLISHING, Path, Pin, observe, repository,
};
use std::{fs, io, os::unix::fs::PermissionsExt};

/// An unchanged query should leave cache directory metadata untouched.
pub(super) fn prepare_cache(cache: &Path) -> io::Result<()> {
    match fs::metadata(cache) {
        Ok(meta) if meta.is_dir() => {
            if meta.permissions().mode() & 0o7777 == 0o700 {
                return Ok(());
            }
        }
        Ok(_) => fs::create_dir_all(cache)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir_all(cache)?,
        Err(e) => return Err(e),
    }
    fs::set_permissions(cache, fs::Permissions::from_mode(0o700))
}

pub(super) fn open(cache: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(cache.join("refresh.lock"))
}

pub(super) struct Checked {
    pub rows: Vec<ObservedPath>,
    pub prefix: Option<usize>,
}

impl Checked {
    pub fn new(rows: Vec<ObservedPath>, pin: Option<&Pin>) -> Self {
        let prefix = pin.and_then(|pin| pin.matching_prefix(&rows));
        Self { rows, prefix }
    }

    pub fn current(&self, pin: &Pin, order: Freshness<'_>) -> io::Result<bool> {
        if self.prefix != Some(self.rows.len()) {
            return Ok(false);
        }
        match order {
            Freshness::Optimistic(root) => {
                Ok(pin.sdk_current()? && pin.indexed_commit == repository::head_commit(root)?)
            }
            Freshness::Locked(commit) => Ok(pin.indexed_commit == *commit && pin.sdk_current()?),
        }
    }
}

/// Optimistic validation probes SDK before HEAD. A writer has already read HEAD
/// for the next generation, and only checks SDK when that commit still matches.
pub(super) enum Freshness<'a> {
    Optimistic(&'a Path),
    Locked(&'a Option<String>),
}

pub(super) enum Inspection {
    Ready(Pin),
    Refresh {
        current: Option<Pin>,
        observation: Option<Checked>,
    },
}

pub(super) fn inspect(
    lock: &File,
    cache: &Path,
    root: &Path,
    mut current: Option<Pin>,
) -> io::Result<Inspection> {
    lock.lock_shared()?;
    Pin::select(cache, root, &mut current)?;
    let marker = cache.join(PUBLISHING);
    let interrupted = marker.try_exists()?;
    lock.unlock()?;
    let Some(pin) = current.as_mut().filter(|_| !interrupted) else {
        return Ok(Inspection::Refresh { current, observation: None });
    };
    let observation = Checked::new(observe(root)?, Some(pin));
    if !observation.current(pin, Freshness::Optimistic(root))? {
        return Ok(Inspection::Refresh { current, observation: Some(observation) });
    }
    if still_selected(pin, lock, cache, &marker)? {
        return Ok(Inspection::Ready(current.unwrap()));
    }
    Ok(Inspection::Refresh { current, observation: None })
}

fn still_selected(pin: &mut Pin, lock: &File, cache: &Path, marker: &Path) -> io::Result<bool> {
    lock.lock_shared()?;
    let selector = pin
        .selector
        .as_mut()
        .expect("a selected disk pin holds its selector");
    let unchanged = !marker.try_exists()?
        && selector.state(&cache.join("current"))? == super::SelectorState::Held;
    lock.unlock()?;
    Ok(unchanged)
}

pub(super) fn exclusive(lock: &File) -> io::Result<bool> {
    match lock.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => {
            lock.lock()?;
            Ok(true)
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}
