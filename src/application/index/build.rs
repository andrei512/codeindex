use super::{Entry, Observation, ObservedPath, graph};
use crate::{
    application::index::layout::*,
    domain::{
        BlobId,
        code::{self, Analysis, ForwardNames, Language, Unit},
        markdown::{self, Parsed},
    },
};
use std::{io, path::Path};

mod binding_reuse;
mod c_project;
mod capture;
mod catalog;
mod content_delta;
mod evidence_batches;
mod fresh_postings;
mod graph_reuse;
use graph_reuse::GraphReuse;
mod incremental;
#[cfg(test)]
mod incremental_tests;
mod markdown_delta;
mod path_order;
pub(crate) use path_order::{PathOrder, PriorRow};
mod retained;
mod script_edit;
mod source;
use source::{Pool, Source};
mod local;
mod local_job;
use local::Facts;
pub(crate) use local::Local;

fn analyze(path: &str, body: Option<&str>) -> Local {
    Facts::All.read(path, body, None)
}

struct Captured {
    blob: Option<crate::domain::BlobId>,
    source: Option<Source>,
    local: Option<Local>,
    prior: Option<usize>,
}
mod capture_table;
mod source_capture;
use capture_table::{CaptureTable, Work};
use source_capture::SourceCapture;
type Identify<'a> = dyn Fn(
        usize,
        &ObservedPath,
        Option<&str>,
        PriorRow,
    ) -> io::Result<(Option<usize>, Option<crate::domain::BlobId>)>
    + Sync
    + 'a;
pub(crate) trait Previous: Sync {
    fn compilation_database(&self) -> Option<usize> {
        self.entries()
            .iter()
            .position(|f| code::compilation_database(f.path()))
    }
    fn compatible(&self, _kind: usize) -> bool {
        true
    }
    fn entries(&self) -> &[Entry];
    fn extent(&self, product: usize) -> std::ops::Range<u64>;
    fn copy_bytes(&self, range: std::ops::Range<u64>, out: &mut dyn io::Write) -> io::Result<()>;
    fn same_bytes(&self, range: std::ops::Range<u64>, expected: &[u8]) -> io::Result<bool>;
    fn local(&self, id: usize, decode_code: bool, decode_markdown: bool) -> io::Result<Local>;
    fn markdown_record(&self, _id: usize) -> io::Result<Option<std::ops::Range<u64>>> {
        Ok(None)
    }
    fn source_end_word(&self, _file: usize, _end: usize) -> io::Result<Option<u64>> {
        Ok(None)
    }
    fn graph_reader(
        &self,
        kind: usize,
    ) -> io::Result<Box<dyn crate::domain::content_lookup::ExtentRead + '_>> {
        Ok(Box::new(std::io::Cursor::new(self.product(kind)?)))
    }
    fn product(&self, kind: usize) -> io::Result<Vec<u8>>;
}
mod worker;
pub(crate) use worker::Worker;
pub(crate) type RankLane =
    Box<dyn FnOnce(crate::domain::ranked::FreshRank) -> io::Result<Worker<u64>> + Send>;

pub(crate) type ProvenanceLane<'a> =
    Box<dyn FnOnce(&[ObservedPath], &[Option<BlobId>]) + Send + 'a>;

pub(crate) struct Output {
    pub(crate) paths: PathOrder,
    pub(crate) dir: Directory,
    pub(crate) files: Vec<Entry>,
    pub(crate) rank_write: Option<Worker<u64>>,
    pub(crate) retired: Vec<Local>,
    pub(crate) sdk_catalog: Option<usize>,
    pub(crate) sdk_receipts: Vec<code::CReceipt>,
}
/// Facts already checked against this predecessor during refresh. Rows before
/// the prefix have both the same path and verified-identical input bytes.
#[derive(Default, Clone, Copy)]
pub(crate) struct Predecessor<'a> {
    pub previous: Option<&'a dyn Previous>,
    pub forward_known: bool,
    pub verified_prefix: usize,
}

