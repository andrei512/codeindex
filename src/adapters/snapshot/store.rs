mod attempt;
mod candidate;
use candidate::Candidate;
mod lease;
mod publication;
mod recovery;
mod selector;
mod source;
mod staged;
use crate::adapters::repository::{ObservedPath, observe};
use crate::adapters::{
    repository,
    snapshot::{
        Envelope, FileKind,
        manifest::{self, Directory},
    },
};
use crate::application::index::{
    Entry, binary,
    build::{Inputs, Lanes, PathOrder, Worker},
    files,
};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

use super::{
    checkpoint::Checkpoint,
    pieces::{self, Segments},
    refresh_plan::Plan,
};

/// New immutable products awaiting publication. A metadata-only checkpoint
/// borrows the existing pin's products through the selector switch.
struct Rebuilt {
    dir: Directory,
    map: pieces::Map,
    files: Vec<Entry>,
    sdk_catalog: Option<usize>,
    receipts: Vec<crate::domain::code::CReceipt>,
}

/// The open descriptor keeps its inode alive while the selected path may be replaced.
struct HeldSelector {
    file: File,
    identity: Option<(u64, u64)>,
}

#[derive(Debug, PartialEq, Eq)]
enum SelectorState {
    Held,
    Replaced,
    Missing,
}

impl HeldSelector {
    fn new(file: File) -> Self {
        Self {
            file,
            identity: None,
        }
    }

    fn state(&mut self, current: &Path) -> io::Result<SelectorState> {
        let held = self.identity()?;
        match fs::metadata(current) {
            Ok(selected) if (selected.dev(), selected.ino()) == held => Ok(SelectorState::Held),
            Ok(_) => Ok(SelectorState::Replaced),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SelectorState::Missing),
            Err(e) => Err(e),
        }
    }
    fn identity(&mut self) -> io::Result<(u64, u64)> {
        if let Some(identity) = self.identity {
            return Ok(identity);
        }
        let meta = self.file.metadata()?;
        Ok(*self.identity.insert((meta.dev(), meta.ino())))
    }
}

pub struct Pin {
    pub(super) checkpoint: Option<Checkpoint>,
    segment_id: u64,
    coverage: std::sync::OnceLock<(usize, usize)>,
    sdk_catalog: Option<usize>,
    sdk_receipts: std::sync::OnceLock<Vec<crate::domain::code::CReceipt>>,
    selector: Option<HeldSelector>,
    pub generation: u64,
    pub indexed_commit: Option<String>,
    pub reused: bool,
    pub root: PathBuf,
    pub files: Vec<Entry>,
    segment: Segments,
    native_v2: bool,
    dir: Directory,
    migration: super::registry::Migration,
}
fn cwd_cache() -> io::Result<PathBuf> {
    env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".codeindex/repos"))
        .ok_or_else(|| io::Error::other("HOME is not set"))
}
fn segment_name(id: u64) -> String {
    pieces::name(id)
}
fn private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}
#[cfg(target_os = "macos")]
fn retry_sync(mut syscall: impl FnMut() -> i32) -> io::Result<()> {
    while syscall() != 0 {
        interrupted_or_error(io::Error::last_os_error())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn interrupted_or_error(error: io::Error) -> io::Result<()> {
    if error.kind() == io::ErrorKind::Interrupted { Ok(()) } else { Err(error) }
}

fn durable(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        }
        const F_FULLFSYNC: i32 = 51;
        retry_sync(|| unsafe { fcntl(file.as_raw_fd(), F_FULLFSYNC) })
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

#[derive(Clone, Copy)]
struct PublicationBarrier {
    #[cfg(target_os = "macos")]
    device: u64,
}

impl PublicationBarrier {
    fn new(_directory: &File) -> io::Result<Self> {
        Ok(Self {
            #[cfg(target_os = "macos")]
            device: _directory.metadata()?.dev(),
        })
    }

    fn stage(self, file: &File) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            unsafe extern "C" { fn fsync(fd: i32) -> i32; }
            if file.metadata()?.dev() != self.device {
                return Err(io::Error::other(
                    "snapshot part and cache directory occupy different devices",
                ));
            }
            retry_sync(|| unsafe { fsync(file.as_raw_fd()) })
        }
        #[cfg(not(target_os = "macos"))]
        durable(file)
    }

    #[cfg(target_os = "macos")]
    fn commit(self, directory: &File) -> io::Result<()> {
        // fcntl(2): F_FULLFSYNC persists earlier fsync(2) calls on this
        // device. Join all staged part and manifest writes before this call.
        self.stage(directory)?;
        durable(directory)
    }
}
const PUBLISHING: &str = "publishing";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publication {
    SegmentDurable,
    ManifestDurable,
    Replaced,
    SelectorDurable,
}
#[inline]
fn checkpoint(_stage: Publication) {
    #[cfg(test)]
    if std::env::var("CODEINDEX_TEST_CRASH_STAGE").ok().as_deref() == Some(&format!("{_stage:?}")) {
        // No unwinding: simulate a writer process dying while it owns the lease.
        std::process::exit(86)
    }
}
impl Pin {
    pub(crate) fn is_memory(&self) -> bool {
        self.selector.is_none()
    }

