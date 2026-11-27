use super::{CandidateFiles, Lines, Pin, events};
use crate::adapters::{
    query::Outcome,
    repository::search_paths::{SearchPath, SearchRoot},
};
use std::{
    io,
    path::{Path, PathBuf},
    time::Instant,
};

impl Lines<'_> {
    pub(in crate::adapters::query) fn walk_search(
        &self,
        cwd: &Path,
        outcome: &mut Outcome,
        index: Option<&Pin>,
    ) -> io::Result<Vec<u8>> {
        let start = Instant::now();
        let mut batch = Batch::default();
        let operands = if self.paths.is_empty() {
            &["."][..]
        } else {
            &self.paths
        };
        let roots = SearchRoots {
            query: self,
            index,
            candidates: Default::default(),
        };
        for operand in operands {
            if batch.finished(self) {
                break;
            }
            roots.run_operand((cwd, operand), operands.len() == 1, &mut batch, outcome)?;
        }
        Ok(batch.finish(self, cwd, start, outcome))
    }

    fn warn_when_unsearched(&self) -> bool {
        let default_search = self.paths.is_empty() && !self.files;
        default_search && !self.quiet
    }

    fn list_file(&self, out: &mut Vec<u8>, operand: &str) -> bool {
        if !self.quiet {
            self.print_path(out, operand, b"\n");
        }
        true
    }

    fn list_operand(
        &self,
        origin: SearchRoot<'_>,
        operand: &str,
        out: &mut Vec<u8>,
        outcome: &mut Outcome,
    ) -> io::Result<bool> {
        if origin.root.is_file() {
            return Ok(self.list_file(out, operand));
        }
        if self.quiet {
            let mut hit = false;
            let timestamp = self.scope.sort.timestamp();
            self.visit_until(origin, outcome, |_, source| {
                timestamp.map(|_| source.get()).transpose()?;
                hit = true;
                Ok(true)
            })?;
            return Ok(hit);
        }
        let names = self.ordered_paths(origin, outcome, |name, _| Ok(name.into_owned()))?;
        let prefix = (!self.paths.is_empty()).then_some(operand);
        let hit = !names.is_empty();
        let mut display = PathBuf::new();
        for name in names {
            self.print_path(out, display_name(prefix, &name, &mut display), b"\n");
        }
        Ok(hit)
    }
}

#[derive(Default)]
struct Batch {
    out: Vec<u8>,
    stats: events::Stats,
    searched: usize,
    diagnostics: String,
    matched: bool,
}

impl Batch {
    fn finished(&self, query: &Lines) -> bool {
        self.matched && query.stops_at_first_hit()
    }

    fn finish(self, query: &Lines, cwd: &Path, start: Instant, outcome: &mut Outcome) -> Vec<u8> {
        let Batch {
            stats,
            mut out,
            matched,
            searched,
            diagnostics,
        } = self;
        outcome.diagnostics.push_str(&diagnostics);
        query.summary(&stats, start, &mut out);
        query.finish(outcome, matched);
        if query.no_messages {
            outcome.diagnostics.clear();
            return out;
        }
        if searched == 0 && query.warn_when_unsearched() {
            outcome.diagnostics.push_str(crate::adapters::cli::name());
            outcome.diagnostics.push_str(": No files were searched, which means ripgrep probably applied a filter you didn't expect.\nRunning with --debug will show why files are being skipped.\n");
        }
        outcome.diagnostics.insert_str(0, query.walk.ignore_file_warnings(cwd));
        out
    }

    fn append(&mut self, part: Self) {
        if self.out.is_empty() {
            self.out = part.out;
        } else {
            self.out.extend(part.out);
        }
        self.diagnostics.push_str(&part.diagnostics);
        self.stats.add(&part.stats);
        self.searched += part.searched;
        self.matched |= part.matched;
    }
}

use crate::adapters::repository::CaptureRoot;
use std::{borrow::Cow, sync::OnceLock};

/// All directory operands in one invocation share the same indexed candidates.
struct SearchRoots<'a, 'q> {
    query: &'a Lines<'q>,
    index: Option<&'a Pin>,
    candidates: CandidateCache,
}