/// The repository supplies the observation and identity callbacks as one capture source.
pub(crate) struct Inputs<'a> {
    pub root: &'a Path,
    pub predecessor: Predecessor<'a>,
    pub read: &'a source_capture::Reader<'a>,
    pub identify: &'a Identify<'a>,
}
#[derive(Default)]
pub(crate) struct Lanes<'a> {
    pub rank: Option<RankLane>,
    pub provenance: Option<ProvenanceLane<'a>>,
}
impl Inputs<'_> {
    pub fn build(
        self,
        obs: &mut [ObservedPath],
        w: &mut impl ExtentSink,
        lanes: Lanes<'_>,
    ) -> io::Result<Output> {
        self.with_allowance(obs, w, lanes, source::source_allowance())
    }
    fn with_allowance(
        self,
        obs: &mut [ObservedPath],
        w: &mut impl ExtentSink,
        lanes: Lanes<'_>,
        allowance: usize,
    ) -> io::Result<Output> {
        let captured = self.capture(obs, allowance)?;
        captured.seal(self.root, obs, w, lanes)
    }
}

mod product_writer;
use product_writer::{CatalogWriter, GraphInputs, GraphPlan, ProductPlan, ProductReuse};

/// Retained sources and concurrent capture jobs share this build's allowance.
struct CaptureBudget {
    width: usize,
    capture_credit: usize,
    pool: Pool,
}

impl CaptureBudget {
    fn new(allowance: usize, files: usize) -> Self {
        let width = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(files.max(1));
        let retained = allowance / 2;
        let capture_credit = allowance.saturating_sub(retained).max(1);
        Self {
            width,
            capture_credit,
            pool: Pool::new(retained),
        }
    }
}

/// Capture reads, predecessor rows and limits belong to one verified path order.
struct CapturePlan<'a> {
    inputs: &'a Inputs<'a>,
    paths: PathOrder,
    reuse: Vec<Option<usize>>,
    config: c_project::Refresh,
    reads: CaptureReads,
    budget: CaptureBudget,
}

impl<'a> CapturePlan<'a> {
    fn new(inputs: &'a Inputs<'a>, obs: &[ObservedPath], allowance: usize) -> io::Result<Self> {
        let predecessor = inputs.predecessor;
        let (paths, reuse) =
            PathOrder::compare(predecessor.previous, obs, predecessor.verified_prefix);
        let config = c_project::Refresh::plan(predecessor.previous, obs, &reuse, &paths)?;
        let reads = CaptureReads::plan(predecessor, &paths);
        Ok(Self {
            inputs,
            paths,
            reuse,
            config,
            reads,
            budget: CaptureBudget::new(allowance, obs.len()),
        })
    }
}

/// These product requirements determine which retained rows need source text
/// or decoded Markdown during capture.
struct CaptureReads {
    searchable: incremental::Compatibility,
    forward_known: bool,
    share_markdown: bool,
    decode_markdown: bool,
    refresh_markdown: bool,
}

impl CaptureReads {
    fn plan(predecessor: Predecessor<'_>, paths: &PathOrder) -> Self {
        let reuse_markdown = predecessor.previous.is_some_and(|p| p.compatible(MD));
        let searchable = incremental::Compatibility::inspect(predecessor.previous);
        let rewrite_rank = predecessor.previous.is_some() && !searchable.rank;
        let rewrite_content = predecessor.previous.is_some() && !searchable.content;
        let share_markdown = paths.stable() && reuse_markdown;
        Self {
            searchable,
            forward_known: predecessor.forward_known && !rewrite_rank && !rewrite_content,
            share_markdown,
            decode_markdown: reuse_markdown && (!share_markdown || rewrite_rank),
            refresh_markdown: !reuse_markdown,
        }
    }
}