    pub(crate) fn memory(
        root: &Path,
        needs: crate::application::index::build::MemoryQuery,
    ) -> io::Result<Self> {
        let paths = repository::plain_paths(root, false)?;
        let capture = repository::CaptureRoot::new(root)?;
        let (files, dir, data) =
            crate::application::index::build::memory_index(root, paths, needs, |p, stamp| {
                capture.capture(p, stamp)
            })?;
        Ok(Self {
            files,
            dir,
            root: root.into(),
            segment: Segments::memory(data),
            generation: 0,
            segment_id: 0,
            reused: false,
            checkpoint: None,
            indexed_commit: None,
            selector: None,
            native_v2: false,
            sdk_catalog: None,
            sdk_receipts: Default::default(),
            coverage: Default::default(),
            migration: super::registry::Migration::current(),
        })
    }

    /// File kinds and paths are immutable for the lifetime of this generation.
    pub(crate) fn coverage_counts(&self) -> (usize, usize) {
        *self.coverage.get_or_init(|| {
            let mut text = 0;
            let mut parsed = 0;
            for file in &self.files {
                if file.bytes.is_some() {
                    text += 1;
                    parsed +=
                        usize::from(crate::domain::code::Language::for_path(file.path()).is_some());
                }
            }
            (text, parsed)
        })
    }

    /// Query products may span several metadata-only checkpoint publications.
    pub fn product_generation(&self) -> u64 {
        self.segment_id
    }

    pub fn committed_files(&self) -> usize {
        self.checkpoint
            .as_ref()
            .map_or(0, |cp| cp.rows.iter().filter(|r| r.committed).count())
    }