impl<'q> SearchRoots<'_, 'q> {
    fn run_operand(
        &self,
        (cwd, operand): (&Path, &str),
        single: bool,
        batch: &mut Batch,
        outcome: &mut Outcome,
    ) -> io::Result<()> {
        let Some(path) = self.query.operand(cwd, operand, outcome) else {
            return Ok(());
        };
        let origin = SearchRoot { root: &path, cwd, operand: Path::new(operand) };
        if self.query.files {
            batch.matched |= self.query.list_operand(origin, operand, &mut batch.out, outcome)?;
            return Ok(());
        }
        self.input(origin, single)?.search_operand(operand, batch, outcome)
    }

    fn input<'s>(
        &'s self,
        origin: SearchRoot<'s>,
        only_operand: bool,
    ) -> io::Result<FileInput<'s, 'q>> {
        let path = origin.root;
        let file = path.is_file();
        let single = file && only_operand;
        let index = self.index.and_then(|pin| IndexedRoot::new(pin, path));
        let capture = if file || self.query.walk.follow {
            None
        } else {
            Some(CaptureRoot::new(path)?)
        };
        Ok(FileInput {
            query: self.query,
            origin,
            index,
            candidates: &self.candidates,
            capture,
            single,
            file,
        })
    }
}

/// Walks, transformed inputs and stale files can finish without reading a posting.
#[derive(Default)]
struct CandidateCache(OnceLock<io::Result<Option<CandidateFiles>>>);

impl CandidateCache {
    fn admits(&self, query: &Lines, pin: &Pin, id: usize, single: bool) -> io::Result<bool> {
        if pin.is_memory() { return Ok(true); }
        if single {
            return query.candidate_file(pin, id);
        }
        match self.0.get_or_init(|| query.candidate_ids(pin)) {
            Ok(ids) => Ok(ids.as_ref().is_none_or(|ids| ids.contains(id))),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    }
}

/// Map a walker's operand-relative names directly into the snapshot namespace.
struct IndexedRoot<'a> {
    pin: &'a Pin,
    relation: RootRelation<'a>,
}

enum RootRelation<'a> {
    Inside(&'a Path),
    Above(&'a Path),
}

impl<'a> IndexedRoot<'a> {
    fn new(pin: &'a Pin, operand: &'a Path) -> Option<Self> {
        let relation = match operand.strip_prefix(&pin.root) {
            Ok(prefix) => RootRelation::Inside(prefix),
            Err(_) => RootRelation::Above(pin.root.strip_prefix(operand).ok()?),
        };
        Some(Self { pin, relation })
    }

    fn locate(&self, entry: Option<&SearchPath>, buffer: &mut PathBuf) -> Option<usize> {
        let child = entry.map(|e| Path::new(&e.name));
        let relative = match self.relation {
            RootRelation::Inside(prefix) => child_path(prefix, child, buffer),
            RootRelation::Above(prefix) => child?.strip_prefix(prefix).ok()?,
        };
        let name = relative.to_str()?;
        let id = self.pin.files.binary_search_by(|f| f.path().cmp(name)).ok()?;
        let indexed = &self.pin.files[id];
        (indexed.bytes.is_some() && entry.is_none_or(|e| e.stamp == indexed.stamp())).then_some(id)
    }
}

fn child_path<'a>(prefix: &'a Path, child: Option<&'a Path>, buffer: &'a mut PathBuf) -> &'a Path {
    let Some(child) = child else { return prefix };
    if prefix.as_os_str().is_empty() {
        return child;
    }
    buffer.clear();
    buffer.push(prefix);
    buffer.push(child);
    buffer.as_path()
}

/// The caller's operand determines the capture directory and explicit-file semantics.
struct FileInput<'a, 'q> {
    query: &'a Lines<'q>,
    origin: SearchRoot<'a>,
    index: Option<IndexedRoot<'a>>,
    candidates: &'a CandidateCache,
    capture: Option<CaptureRoot<'a>>,
    single: bool,
    file: bool,
}

impl FileInput<'_, '_> {
    fn search_operand(
        &self,
        operand: &str,
        batch: &mut Batch,
        outcome: &mut Outcome,
    ) -> io::Result<()> {
        if self.file {
            self.scan(operand, None, &mut PathBuf::new(), batch)?;
            return Ok(());
        }
        let prefix = (!self.query.paths.is_empty()).then_some(operand);
        if self.query.stops_at_first_hit() {
            let mut child = PathBuf::new();
            let mut display = PathBuf::new();
            let mut answer = Ok(false);
            self.query
                .visit_until(self.origin, outcome, |name, metadata| {
                    let entry = SearchPath::new(name, metadata.get()?);
                    let name = display_name(prefix, &entry.name, &mut display);
                    answer = self.scan(name, Some(&entry), &mut child, batch);
                    Ok(!matches!(answer, Ok(false)))
                })?;
            answer?;
            return Ok(());
        }
        let entries = self
            .query
            .ordered_paths(self.origin, outcome, |name, source| {
                Ok(SearchPath::new(name, source.get()?))
            })?;
        self.directory(prefix, &entries, batch)
    }