struct DeferredLocals<'a> {
    root: &'a Path,
    obs: &'a [ObservedPath],
    sources: &'a [Option<Source>],
    backing: Option<&'a source::Mapping>,
    previous: Option<&'a dyn Previous>,
    reuse: &'a [Option<usize>],
    width: usize,
    capture_credit: usize,
}

impl DeferredLocals<'_> {
    fn complete(
        &self,
        config: &mut c_project::Refresh,
        mut parsed: Vec<Option<Local>>,
        evidence_reuse: &mut std::borrow::Cow<'_, [Option<usize>]>,
    ) -> io::Result<Vec<Local>> {
        self.invalidate_evidence(config, &parsed, evidence_reuse);
        if config.rewrite_catalog() || parsed.iter().any(Option::is_none) {
            self.resolve(config, &mut parsed, evidence_reuse)?;
        }
        Ok(parsed.into_iter().map(|row| row.expect("completed parser row")).collect())
    }

    fn resolve(
        &self,
        config: &mut c_project::Refresh,
        parsed: &mut [Option<Local>],
        evidence_reuse: &mut std::borrow::Cow<'_, [Option<usize>]>,
    ) -> io::Result<()> {
        let project = config.prepare(self.root, self.obs, self.sources, self.backing);
        if config.refine(project.as_ref()) {
            self.restore_locals(config, parsed, evidence_reuse)?;
        }
        let mut pending = capture::Jobs::new(self.capture_credit);
        for (id, slot) in parsed
            .iter_mut()
            .enumerate()
            .filter(|(_, slot)| slot.is_none())
        {
            let job = local_job::Job::new(
                slot,
                &self.obs[id].path,
                self.sources[id].as_ref().map(|s| s.text(self.backing)),
                project.as_ref(),
            );
            pending.push(self.obs[id].stamp[2], job);
        }
        pending.run_steps(self.width, |mut job| {
            Ok((!job.advance(project.as_ref())).then_some(job))
        })?;
        config.save_observations(project, self.obs, parsed, evidence_reuse);
        Ok(())
    }

    fn invalidate_evidence(
        &self,
        config: &c_project::Refresh,
        parsed: &[Option<Local>],
        evidence_reuse: &mut std::borrow::Cow<'_, [Option<usize>]>,
    ) {
        if !config.invalidates_graph() && config.catalog.is_none() {
            return;
        }
        for (i, local) in parsed.iter().enumerate() {
            let missing_retained_local = local.is_none() && evidence_reuse[i].is_some();
            if missing_retained_local && config.observes(&self.obs[i].path) {
                evidence_reuse.to_mut()[i] = None;
            }
        }
    }

    fn restore_locals(
        &self,
        config: &c_project::Refresh,
        parsed: &mut [Option<Local>],
        evidence_reuse: &mut std::borrow::Cow<'_, [Option<usize>]>,
    ) -> io::Result<()> {
        for (id, slot) in parsed.iter_mut().enumerate() {
            if slot.is_none()
                && let Some(old) = self.reuse[id]
                && !config.requires_analysis(&self.obs[id].path, Some(old))
            {
                *slot = Some(
                    self.previous
                        .expect("retained source")
                        .local(old, true, true)?,
                );
                evidence_reuse.to_mut()[id] = Some(old);
            }
        }
        Ok(())
    }
}