    fn open(cache: &Path, root: &Path) -> io::Result<Option<Self>> {
        let file = match File::open(cache.join("current")) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let (m, id) = Envelope::open_manifest(file)?;
        let layout_end = m.root().end;
        let payload = m.manifest_payload()?;
        let manifest::Selected {
            directory: dir,
            migration,
            commit: indexed_commit,
            generation: segment_id,
            map,
            checkpoint,
        } = Directory::decode(&payload, layout_end as usize, root, id)?;
        let segment = Segments::open(cache, map)?;
        let mut p = Self {
            native_v2: layout_end == 312,
            coverage: Default::default(),
            sdk_catalog: None,
            sdk_receipts: Default::default(),
            selector: Some(HeldSelector::new(m.into_file())),
            generation: id,
            indexed_commit,
            segment_id,
            checkpoint: None,
            reused: true,
            root: root.into(),
            files: Vec::new(),
            segment,
            dir,
            migration,
        };
        let source = &p.dir.0[manifest::SOURCE - 1];
        p.files = files(&p.product(manifest::FILES)?, source.end - source.start)?;
        p.load_checkpoint(checkpoint)?;
        Ok(Some(p))
    }
    fn load_checkpoint(&mut self, checkpoint: Option<&[u8]>) -> io::Result<()> {
        if let Some(encoded) = checkpoint {
            let checkpoint = Checkpoint::decode_catalog(
                encoded,
                self.files.iter().map(|f| (f.catalog_stamp(), f.blob)),
            )?;
            for (id, (row, file)) in checkpoint.rows.iter().zip(&mut self.files).enumerate() {
                if self.sdk_catalog.is_none()
                    && crate::domain::code::compilation_database(file.path())
                {
                    self.sdk_catalog = Some(id);
                }
                if row.blob.is_some() != file.bytes.is_some() {
                    return Err(binary::bad());
                }
                file.update_stamp(row.stamp);
            }
            self.checkpoint = Some(checkpoint);
        } else {
            self.sdk_catalog = self
                .files
                .iter()
                .position(|file| crate::domain::code::compilation_database(file.path()));
        }
        Ok(())
    }
    fn product_range(&self, product: usize) -> io::Result<std::ops::Range<u64>> {
        self.dir
            .0
            .get(product.checked_sub(1).ok_or_else(binary::bad)?)
            .cloned()
            .ok_or_else(binary::bad)
    }
    pub fn reader(
        &self,
        product: usize,
    ) -> io::Result<impl crate::domain::content_lookup::ExtentRead + '_> {
        let extent = self.product_range(product)?;
        Ok(ExtentCursor {
            file: &self.segment,
            extent,
            window: Vec::new(),
            begin: 0,
        })
    }
    fn markdown_reader(
        &self,
    ) -> io::Result<(
        crate::application::index::markdown_lookup::Lookup,
        impl crate::domain::content_lookup::ExtentRead + '_,
    )> {
        let mut reader = self.reader(manifest::MD)?;
        let lookup =
            crate::application::index::markdown_lookup::Lookup::read_directory(&mut reader)?;
        Ok((lookup, reader))
    }
    pub fn product(&self, product: usize) -> io::Result<Vec<u8>> {
        let r = self.product_range(product)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact((r.end - r.start).try_into().map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        self.segment.visit(r, |b| bytes.write_all(b))?;
        Ok(bytes)
    }
    pub(crate) fn decode_evidence(
        &self,
        entry: &Entry,
    ) -> io::Result<crate::domain::code::Analysis> {
        use crate::domain::code::Analysis;
        if entry.evidence.is_empty() {
            return Ok(Analysis::default());
        }
        let mut data = Vec::new();
        self.segment
            .visit(entry.evidence.clone(), |bytes| data.write_all(bytes))?;
        let mut input = data.as_slice();
        let analysis = Analysis::decode(&mut input)?;
        if !input.is_empty() {
            return Err(binary::bad());
        }
        Ok(analysis)
    }

    pub(crate) fn has_compilation_database(&self) -> bool {
        self.sdk_catalog.is_some()
    }

    fn sdk_receipts(&self) -> io::Result<&[crate::domain::code::CReceipt]> {
        if let Some(receipts) = self.sdk_receipts.get() {
            return Ok(receipts);
        }
        let Some(id) = self.sdk_catalog.filter(|_| self.checkpoint.is_some()) else {
            return Ok(&[]);
        };
        let analysis = self.decode_evidence(&self.files[id])?;
        let receipts = crate::domain::code::CProject::take_dependencies(analysis)
            .map(|inputs| inputs.receipts)
            .unwrap_or_default();
        let _ = self.sdk_receipts.set(receipts);
        Ok(self.sdk_receipts.get().unwrap())
    }

    fn sdk_current(&self) -> io::Result<bool> {
        Ok(self.sdk_receipts()?.iter().all(|receipt| receipt.valid()))
    }

    /// Rows verified against this immutable generation. A mismatch at the
    /// end of a large catalog must not force another walk of the same prefix.
    fn matching_prefix(&self, obs: &[ObservedPath]) -> Option<usize> {
        let comparable = self.native_v2 && self.migration.reusable();
        if !comparable || self.files.len() != obs.len() {
            return None;
        }
        Some(
            self.files
                .iter()
                .zip(obs)
                .position(|(file, observed)| !file.matches_observation(observed))
                .unwrap_or(obs.len()),
        )
    }
    pub fn refresh(cwd: &Path) -> io::Result<Self> {
        Self::refresh_with_prior(cwd, None)
    }

    /// Update a held pin under the lease. A retained descriptor is the only
    /// authority for carrying an optimistic catalog comparison across lock changes.
    fn select(cache: &Path, root: &Path, current: &mut Option<Self>) -> io::Result<bool> {
        if let Some(pin) = current.as_mut().filter(|p| p.root == root)
            && let Some(selector) = &mut pin.selector
        {
            match selector.state(&cache.join("current"))? {
                SelectorState::Held => {
                    pin.reused = true;
                    return Ok(true);
                }
                SelectorState::Missing => {
                    *current = None;
                    return Ok(false);
                }
                SelectorState::Replaced => {}
            }
        }
        *current = Self::open(cache, root)?;
        Ok(false)
    }

    pub fn refresh_with_prior(cwd: &Path, prior: Option<Self>) -> io::Result<Self> {
        Self::refresh_root(repository::locate(cwd)?, prior)
    }

    pub(crate) fn refresh_root(root: PathBuf, prior: Option<Self>) -> io::Result<Self> {
        let cache = cwd_cache()?.join(repository::cache_key(&root));
        lease::prepare_cache(&cache)?;
        let lock = lease::open(&cache)?;
        let (mut old, observation) = match lease::inspect(&lock, &cache, &root, prior)? {
            lease::Inspection::Ready(pin) => return Ok(pin),
            lease::Inspection::Refresh {
                current,
                observation,
            } => (current, observation),
        };
        let waited = lease::exclusive(&lock)?;
        let retained = Self::select(&cache, &root, &mut old)?;
        let mut recovery = recovery::Recovery::open(&cache, old.as_ref())?;
        let observation = recovery.observe_current(
            &root,
            old.as_ref(),
            observation.filter(|_| retained && !waited),
        )?;
        let commit = repository::head_commit(&root)?;
        if let Some(pin) = old.as_ref() {
            if observation.current(pin, lease::Freshness::Locked(&commit))? {
                return Ok(old.unwrap());
            }
            recovery.mark()?;
        }
        let staging = attempt::Staging {
            root: &root,
            cache: &cache,
            directory: &recovery.directory,
            old: old.as_ref(),
        };
        let verified = staging.verify(observation.rows, commit, observation.prefix)?;
        let published = publication::Published::commit(verified, &cache, &mut recovery)?;
        published.pin_and_retire(&cache, root, old, &recovery)
    }
}
impl crate::application::index::build::Previous for Pin {
    fn compilation_database(&self) -> Option<usize> {
        self.sdk_catalog
    }
    fn compatible(&self, kind: usize) -> bool {
        !self.migration.requires_rebuild(kind)
    }
    fn entries(&self) -> &[Entry] {
        &self.files
    }
    fn graph_reader(
        &self,
        kind: usize,
    ) -> io::Result<Box<dyn crate::domain::content_lookup::ExtentRead + '_>> {
        Ok(Box::new(self.reader(kind)?))
    }
    fn product(&self, kind: usize) -> io::Result<Vec<u8>> {
        Pin::product(self, kind)
    }
    fn extent(&self, product: usize) -> std::ops::Range<u64> {
        self.dir.0[product - 1].clone()
    }
    fn copy_bytes(&self, range: std::ops::Range<u64>, out: &mut dyn Write) -> io::Result<()> {
        self.segment.visit(range, |bytes| out.write_all(bytes))
    }
    fn same_bytes(&self, range: std::ops::Range<u64>, expected: &[u8]) -> io::Result<bool> {
        let mut remaining = Some(expected);
        self.segment.visit_cached(range, |chunk| {
            remaining = remaining.and_then(|tail| tail.strip_prefix(chunk));
            Ok(())
        })?;
        Ok(remaining == Some(&[]))
    }
    fn markdown_record(&self, id: usize) -> io::Result<Option<std::ops::Range<u64>>> {
        let (lookup, mut reader) = self.markdown_reader()?;
        let base = self.dir.0[manifest::MD - 1].start;
        Ok(lookup
            .document_range(&mut reader, id)?
            .map(|range| base + range.start..base + range.end))
    }
    fn source_end_word(&self, file: usize, end: usize) -> io::Result<Option<u64>> {
        use crate::application::index::symbol_lookup::SymbolLookup;
        let mut symbols = SymbolLookup::open(self.reader(manifest::SYMBOLS)?, self.files.len())?;
        let base = self.dir.0[manifest::SYMBOLS - 1].start;
        Ok(symbols
            .source_end_word(file, end)?
            .map(|offset| base + offset))
    }
    fn local(
        &self,
        id: usize,
        decode_code: bool,
        decode_markdown: bool,
    ) -> io::Result<crate::application::index::build::Local> {
        use crate::{
            application::index::build::Local,
            domain::code::Analysis,
        };
        let f = &self.files[id];
        let code = if decode_code {
            self.decode_evidence(f)?
        } else {
            Analysis::default()
        };
        let markdown = self.local_markdown(id, f, decode_markdown)?;
        Ok(Local {
            score: f.score.clone(),
            test_score: f.test_score.clone(),
            code,
            markdown,
            line_ends: f.line_ends.clone(),
            byte_len: f.bytes.as_ref().map_or(0, |r| (r.end - r.start) as usize),
        })
    }
}

