use crate::adapters::repository::{ObservedPath, observe};
use crate::adapters::{
    repository,
    snapshot::{
        Envelope, FileKind,
        manifest::{self, Directory},
        write_envelope,
    },
};
use crate::application::index::{Entry, binary, build::build, files, same_observation};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

use super::{
    checkpoint::Checkpoint,
    pieces::{self, Segments, Writer},
    refresh_plan::Plan,
};

pub struct Pin {
    pub(super) checkpoint: Option<Checkpoint>,
    segment_id: u64,
    coverage: std::sync::OnceLock<(usize, usize)>,
    selector: File,
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
fn durable(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        }
        // F_FULLFSYNC commits cached drive data on the benchmark platform.
        if unsafe { fcntl(file.as_raw_fd(), 51) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()?;
    Ok(())
}
const PUBLISHING: &str = "publishing";

fn engine_private(name: &str) -> bool {
    fn generation(s: &str) -> bool {
        s.len() == 16
            && s.bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    }
    if let Some(id) = name
        .strip_prefix("segment-")
        .or_else(|| name.strip_prefix("part-"))
    {
        return generation(id);
    }
    name.strip_prefix("manifest-")
        .and_then(|s| s.split_once('-'))
        .is_some_and(|(id, pid)| {
            generation(id) && pid.parse::<u32>().is_ok_and(|n| n.to_string() == pid)
        })
}

// The refresh lease excludes writers. A validated pin already owns the segment
// handle, so unlinking an obsolete name cannot invalidate an existing reader.
fn recover(cache: &Path, current: Option<&Pin>) -> io::Result<()> {
    let marker = cache.join(PUBLISHING);
    if !marker.try_exists()? {
        return Ok(());
    }
    let directory = File::open(cache)?;
    // After a process died immediately after rename, current is visible but the
    // switch may still be volatile. Commit that choice before retiring either side.
    if current.is_some() {
        durable(&File::open(cache.join("current"))?)?;
    }
    directory.sync_all()?;
    let selected: std::collections::HashSet<_> = current
        .into_iter()
        .flat_map(|p| p.segment.keys().map(pieces::name))
        .collect();
    for entry in fs::read_dir(cache)? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str()
            && !selected.contains(name)
            && engine_private(name)
        {
            fs::remove_file(entry.path())?;
        }
    }
    directory.sync_all()?;
    fs::remove_file(marker)?;
    directory.sync_all()
}

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
    /// File kinds and paths are immutable for the lifetime of this generation.
    pub(crate) fn coverage_counts(&self) -> (usize, usize) {
        *self.coverage.get_or_init(|| {
            let mut text = 0;
            let mut parsed = 0;
            for file in &self.files {
                if file.bytes.is_some() {
                    text += 1;
                    parsed += usize::from(crate::domain::code::Language::for_path(file.path()).is_some());
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
        let mut id = [0; 8];
        file.read_exact_at(&mut id, 16)?;
        let id = u64::from_le_bytes(id);
        let m = Envelope::open(file.try_clone()?, FileKind::Manifest, id)?;
        let (dir, migration, indexed_commit, segment_id, map, encoded) =
            Directory::decode(&m, root, id)?;
        let segment = Segments::open(cache, map)?;
        let mut p = Self {
            native_v2: m.root().end == 312,
            coverage: Default::default(),
            selector: file,
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
        let raw = p.product(manifest::FILES)?;
        let source = &p.dir.0[manifest::SOURCE - 1];
        p.files = files(&raw, source.end - source.start)?;
        if m.root().end >= 264 {
            let checkpoint =
                Checkpoint::decode_catalog(&encoded,p.files.iter().map(|f|(f.catalog_stamp(),f.blob)))?;
            for (row, file) in checkpoint.rows.iter().zip(&mut p.files) {
                if row.blob.is_some() != file.bytes.is_some() {
                    return Err(binary::bad());
                }
                file.update_stamp(row.stamp);
            }
            p.checkpoint = Some(checkpoint);
        }
        Ok(Some(p))
    }
    pub fn reader(
        &self,
        product: usize,
    ) -> io::Result<impl crate::domain::content_lookup::ExtentRead + '_> {
        let extent = self
            .dir
            .0
            .get(product.checked_sub(1).ok_or_else(binary::bad)?)
            .ok_or_else(binary::bad)?
            .clone();
        Ok(ExtentCursor {
            file: &self.segment,
            extent,
            window: Vec::new(),
            begin: 0,
        })
    }
    pub fn product(&self, product: usize) -> io::Result<Vec<u8>> {
        let r = self
            .dir
            .0
            .get(product.checked_sub(1).ok_or_else(binary::bad)?)
            .ok_or_else(binary::bad)?
            .clone();
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact((r.end - r.start).try_into().map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        self.segment.visit(r, |b| bytes.write_all(b))?;
        Ok(bytes)
    }
    pub(crate) fn source_window(&self) -> SourceWindow<'_> {
        SourceWindow {
            pin: self,
            start: 0,
            data: Vec::new(),
        }
    }
    pub fn source(&self, id: usize) -> io::Result<String> {
        let Some(r) = self.files.get(id).and_then(|e| e.bytes.as_ref()) else {
            return Err(io::Error::other("path has no captured source"));
        };
        self.source_slice(id, 0..r.end - r.start)
    }
    pub fn source_slice(&self, id: usize, range: std::ops::Range<u64>) -> io::Result<String> {
        let r = self
            .files
            .get(id)
            .and_then(|f| f.bytes.as_ref())
            .ok_or_else(binary::bad)?;
        if range.start > range.end || range.end > r.end - r.start {
            return Err(binary::bad());
        }
        let start = self.dir.0[manifest::SOURCE - 1].start + r.start;
        let mut b = Vec::new();
        b.try_reserve_exact(
            (range.end - range.start)
                .try_into()
                .map_err(io::Error::other)?,
        )
        .map_err(io::Error::other)?;
        self.segment
            .visit_cached(start + range.start..start + range.end, |s| b.write_all(s))?;
        String::from_utf8(b).map_err(io::Error::other)
    }
    fn matches(&self, obs: &[ObservedPath]) -> bool {
        self.native_v2
            && self.migration.reusable()
            && self.files.len() == obs.len()
            && self
                .files
                .iter()
                .zip(obs)
                .all(|(f, o)| f.path() == o.path && f.stamp() == o.stamp)
    }
    pub fn refresh(cwd: &Path) -> io::Result<Self> {
        Self::refresh_with_prior(cwd, None)
    }

    fn select(cache: &Path, root: &Path, prior: Option<Self>) -> io::Result<Option<Self>> {
        if let Some(mut pin) = prior
            && pin.root == root
        {
            let held = pin.selector.metadata()?;
            match fs::metadata(cache.join("current")) {
                Ok(selected) if (selected.dev(), selected.ino()) == (held.dev(), held.ino()) => {
                    pin.reused = true;
                    return Ok(Some(pin));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        Self::open(cache, root)
    }

    pub fn refresh_with_prior(cwd: &Path, prior: Option<Self>) -> io::Result<Self> {
        let root = repository::locate(cwd)?;
        let cache = cwd_cache()?.join(repository::cache_key(&root));
        fs::create_dir_all(&cache)?;
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(cache.join("refresh.lock"))?;
        lock.lock_shared()?;
        let mut current = Self::select(&cache, &root, prior)?;
        let interrupted = cache.join(PUBLISHING).try_exists()?;
        lock.unlock()?;
        let mut observation = None;
        let mut observed_id = None;
        if !interrupted && let Some(pin) = &current {
            let obs = observe(&root)?;
            if pin.matches(&obs) && pin.indexed_commit == repository::head_commit(&root)? {
                lock.lock_shared()?;
                let mut generation = [0; 8];
                File::open(cache.join("current"))?.read_exact_at(&mut generation, 16)?;
                let interrupted = cache.join(PUBLISHING).try_exists()?;
                lock.unlock()?;
                if !interrupted && pin.generation == u64::from_le_bytes(generation) {
                    return Ok(current.take().unwrap());
                }
            } else {
                observed_id = Some(pin.generation);
                observation = Some(obs);
            }
        }
        let waited = match lock.try_lock() {
            Ok(()) => false,
            Err(std::fs::TryLockError::WouldBlock) => {
                lock.lock()?;
                true
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        };
        let mut old = Self::select(&cache, &root, current)?;
        recover(&cache, old.as_ref())?;
        let previous = old.as_ref().map_or(0, |p| p.generation);
        let mut obs = if !waited && observed_id == Some(previous) {
            observation.unwrap()
        } else {
            observe(&root)?
        };
        let head = repository::head_commit(&root)?;
        if old
            .as_ref()
            .is_some_and(|p| p.matches(&obs) && p.indexed_commit == head)
        {
            return Ok(old.unwrap());
        }
        if old.is_none() {
            // create_dir_all can have installed any prefix (including in a writer
            // that died before we acquired the lock). Persist the linking entries
            // of HOME/.codeindex/repos/<key> before selecting the first generation.
            for ancestor in cache.ancestors().skip(1).take(3) {
                File::open(ancestor)?.sync_all()?;
            }
        }
        let marker = cache.join(PUBLISHING);
        private(&marker)?;
        let directory = File::open(&cache)?;
        // Persist the recovery obligation before leaving any private file behind.
        directory.sync_all()?;
        for attempt in 0..2 {
            let commit = repository::head_commit(&root)?;
            // An old parser or input-classification contract can change the
            // meaning of the catalog itself. Recapture those generations.
            let prior = old.as_ref().filter(|p| {
                !p.migration.requires_rebuild(manifest::FILES)
                    && !p.migration.requires_rebuild(manifest::SOURCE)
            });
            let mut plan = Plan::new(obs.len(), repository::uses_sha256(&root)?);
            let same = match prior.filter(|p| p.migration.reusable()) {
                Some(pin) => match plan.verify_metadata_only(&root, &mut obs, pin) {
                    Ok(same) => same,
                    Err(e) if attempt == 0 => {
                        let fresh = observe(&root)?;
                        if same_observation(&fresh, &obs) {
                            return Err(e);
                        }
                        obs = fresh;
                        continue;
                    }
                    Err(e) => return Err(e),
                },
                None => false,
            };
            let mut generation = previous.checked_add(1).ok_or_else(binary::bad)?;
            let (dir, segment_id, map, mut built_files, mut cleanup) = if same {
                let pin = prior.unwrap();
                (
                    pin.dir.clone(),
                    pin.segment_id,
                    pin.segment.map.clone(),
                    Vec::new(),
                    Remove(None),
                )
            } else {
                let (path, file) = loop {
                    let path = cache.join(segment_name(generation));
                    match private(&path) {
                        Ok(f) => break (path, f),
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                            generation = generation.checked_add(1).ok_or_else(binary::bad)?
                        }
                        Err(e) => return Err(e),
                    }
                };
                let cleanup = Remove(Some(path));
                let mut w = Writer::new(file, generation, prior.map(|p| &p.segment))?;
                let capture = |root: &Path, path: &str, stamp: [u64; 7]| {
                    if let Some(pin) = prior
                        && let Ok(id) = pin.files.binary_search_by(|f| f.path().cmp(path))
                        && (pin.files[id].stamp() == stamp || plan.verified(id))
                    {
                        return if pin.files[id].bytes.is_some() {
                            pin.source(id).map(Some)
                        } else {
                            Ok(None)
                        };
                    }
                    repository::capture_observed(root, path, stamp)
                };
                let (dir, catalog) = match build(
                    &root,
                    &mut obs,
                    &capture,
                    prior.map(|p| p as &dyn crate::application::index::build::Previous),
                    prior.is_some_and(|p| plan.can_forward(p)),
                    &|i, o, body, id| plan.identify(i, o, body, id, prior),
                    &mut w,
                ) {
                    Ok(d) => d,
                    Err(error) if attempt == 0 => {
                        // A path may vanish or change while a capture worker has
                        // it open. Retry only if the new observation proves churn.
                        let fresh = observe(&root)?;
                        if !same_observation(&obs, &fresh) {
                            obs = fresh;
                            continue;
                        }
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                };
                let (file, map) = w.finish()?;
                durable(&file)?;
                directory.sync_all()?;
                (dir, generation, map, catalog, cleanup)
            };
            let provenance = plan.finish(&root, commit.as_deref(), &obs, prior)?;
            let after = observe(&root)?;
            if !same_observation(&obs, &after) || repository::head_commit(&root)? != commit {
                if attempt == 0 {
                    obs = after;
                    continue;
                }
                return Err(io::Error::other(
                    "checkout changed during both refresh attempts",
                ));
            }
            checkpoint(Publication::SegmentDurable);
            let mut evidence = Vec::new();
            let base_catalog=if same {&prior.unwrap().files}else{&built_files};
            provenance.encode_catalog(&mut evidence,base_catalog)?;
            let bytes = dir.manifest(
                &root,
                segment_id,
                previous,
                commit.as_deref(),
                &evidence,
                &map,
            );
            let temp = cache.join(format!("manifest-{generation:016x}-{}", std::process::id()));
            let mut m = private(&temp)?;
            let mut temp_cleanup = Remove(Some(temp.clone()));
            write_envelope(
                &mut m,
                FileKind::Manifest,
                generation,
                0..312,
                bytes.len() as u64,
                &bytes[..],
            )?;
            durable(&m)?;
            checkpoint(Publication::ManifestDurable);
            fs::rename(&temp, cache.join("current"))?;
            temp_cleanup.0 = None;
            cleanup.0 = None;
            checkpoint(Publication::Replaced);
            directory.sync_all()?;
            checkpoint(Publication::SelectorDurable);
            // The builder already owns the exact rows it sealed. Keep them for
            // the requesting client instead of decoding the same catalog again.
            if same {
                built_files = std::mem::take(&mut old.as_mut().unwrap().files);
                for (file, row) in built_files.iter_mut().zip(&provenance.rows) {
                    file.update_stamp(row.stamp);
                }
            }
            let p = Self {
                coverage: Default::default(),
                selector: m,
                native_v2: true,
                generation,
                indexed_commit: commit,
                segment_id,
                checkpoint: Some(provenance),
                reused: false,
                root: root.clone(),
                files: built_files,
                segment: Segments::open(&cache, map)?,
                dir,
                migration: super::registry::Migration::current(),
            };
            if let Some(pin) = old.as_ref() {
                for key in pin.segment.keys() {
                    if !p.segment.keys().any(|live| live == key) {
                        fs::remove_file(cache.join(pieces::name(key)))?;
                    }
                }
            }
            // Remove retirement and recovery names in order. A crash before this
            // directory barrier leaves a durable marker for another lease holder.
            directory.sync_all()?;
            fs::remove_file(&marker)?;
            directory.sync_all()?;
            return Ok(p);
        }
        unreachable!()
    }
}
impl crate::application::index::build::Previous for Pin {
    fn compatible(&self, kind: usize) -> bool {
        !self.migration.requires_rebuild(kind)
    }
    fn entries(&self) -> &[Entry] {
        &self.files
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
    fn local(
        &self,
        id: usize,
        decode_code: bool,
        decode_markdown: bool,
    ) -> io::Result<crate::application::index::build::Local> {
        use crate::{
            application::index::{build::Local, markdown_lookup::Lookup},
            domain::{code::Analysis, markdown::is_markdown},
        };
        let f = &self.files[id];
        let mut lf = f.line_ends.clone();
        let size = f.bytes.as_ref().map_or(0, |r| (r.end - r.start) as usize);
        let partial_line = size != 0 && lf.last() != Some(&size);
        if partial_line {
            lf.push(size);
        }
        let code = if !decode_code || f.evidence.is_empty() {
            Analysis::default()
        } else {
            let mut data = Vec::new();
            self.segment
                .visit(f.evidence.clone(), |b| data.write_all(b))?;
            let mut r = data.as_slice();
            let a = Analysis::decode(&mut r)?;
            if !r.is_empty() {
                return Err(binary::bad());
            }
            a
        };
        let markdown = if decode_markdown && f.bytes.is_some() && is_markdown(f.path()) {
            Lookup::open(self.reader(manifest::MD)?)?.document(self.reader(manifest::MD)?, id)?
        } else {
            None
        };
        Ok(Local {
            code,
            markdown,
            lf,
            partial_line,
        })
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

/// Sequential scans share verified read-ahead across neighboring documents.
/// Each returned str is still bounded by its file's exact byte range.
pub(crate) struct SourceWindow<'a> {
    pin: &'a Pin,
    start: u64,
    data: Vec<u8>,
}
impl SourceWindow<'_> {
    pub fn get(&mut self, id: usize) -> io::Result<&str> {
        let file = self
            .pin
            .files
            .get(id)
            .and_then(|f| f.bytes.as_ref())
            .ok_or_else(binary::bad)?;
        let source = &self.pin.dir.0[manifest::SOURCE - 1];
        let range = source.start + file.start..source.start + file.end;
        if range.is_empty() {
            return Ok("");
        }
        if range.start < self.start || range.end - self.start > self.data.len() as u64 {
            self.start = range.start;
            self.data.clear();
            let end = range
                .end
                .max(range.start.saturating_add(256 * 1024))
                .min(source.end);
            self.pin
                .segment
                .visit(range.start..end, |bytes| self.data.write_all(bytes))?;
        }
        std::str::from_utf8(
            &self.data[(range.start - self.start) as usize..(range.end - self.start) as usize],
        )
        .map_err(io::Error::other)
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
mod publication_tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn writer_process() {
        if let Some(cwd) = env::var_os("CODEINDEX_TEST_WRITER_CWD") {
            Pin::refresh(Path::new(&cwd)).unwrap();
        }
    }

    struct Fixture {
        home: PathBuf,
        root: PathBuf,
        cache: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let home = env::temp_dir().join(format!(
                "codeindex-publication-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&home).unwrap();
            let root = home.join("repo");
            fs::create_dir_all(root.join(".git")).unwrap();
            let root = root.canonicalize().unwrap();
            let cache = home
                .join(".codeindex/repos")
                .join(repository::cache_key(&root));
            fs::write(root.join("a.md"), "old").unwrap();
            Self { home, root, cache }
        }
        fn writer(&self, stage: Option<Publication>) -> Command {
            let mut cmd = Command::new(env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "adapters::snapshot::store::publication_tests::writer_process",
            ])
            .env("HOME", &self.home)
            .env("PATH", "")
            .env("CODEINDEX_TEST_WRITER_CWD", &self.root)
            .env_remove("CODEINDEX_TEST_CRASH_STAGE");
            if let Some(stage) = stage {
                cmd.env("CODEINDEX_TEST_CRASH_STAGE", format!("{stage:?}"));
            }
            cmd
        }
        fn pin(&self) -> Option<Pin> {
            Pin::open(&self.cache, &self.root).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn script_graph_reuse_preserves_names_coordinates_and_targets() {
        let repo = Fixture::new();
        let declarations: String = (0..160)
            .map(|n| format!("def helper{n}():\n    return 1\n"))
            .collect();
        fs::write(repo.root.join("helpers.py"), &declarations).unwrap();
        let path = repo.root.join("caller.py");
        let caller = "from helpers import helper1, helper2\nhelper1()\n";
        fs::write(&path, caller).unwrap();
        assert!(repo.writer(None).status().unwrap().success());
        let baseline = repo.pin().unwrap();
        let edit = format!("{caller}# new trailing comment\n");
        fs::write(&path, &edit).unwrap();
        assert!(repo.writer(None).status().unwrap().success());
        let next = repo.pin().unwrap();
        for product in [manifest::SYMBOLS, manifest::REL] {
            let range = &next.dir.0[product - 1];
            let mut begin = 0;
            for row in next.segment.map.encode().chunks_exact(24) {
                let end = u64::from_le_bytes(row[..8].try_into().unwrap());
                if begin < range.end && range.start < end {
                    assert_ne!(u64::from_le_bytes(row[8..16].try_into().unwrap()), next.segment_id);
                }
                begin = end;
            }
            assert_eq!(next.product(product).unwrap(), baseline.product(product).unwrap());
        }

        // Same-length call-site rename, then shifted coordinates. Both must
        // produce exactly the graph a new index would build for the edited tree.
        for changed in [edit.replace("helper1()", "helper2()"), format!("\n{caller}")] {
            fs::write(&path, &changed).unwrap();
            assert!(repo.writer(None).status().unwrap().success());
            let actual = repo.pin().unwrap();
            let fresh = Fixture::new();
            fs::write(fresh.root.join("helpers.py"), &declarations).unwrap();
            fs::write(fresh.root.join("caller.py"), &changed).unwrap();
            assert!(fresh.writer(None).status().unwrap().success());
            let expected = fresh.pin().unwrap();
            for product in [manifest::SYMBOLS, manifest::REL] {
                assert_eq!(actual.product(product).unwrap(), expected.product(product).unwrap());
            }
        }
    }

    #[test]
    fn selective_migration_process() {
        let Some(root) = env::var_os("CX_TEST_MIGRATION_ROOT") else {
            return;
        };
        let id: u16 = env::var("CX_TEST_MIGRATION_RULE").unwrap().parse().unwrap();
        let cwd = Path::new(&root);
        let mut previous = Pin::refresh(cwd).unwrap();
        let old_id = previous.segment_id;
        let products: Vec<_> = (1..=manifest::N)
            .map(|p| previous.product(p).unwrap())
            .collect();
        previous.migration = super::super::registry::fixture_upgrade(id);
        let next = Pin::refresh_with_prior(cwd, Some(previous)).unwrap();
        assert_eq!(next.generation, 2);
        let affected = super::super::registry::fixture_upgrade(id);
        let spans = next.segment.map.encode();
        for kind in [
            manifest::SOURCE,
            manifest::PATHGRAM,
            manifest::CONTENT,
            manifest::RANK,
            manifest::SYMBOLS,
            manifest::REL,
            manifest::MD,
        ] {
            assert_eq!(
                next.product(kind).unwrap(),
                products[kind - 1],
                "fresh product {kind} for migration {id}"
            );
            let range = &next.dir.0[kind - 1];
            let mut seen_new = false;
            let mut start = 0;
            for row in spans.chunks_exact(24) {
                let end = u64::from_le_bytes(row[..8].try_into().unwrap());
                let key = u64::from_le_bytes(row[8..16].try_into().unwrap());
                if start < range.end && range.start < end && key != old_id {
                    seen_new = true
                }
                start = end;
            }
            assert_eq!(
                seen_new,
                affected.requires_rebuild(kind),
                "physical ownership for rule {id}, product {kind}"
            );
        }
    }

    #[test]
    fn producer_migrations_preserve_unrelated_product_bytes() {
        for rule in [3, 4, 6, 8] {
            let f = Fixture::new();
            fs::write(
                f.root.join("a.md"),
                "# Setup\n[other](b.md#help)\nbody needle",
            )
            .unwrap();
            fs::write(f.root.join("b.md"), "## Help\nneedle").unwrap();
            let code = (0..32)
                .map(|n| format!("fn caller{n}() {{ target(); }}\n"))
                .collect::<String>();
            fs::write(f.root.join("x.rs"), format!("fn target() {{}}\n{code}")).unwrap();
            for n in 0..16 {
                fs::write(
                    f.root.join(format!("guide-{n}.md")),
                    format!("# Chapter {n}\n[Setup](a.md#setup)"),
                )
                .unwrap();
            }
            let out = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "adapters::snapshot::store::publication_tests::selective_migration_process",
                    "--nocapture",
                ])
                .env("HOME", &f.home)
                .env("PATH", "")
                .env("CX_TEST_MIGRATION_ROOT", &f.root)
                .env("CX_TEST_MIGRATION_RULE", rule.to_string())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn recovery_preserves_all_referenced_parts_across_reader_generations() {
        let f = Fixture::new();
        let body = "retained source and evidence\n".repeat(1024);
        fs::write(f.root.join("b.txt"), &body).unwrap();
        assert!(f.writer(None).output().unwrap().status.success());
        let first = f.pin().unwrap();
        fs::write(f.root.join("a.md"), "new text").unwrap();
        assert_eq!(
            f.writer(Some(Publication::Replaced))
                .output()
                .unwrap()
                .status
                .code(),
            Some(86)
        );
        assert!(f.writer(None).output().unwrap().status.success());
        let second = f.pin().unwrap();
        assert!(second.segment.keys().count() > 1);
        assert_eq!(second.source(1).unwrap(), body);
        fs::remove_file(f.root.join("b.txt")).unwrap();
        assert!(f.writer(None).output().unwrap().status.success());
        let latest = f.pin().unwrap();
        assert_eq!(latest.files.len(), 1);
        assert_eq!(first.source(0).unwrap(), "old");
        assert_eq!(second.source(1).unwrap(), body);
    }

    #[test]
    fn metadata_only_crash_keeps_shared_segment_and_old_pin() {
        for stage in [
            Publication::SegmentDurable,
            Publication::ManifestDurable,
            Publication::Replaced,
            Publication::SelectorDurable,
        ] {
            let fixture = Fixture::new();
            assert!(fixture.writer(None).output().unwrap().status.success());
            let old = fixture.pin().unwrap();
            // Rename an identical file over the original so the stamp must change.
            fs::write(fixture.root.join("replacement"), "old").unwrap();
            fs::rename(fixture.root.join("replacement"), fixture.root.join("a.md")).unwrap();
            assert_eq!(
                fixture.writer(Some(stage)).output().unwrap().status.code(),
                Some(86)
            );
            assert!(fixture.writer(None).output().unwrap().status.success());
            let promoted = fixture.pin().unwrap();
            assert_eq!((promoted.generation, promoted.segment_id), (2, 1));
            assert_eq!(promoted.source(0).unwrap(), "old");
            fs::write(fixture.root.join("a.md"), "new").unwrap();
            assert!(fixture.writer(None).output().unwrap().status.success());
            assert!(!fixture.cache.join(segment_name(1)).exists());
            assert_eq!(old.source(0).unwrap(), "old");
            assert_eq!(promoted.source(0).unwrap(), "old");
        }
    }

    #[test]
    fn crash_before_and_after_switch_with_cold_and_existing_cache() {
        for had_old in [false, true] {
            for stage in [
                Publication::SegmentDurable,
                Publication::ManifestDurable,
                Publication::Replaced,
                Publication::SelectorDurable,
            ] {
                let f = Fixture::new();
                if had_old {
                    assert!(f.writer(None).output().unwrap().status.success());
                }
                let old = f.pin();
                fs::write(f.root.join("a.md"), "new document").unwrap();
                let failed = f.writer(Some(stage)).output().unwrap();
                assert_eq!(
                    failed.status.code(),
                    Some(86),
                    "{}",
                    String::from_utf8_lossy(&failed.stderr)
                );
                assert!(f.cache.join(PUBLISHING).exists());
                let switched =
                    matches!(stage, Publication::Replaced | Publication::SelectorDurable);
                let selected = f.pin();
                if switched {
                    assert_eq!(selected.unwrap().source(0).unwrap(), "new document")
                } else if had_old {
                    assert_eq!(selected.unwrap().source(0).unwrap(), "old")
                } else {
                    assert!(selected.is_none())
                }
                // Two competing recoveries must converge on a single complete successor.
                let first = f
                    .writer(None)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                let second = f
                    .writer(None)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                assert!(first.wait_with_output().unwrap().status.success());
                assert!(second.wait_with_output().unwrap().status.success());
                let new = f.pin().unwrap();
                assert_eq!(new.generation, if had_old { 2 } else { 1 });
                assert_eq!(new.source(0).unwrap(), "new document");
                let mut files: Vec<_> = fs::read_dir(&f.cache)
                    .unwrap()
                    .map(|e| e.unwrap().file_name().into_string().unwrap())
                    .collect();
                files.sort();
                assert_eq!(
                    files,
                    [
                        "current".into(),
                        segment_name(new.generation),
                        "refresh.lock".into()
                    ]
                );
                if let Some(pin) = old {
                    assert_eq!(pin.source(0).unwrap(), "old");
                }
            }
        }
    }
}