struct CapturedProject<'a> {
    table: CaptureTable,
    backing: Option<source::Mapping>,
    paths: PathOrder,
    previous: Option<&'a dyn Previous>,
    graph: GraphReuse<'a>,
    config: c_project::Refresh,
    reads: CaptureReads,
    width: usize,
    capture_credit: usize,
}
impl<'a> Inputs<'a> {
    fn capture(
        &self,
        obs: &mut [ObservedPath],
        allowance: usize,
    ) -> io::Result<CapturedProject<'a>> {
        let plan = CapturePlan::new(self, obs, allowance)?;
        let mut graph = GraphReuse::predict(
            self.predecessor.previous,
            plan.paths.stable() && !plan.config.invalidates_graph(),
            obs,
            &plan.reuse,
        );
        let mut table = CaptureTable::new(obs.len());
        graph.prepare(&plan, obs, &mut table)?;
        let capture = SourceCapture::new(&plan, graph.retain_during_capture());
        capture.run(obs, &mut table, graph.prepared_row())?;
        let CapturePlan {
            paths,
            config,
            reads,
            budget,
            ..
        } = plan;
        let CaptureBudget {
            width,
            capture_credit,
            pool,
        } = budget;
        Ok(CapturedProject {
            table,
            backing: pool.seal()?,
            paths,
            previous: self.predecessor.previous,
            graph,
            config,
            reads,
            width,
            capture_credit,
        })
    }
}
impl CapturedProject<'_> {
    fn seal(
        mut self,
        root: &Path,
        obs: &[ObservedPath],
        w: &mut impl ExtentSink,
        lanes: Lanes<'_>,
    ) -> io::Result<Output> {
        let previous = self.previous;
        let table = &mut self.table;
        let reuse = self.paths.at_same_path(&table.retained_rows);
        let retained_graph = self.graph.verify(obs, &reuse)?;
        let mut evidence_reuse = std::borrow::Cow::Borrowed(table.retained_rows.as_slice());
        let deferred = DeferredLocals {
            root,
            obs,
            sources: &table.sources,
            backing: self.backing.as_ref(),
            previous,
            reuse: &reuse,
            width: self.width,
            capture_credit: self.capture_credit,
        };
        let locals = deferred.complete(
            &mut self.config,
            std::mem::take(&mut table.parsed),
            &mut evidence_reuse,
        )?;
        let units = captured_units(obs, &locals, &table.sources, self.backing.as_ref());
        let before = incremental::edited_sources(
            previous.filter(|_| self.paths.stable()),
            &reuse,
            self.capture_credit,
            self.graph.take_old_body(),
        )?;
        let graph = self.graph.plan_products(
            retained_graph,
            &self.config,
            GraphInputs { units: &units, before: before.as_ref() },
            &mut evidence_reuse,
        )?;
        let catalog = CatalogWriter {
            previous,
            obs,
            units: &units,
            table,
            locals: &locals,
        };
        let sealed = ProductPlan {
            catalog,
            paths: &self.paths,
            reuse: ProductReuse {
                source: &reuse,
                evidence: &evidence_reuse,
                markdown: self.reads.share_markdown,
            },
            before: before.as_ref(),
            graph,
            searchable: self.reads.searchable,
            evidence_width: (previous.is_none() && self.backing.is_none()).then_some(self.width),
            capture_credit: self.capture_credit,
        }
        .write(w, lanes)?;
        drop(units);
        drop((self.table.sources, self.backing));
        Ok(sealed.finish(self.paths, locals, self.config))
    }
}