impl Pin {
    fn local_markdown(&self, id: usize, file: &Entry, enabled: bool) -> io::Result<Option<Box<crate::domain::markdown::Parsed>>> {
        if !enabled || !file.has_markdown() {
            return Ok(None);
        }
        let (lookup, reader) = self.markdown_reader()?;
        Ok(lookup.document(reader, id)?.map(Box::new))
    }
}
struct Remove(Option<PathBuf>);
impl Drop for Remove {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_file(p);
        }
    }
}

struct ExtentCursor<'a> {
    file: &'a Segments,
    extent: std::ops::Range<u64>,
    window: Vec<u8>,
    begin: u64,
}
impl crate::domain::content_lookup::ExtentRead for ExtentCursor<'_> {
    fn length(&self) -> u64 {
        self.extent.end - self.extent.start
    }
    fn read(&mut self, offset: u64, dest: &mut [u8]) -> io::Result<()> {
        if offset
            .checked_add(dest.len() as u64)
            .is_none_or(|end| end > self.length())
        {
            return Err(binary::bad());
        }
        let mut at = self.extent.start + offset;
        if let Some(bytes) = self.file.memory_range(at..at + dest.len() as u64)? {
            dest.copy_from_slice(bytes);
            return Ok(());
        }
        // Large contiguous reads can stream into the caller's buffer without
        // copying through the read-ahead window used by scalar directory probes.
        if dest.len() >= 32768 {
            let end = at + dest.len() as u64;
            let mut dest = dest;
            return self
                .file
                .visit_cached(at..end, |chunk| dest.write_all(chunk));
        }
        let mut dest = dest;
        while !dest.is_empty() {
            if at < self.begin || at >= self.begin + self.window.len() as u64 {
                self.begin = at / 4096 * 4096;
                self.window.clear();
                self.file
                    .visit_cached(self.begin..(self.begin + 4096).min(self.extent.end), |b| {
                        self.window.write_all(b)
                    })?;
            }
            let available = &self.window[(at - self.begin) as usize..];
            let n = dest.len().min(available.len());
            dest[..n].copy_from_slice(&available[..n]);
            dest = &mut dest[n..];
            at += n as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod publication_tests;