    fn bytes(
        &self,
        name: &str,
        entry: Option<&SearchPath>,
        child: &mut PathBuf,
        diagnostics: &mut String,
    ) -> io::Result<Option<Cow<'_, [u8]>>> {
        let relative = entry.map(|e| Path::new(&e.name));
        if let Some(reader) = self.query.transform.reader(name) {
            let path = child_path(self.origin.root, relative, child);
            return Ok(Some(Cow::Owned(reader.read(path, self.origin.cwd)?)));
        }
        if let Some(index) = &self.index
            && let Some(id) = index.locate(entry, child)
        {
            return self.indexed_bytes(index.pin, id);
        }
        let bytes = match (&self.capture, entry) {
            (Some(root), Some(entry)) => root.bytes(&entry.name, entry.stamp),
            _ => std::fs::read(child_path(self.origin.root, relative, child)).map(Some),
        };
        Ok(source_bytes(bytes, name, diagnostics).map(Cow::Owned))
    }

    fn indexed_bytes<'p>(&self, pin: &'p Pin, id: usize) -> io::Result<Option<Cow<'p, [u8]>>> {
        if !self.candidates.admits(self.query, pin, id, self.single)? {
            return Ok(None);
        }
        Ok(Some(match pin.source_text(id)? {
            Cow::Borrowed(text) => Cow::Borrowed(text.as_bytes()),
            Cow::Owned(text) => Cow::Owned(text.into_bytes()),
        }))
    }

    fn scan(
        &self,
        name: &str,
        entry: Option<&SearchPath>,
        child: &mut PathBuf,
        batch: &mut Batch,
    ) -> io::Result<bool> {
        batch.searched += 1;
        let hit = self.bytes(name, entry, child, &mut batch.diagnostics)?
            .is_some_and(|bytes| {
                self.query.scan_input(
                    super::operands::Operand {
                        name,
                        show_path: !self.single,
                        explicit: self.file,
                    },
                    &bytes,
                    &mut batch.out,
                    &mut batch.stats,
                )
            });
        batch.matched |= hit;
        Ok(hit)
    }

    fn chunk(
        &self,
        prefix: Option<&str>,
        entries: &[SearchPath],
        batch: &mut Batch,
    ) -> io::Result<()> {
        let mut child = PathBuf::new();
        let mut display = PathBuf::new();
        for entry in entries {
            let name = display_name(prefix, &entry.name, &mut display);
            self.scan(name, Some(entry), &mut child, batch)?;
        }
        Ok(())
    }

    fn directory(
        &self,
        prefix: Option<&str>,
        entries: &[SearchPath],
        batch: &mut Batch,
    ) -> io::Result<()> {
        let q = self.query;
        let workers = q.threads.min(entries.len());
        if workers <= 1 || q.separates_file_output() {
            return self.chunk(prefix, entries, batch);
        }
        std::thread::scope(|scope| {
            let jobs: Vec<_> = entries
                .chunks(entries.len().div_ceil(workers))
                .map(|chunk| {
                    scope.spawn(move || {
                        let mut part = Batch::default();
                        self.chunk(prefix, chunk, &mut part)?;
                        Ok::<_, io::Error>(part)
                    })
                })
                .collect();
            for job in jobs {
                batch.append(
                    job.join()
                        .map_err(|_| io::Error::other("search worker panicked"))??,
                );
            }
            Ok(())
        })
    }
}

fn source_bytes(
    read: io::Result<Option<Vec<u8>>>,
    name: &str,
    diagnostics: &mut String,
) -> Option<Vec<u8>> {
    use std::fmt::Write;
    match read {
        Ok(bytes) => bytes,
        Err(error) => {
            writeln!(diagnostics, "cx: {name}: {error}").unwrap();
            None
        }
    }
}

fn display_name<'a>(operand: Option<&str>, relative: &'a str, buffer: &'a mut PathBuf) -> &'a str {
    let Some(prefix) = operand else {
        return relative;
    };
    buffer.clear();
    buffer.push(prefix);
    buffer.push(relative);
    buffer.to_str().expect("both path components are UTF-8")
}