fn captured_units<'a>(
    observations: &'a [ObservedPath],
    locals: &'a [Local],
    sources: &'a [Option<Source>],
    backing: Option<&'a source::Mapping>,
) -> Vec<Unit<'a>> {
    observations
        .iter()
        .zip(locals)
        .zip(sources)
        .map(|((observed, local), source)| Unit {
            path: &observed.path,
            text: source.as_ref().map_or("", |source| source.text(backing)),
            analysis: &local.code,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    pub(super) struct MemorySink(pub(super) Vec<u8>);
    impl io::Write for MemorySink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl ExtentSink for MemorySink {
        fn offset(&self) -> u64 {
            self.0.len() as u64
        }
    }

    #[test]
    fn primary_failure_waits_for_borrowed_provenance_and_owned_rank_work() {
        use std::sync::{Arc, atomic::AtomicBool, mpsc};
        struct FailingSink {
            started: mpsc::Receiver<()>,
            release: mpsc::Sender<()>,
        }
        impl io::Write for FailingSink {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                // Both workers are alive before the main catalog fails.
                for _ in 0..2 {
                    self.started
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .expect("product worker started");
                }
                for _ in 0..2 {
                    self.release.send(()).unwrap();
                }
                Err(io::Error::other("primary catalog failed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl ExtentSink for FailingSink {
            fn offset(&self) -> u64 {
                0
            }
        }
        let (started, events) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let proceed = Arc::new(std::sync::Mutex::new(proceed));
        let rank_release = proceed.clone();
        let rank_started = started.clone();
        let rank_done = Arc::new(AtomicBool::new(false));
        let rank_finished = rank_done.clone();
        let provenance_done = AtomicBool::new(false);
        let mut sink = FailingSink {
            started: events,
            release,
        };
        let lanes = Lanes {
            rank: Some(Box::new(move |_| {
                Worker::start(move || {
                    rank_started.send(()).unwrap();
                    rank_release.lock().unwrap().recv().unwrap();
                    rank_finished.store(true, Ordering::Release);
                    Ok(0)
                })
            })),
            provenance: Some(Box::new(|obs, blobs| {
                assert_eq!(obs.len(), blobs.len());
                started.send(()).unwrap();
                proceed.lock().unwrap().recv().unwrap();
                provenance_done.store(true, Ordering::Release);
            })),
        };
        let mut obs = [ObservedPath {
            path: "main.rs".into(),
            stamp: [0; 7],
            captured: Some(Some("fn main() {}".into())),
        }];
        let build = Inputs {
            root: Path::new("."),
            predecessor: Default::default(),
            read: &|_, _, _| unreachable!(),
            identify: &|_, _, _, _| Ok((None, None)),
        };
        let error = build
            .with_allowance(&mut obs, &mut sink, lanes, 4096)
            .err()
            .expect("write should fail");
        assert_eq!(error.to_string(), "primary catalog failed");
        assert!(rank_done.load(Ordering::Acquire));
        assert!(provenance_done.load(Ordering::Acquire));
    }

    #[test]
    fn source_spill_preserves_all_products_and_single_capture() {
        let fixture = [
            (".gitignore", Some("build/\n")),
            (
                "README.md",
                Some("# Café\n## Help\nSee [other](other.md#thing)."),
            ),
            ("blob", None),
            ("empty", Some("")),
            ("main.py", Some("from util import helper\nhelper()\n")),
            ("other.md", Some("# Thing\n💡\n")),
            ("util.py", Some("def helper():\n    return 'hello'\n")),
        ];
        let build = |allowance| {
            let mut obs: Vec<_> = fixture
                .iter()
                .map(|&(path, body)| {
                    let mut stamp = [0; 7];
                    stamp[2] = body.map_or(16, str::len) as u64;
                    ObservedPath {
                        path: path.into(),
                        stamp,
                        captured: (path == ".gitignore").then(|| body.map(String::from)),
                    }
                })
                .collect();
            let counts = fixture.map(|_| AtomicUsize::new(0));
            let read = |i: usize, file: &ObservedPath, prior: Option<usize>| {
                assert_eq!(file.path, fixture[i].0);
                assert!(prior.is_none());
                counts[i].fetch_add(1, Ordering::Relaxed);
                Ok(fixture[i].1.map(String::from))
            };
            let mut sink = MemorySink::default();
            let Output { dir, .. } = Inputs {
                root: Path::new("."),
                read: &read,
                predecessor: Predecessor::default(),
                identify: &|_, _, text, id| {
                    Ok((
                        id.unchanged(),
                        text.map(|s| crate::adapters::repository::raw_blob_id(s.as_bytes(), false)),
                    ))
                },
            }
            .with_allowance(&mut obs, &mut sink, Lanes::default(), allowance)
            .unwrap();
            assert_eq!(counts.map(AtomicUsize::into_inner), [0, 1, 1, 1, 1, 1, 1]);
            (dir.0, sink.0)
        };
        let heap = build(8192);
        assert_eq!(heap, build(1));
        assert_eq!(heap, build(64));
    }
}

mod memory;
pub(crate) use memory::{MemoryBytes, MemoryQuery, memory_index};
