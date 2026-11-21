use super::{
    Entry, Observation, ObservedPath,
    binary::{put, string},
    graph,
};
use crate::{
    application::index::layout::*,
    domain::{
        code::{self, Analysis, Language, Names, Unit},
        markdown::{self, Parsed},
    },
};
use std::{collections::BTreeMap, io, path::Path};

mod content_delta;
mod fresh_postings;
mod incremental;
#[cfg(test)]
mod incremental_tests;
mod markdown_delta;
mod retained;
mod script_edit;
mod source;
use source::{Pool, Source};
pub(crate) struct Local {
    pub(crate) code: Analysis,
    pub(crate) markdown: Option<Parsed>,
    pub(crate) lf: Vec<usize>,
    pub(crate) partial_line: bool,
}
fn analyze(path: &str, body: Option<&str>) -> Local {
    let text = body.unwrap_or("");
    let code = Language::for_path(path)
        .filter(|_| body.is_some())
        .map_or_else(Analysis::default, |l| code::analyze(l, text));
    let markdown =
        (body.is_some() && markdown::is_markdown(path)).then(|| Parsed::parse(path, text));
    let mut lf: Vec<_> = text.match_indices('\n').map(|(i, _)| i + 1).collect();
    let partial_line = !text.is_empty() && !text.ends_with('\n');
    if partial_line {
        lf.push(text.len());
    }
    Local {
        code,
        markdown,
        lf,
        partial_line,
    }
}

struct Captured {
    blob: Option<crate::domain::BlobId>,
    source: Option<Source>,
    local: Option<Local>,
    prior: Option<usize>,
}
type Slot = Option<Captured>;
type Identify<'a> = dyn Fn(
        usize,
        &ObservedPath,
        Option<&str>,
        Option<usize>,
    ) -> io::Result<(Option<usize>, Option<crate::domain::BlobId>)>
    + Sync
    + 'a;
type Job<'a> = (usize, &'a mut ObservedPath, &'a mut Slot, Option<usize>);

struct Queue<'a> {
    jobs: BTreeMap<usize, Vec<Job<'a>>>,
    available: usize,
}
impl<'a> Queue<'a> {
    fn take(&mut self) -> Option<(Job<'a>, usize)> {
        // Select the heaviest job that fits. An oversized job's charge is the
        // entire capture allowance and can only be admitted by itself.
        let cost = *self.jobs.range(..=self.available).next_back()?.0;
        let bucket = self.jobs.get_mut(&cost).unwrap();
        let job = bucket.pop().unwrap();
        if bucket.is_empty() {
            self.jobs.remove(&cost);
        }
        self.available -= cost;
        Some((job, cost))
    }
}
pub(crate) trait Previous: Sync {
    fn compatible(&self, _kind: usize) -> bool {
        true
    }
    fn entries(&self) -> &[Entry];
    fn extent(&self, product: usize) -> std::ops::Range<u64>;
    fn copy_bytes(&self, range: std::ops::Range<u64>, out: &mut dyn io::Write) -> io::Result<()>;
    fn local(&self, id: usize, decode_code: bool, decode_markdown: bool) -> io::Result<Local>;
    fn product(&self, kind: usize) -> io::Result<Vec<u8>>;
}
pub(crate) fn build(
    root: &Path,
    obs: &mut [ObservedPath],
    read: &(impl Fn(&Path, &str, [u64; 7]) -> io::Result<Option<String>> + Sync),
    previous: Option<&dyn Previous>,
    forward_known: bool,
    identify: &Identify,
    w: &mut impl ExtentSink,
) -> io::Result<(Directory, Vec<Entry>)> {
    build_with_allowance(
        root,
        obs,
        read,
        previous,
        forward_known,
        identify,
        w,
        source::source_allowance(),
    )
}
fn build_with_allowance(
    root: &Path,
    obs: &mut [ObservedPath],
    read: &(impl Fn(&Path, &str, [u64; 7]) -> io::Result<Option<String>> + Sync),
    previous: Option<&dyn Previous>,
    forward_known: bool,
    identify: &Identify,
    w: &mut impl ExtentSink,
    allowance: usize,
) -> io::Result<(Directory, Vec<Entry>)> {
    let reuse: Vec<Option<usize>> = obs
        .iter()
        .enumerate()
        .map(|(index, o)| {
            let entries = previous?.entries();
            let id = if entries
                .get(index)
                .is_some_and(|entry| entry.path() == o.path)
            {
                index
            } else {
                entries.binary_search_by(|e| e.path().cmp(&o.path)).ok()?
            };
            (entries[id].stamp() == o.stamp).then_some(id)
        })
        .collect();
    let stable_paths = previous.is_some_and(|p| {
        p.entries().len() == obs.len()
            && p.entries()
                .iter()
                .zip(obs.iter())
                .all(|(entry, file)| entry.path() == file.path)
    });
    let reuse_markdown = previous.is_some_and(|p| p.compatible(MD));
    let rewrite_rank = previous.is_some_and(|p| !p.compatible(RANK));
    let read_all_text = rewrite_rank || previous.is_some_and(|p| !p.compatible(CONTENT));
    let share_markdown = stable_paths && reuse_markdown;
    let keep_graph = incremental::graph_unchanged(previous, obs, &reuse);
    let width = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(obs.len().max(1));
    let retained = allowance / 2;
    let capture_credit = allowance.saturating_sub(retained).max(1);
    let pool = Pool::new(retained);
    let mut slots: Vec<Slot> = (0..obs.len()).map(|_| None).collect();
    let prepared =
        if let Some(previous) = previous.filter(|_| stable_paths && forward_known && !keep_graph) {
            script_edit::prepare(previous, obs, &reuse, &pool, capture_credit, identify)?
        } else {
            None
        };
    let mut old_body = None;
    let mut verified_graph = None;
    if let Some(edit) = prepared {
        verified_graph = Some((edit.id, edit.same_graph));
        slots[edit.id] = Some(edit.capture);
        old_body = Some((edit.id, edit.old_text));
    }
    let keep_graph = keep_graph || verified_graph.is_some_and(|(_, equal)| equal);
    {
        let mut jobs = BTreeMap::<usize, Vec<Job>>::new();
        let direct =
            previous.filter(|_| forward_known && keep_graph && share_markdown && !read_all_text);
        let mut scheduled = 0;
        for (index, ((o, slot), &prior)) in obs.iter_mut().zip(&mut slots).zip(&reuse).enumerate() {
            if slot.is_some() {
                continue;
            }
            if let (Some(p), Some(id)) = (direct, prior) {
                let (prior, blob) = identify(index, o, None, Some(id))?;
                *slot = Some(Captured {
                    blob,
                    source: None,
                    local: Some(p.local(id, false, false)?),
                    prior,
                });
                continue;
            }
            scheduled += 1;
            let cost = usize::try_from(o.stamp[2])
                .unwrap_or(usize::MAX)
                .clamp(1, capture_credit);
            jobs.entry(cost).or_default().push((index, o, slot, prior));
        }
        let state = std::sync::Mutex::new(Queue {
            jobs,
            available: capture_credit,
        });
        let ready = std::sync::Condvar::new();
        std::thread::scope(|scope| -> io::Result<()> {
            let workers: Vec<_> = (0..width.min(scheduled))
                .map(|_| {
                    scope.spawn(|| -> io::Result<()> {
                        loop {
                            let ((index, o, slot, prior), cost) = {
                                let mut q = state.lock().unwrap();
                                loop {
                                    if q.jobs.is_empty() {
                                        return Ok(());
                                    }
                                    if let Some(job) = q.take() {
                                        break job;
                                    }
                                    q = ready.wait(q).unwrap();
                                }
                            };
                            // Return credit on unwinding too. Waiting workers must be
                            // able to drain the queue even when a parser panics.
                            struct Return<'a, 'b>(
                                &'a std::sync::Mutex<Queue<'b>>,
                                &'a std::sync::Condvar,
                                usize,
                            );
                            impl Drop for Return<'_, '_> {
                                fn drop(&mut self) {
                                    self.0.lock().unwrap().available += self.2;
                                    self.1.notify_all()
                                }
                            }
                            let _credit = Return(&state, &ready, cost);
                            // A graph-preserving edit has no consumer for the retained
                            // source bodies until their bytes reach the new segment.
                            let refresh_markdown =
                                !reuse_markdown && markdown::is_markdown(&o.path);
                            let forwarded = forward_known
                                && keep_graph
                                && prior.is_some()
                                && !read_all_text
                                && !refresh_markdown;
                            let captured = o.captured.take();
                            let body = if forwarded {
                                None
                            } else {
                                match captured {
                                    Some(text) => text,
                                    None => read(root, &o.path, o.stamp)?,
                                }
                            };
                            let (prior, blob) = identify(index, o, body.as_deref(), prior)?;
                            let regenerated_md = if prior.is_some() && refresh_markdown {
                                body.as_deref().map(|text| Parsed::parse(&o.path, text))
                            } else {
                                None
                            };
                            let source = body.map(|text| pool.retain(text)).transpose()?;
                            let facts = if let Some(id) = prior {
                                let p = previous.unwrap();
                                let mut local = p.local(
                                    id,
                                    !keep_graph,
                                    reuse_markdown && (!share_markdown || rewrite_rank),
                                )?;
                                if refresh_markdown {
                                    local.markdown = regenerated_md;
                                }
                                if p.entries()[id].path() != o.path
                                    && let Some(doc) = &mut local.markdown
                                {
                                    doc.relocate(&o.path);
                                }
                                Some(local)
                            } else {
                                match &source {
                                    Some(Source::Scratch(_)) => None,
                                    source => Some(analyze(
                                        &o.path,
                                        source.as_ref().map(|s| s.text(None)),
                                    )),
                                }
                            };
                            *slot = Some(Captured {
                                blob,
                                source,
                                local: facts,
                                prior,
                            });
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| io::Error::other("capture worker failed"))??;
            }
            Ok(())
        })?;
    }
    let backing = pool.seal()?;
    let mut sources = Vec::with_capacity(slots.len());
    let mut blobs = Vec::with_capacity(slots.len());
    let mut parsed = Vec::with_capacity(slots.len());
    let mut reused_local = Vec::with_capacity(slots.len());
    for slot in slots.into_iter().map(Option::unwrap) {
        sources.push(slot.source);
        blobs.push(slot.blob);
        parsed.push(slot.local);
        reused_local.push(slot.prior);
    }
    // Ranked path tokens and qualified postings incorporate the current path.
    let reuse: Vec<_> = reused_local
        .iter()
        .zip(obs.iter())
        .map(|(&id, o)| id.filter(|&id| previous.unwrap().entries()[id].path() == o.path))
        .collect();
    let share_graph = verified_graph.is_some_and(|(_, equal)| equal)
        || incremental::graph_unchanged(previous, obs, &reuse);
    if keep_graph && !share_graph {
        return Err(io::Error::other(
            "a verified binding input changed during capture",
        ));
    }
    if backing.is_some() {
        let jobs = std::sync::Mutex::new(
            parsed
                .iter_mut()
                .enumerate()
                .filter(|(_, f)| f.is_none())
                .collect::<Vec<_>>(),
        );
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..width)
                .map(|_| {
                    scope.spawn(|| {
                        loop {
                            let job = jobs.lock().unwrap().pop();
                            let Some((i, facts)) = job else { break };
                            *facts = Some(analyze(
                                &obs[i].path,
                                sources[i].as_ref().map(|s| s.text(backing.as_ref())),
                            ));
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| io::Error::other("mapped analysis worker failed"))?
            }
            Ok::<_, io::Error>(())
        })?;
    }
    let locals: Vec<_> = parsed.into_iter().map(Option::unwrap).collect();
    let units: Vec<_> = obs
        .iter()
        .zip(&locals)
        .zip(&sources)
        .map(|((o, l), source)| Unit {
            path: &o.path,
            text: source.as_ref().map_or("", |s| s.text(backing.as_ref())),
            analysis: &l.code,
        })
        .collect();
    let mut dir = Directory::default();
    let start = w.offset();
    // Transpose lexical and ranked postings independently. The writers share
    // immutable file bodies. Only the publication thread owns the segment.
    let before = incremental::edited_sources(previous, &units, &reuse, capture_credit, old_body)?;
    // Facts can survive a changed source. Reuse graph extents only after proving
    // both parser evidence and the source slices consumed by binding are equal.
    let mut evidence_reuse = std::borrow::Cow::Borrowed(reused_local.as_slice());
    if let Some((id, true)) = verified_graph {
        evidence_reuse.to_mut()[id] = Some(id);
    }
    let share_graph = share_graph
        || verified_graph.is_none()
            && incremental::same_binding_inputs(previous, &units, &before, &mut evidence_reuse)?;
    let (mut files, paths, content, ranked, names) =
        std::thread::scope(|scope| -> io::Result<_> {
            let bind = (!share_graph).then(|| scope.spawn(|| Names::build(&units)));
            let trigrams = scope.spawn(|| incremental::trigrams(previous, &units, &reuse, &before));
            let rank =
                scope.spawn(|| incremental::ranked(previous, &units, &locals, &reuse, &before));
            let mut files = Vec::with_capacity(obs.len());
            let mut writer = retained::Run::new(previous, w);
            for (id, ((o, unit), source)) in obs.iter().zip(&units).zip(&sources).enumerate() {
                let off = writer.offset() - start;
                let bytes = if let Some(old_id) = reused_local[id] {
                    let p = previous.unwrap();
                    let old = &p.entries()[old_id];
                    if let Some(range) = &old.bytes {
                        let base = p.extent(SOURCE).start;
                        writer.borrow(base + range.start..base + range.end)?;
                        Some(off..writer.offset() - start)
                    } else {
                        None
                    }
                } else if source.is_some() {
                    writer.bytes(unit.text.as_bytes())?;
                    Some(off..writer.offset() - start)
                } else {
                    None
                };
                files.push(Entry {
                    blob: blobs[id],
                    catalog_row: 0..0,
                    original_stamp: None,
                    observation: Observation {
                        path: o.path.clone(),
                        stamp: o.stamp,
                    },
                    bytes,
                    line_ends: Vec::new(),
                    evidence: 0..0,
                });
            }
            writer.flush()?;
            let (paths, content) = trigrams
                .join()
                .map_err(|_| io::Error::other("posting worker failed"))??;
            Ok((
                files,
                paths,
                content,
                rank.join()
                    .map_err(|_| io::Error::other("ranking worker failed"))??,
                bind.map(|job| {
                    job.join()
                        .map_err(|_| io::Error::other("binding worker failed"))
                })
                .transpose()?,
            ))
        })?;
    dir.0[SOURCE - 1] = start..w.offset();
    if let Some(names) = names {
        // Encode small graph fields together before the segment writer computes
        // block checksums. Logical offsets include bytes in the pending buffer.
        use std::io::Write;
        let mut graph_out = io::BufWriter::with_capacity(64 * 1024, &mut *w);
        let start = graph_out.offset();
        let base = graph::encode_symbols(&units, &names, &mut graph_out)?;
        dir.0[SYMBOLS - 1] = start..graph_out.offset();
        let start = graph_out.offset();
        graph::encode_relations(&units, &names, &base, &mut graph_out)?;
        dir.0[REL - 1] = start..graph_out.offset();
        graph_out.flush()?;
    } else {
        let p = previous.unwrap();
        for kind in [SYMBOLS, REL] {
            let start = w.offset();
            retained::range(p, w, p.extent(kind))?;
            dir.0[kind - 1] = start..w.offset();
        }
    }
    drop(units);
    drop(sources);
    drop(backing);
    let start = w.offset();
    if share_markdown {
        markdown_delta::write(w, previous.unwrap(), &files, &locals, &reuse)?;
    } else {
        let mut docs = Vec::new();
        let mut destinations = Vec::new();
        let mut backlinks = BTreeMap::<(&str, &str), Vec<(usize, usize)>>::new();
        put(w, locals.iter().filter(|l| l.markdown.is_some()).count())?;
        for (i, l) in locals.iter().enumerate() {
            if let Some(m) = &l.markdown {
                docs.push(w.offset() - start);
                put(w, i)?;
                m.write_to(&mut *w)?;
                for (n, link) in m.links.iter().enumerate() {
                    backlinks
                        .entry((&link.target, &link.anchor))
                        .or_default()
                        .push((i, n));
                }
            }
        }
        docs.push(w.offset() - start);
        put(w, backlinks.len())?;
        for ((path, anchor), links) in backlinks {
            destinations.push(w.offset() - start);
            string(w, path)?;
            string(w, anchor)?;
            put(w, links.len())?;
            for (file, link) in links {
                put(w, file)?;
                put(w, link)?;
            }
        }
        destinations.push(w.offset() - start);
        super::offsets::finish(w, start, [docs, destinations])?;
    }
    dir.0[MD - 1] = start..w.offset();
    let offsets = retained::evidence(w, previous, &evidence_reuse, &locals, &files)?;
    let start = w.offset();
    w.write_all(super::CATALOG_MAGIC)?;
    put(w, files.len())?;
    let mut writer = retained::Run::new(previous, w);
    for (i, ((f, mut l), prior)) in files
        .iter_mut()
        .zip(locals)
        .zip(reuse.iter().copied())
        .enumerate()
    {
        let row_start = writer.offset() - start;
        f.evidence = offsets[i]..offsets[i + 1];
        if l.partial_line {
            l.lf.pop();
        }
        f.line_ends = l.lf;
        if let Some((p, old)) = previous.zip(prior)
            && let old = &p.entries()[old]
            && !old.catalog_row.is_empty()
            && old.blob == f.blob
            && old.catalog_stamp() == f.stamp()
        {
            let base = p.extent(FILES).start;
            writer.borrow(base + old.catalog_row.start..base + old.catalog_row.end)?;
        } else {
            f.write(&mut writer)?;
        }
        f.catalog_row = row_start..writer.offset() - start;
    }
    writer.bytes(&offsets[0].to_le_bytes())?;
    for (pair, prior) in offsets.windows(2).zip(reuse) {
        let len = pair[1] - pair[0];
        if let Some((p, id)) = previous.zip(prior)
            && let old = &p.entries()[id]
            && !old.catalog_row.is_empty()
            && old.evidence.end - old.evidence.start == len
        {
            let word = p.extent(FILES).end - 8 * (p.entries().len() - id) as u64;
            writer.borrow(word..word + 8)?;
        } else {
            writer.bytes(&len.to_le_bytes())?;
        }
    }
    writer.flush()?;
    dir.0[FILES - 1] = start..w.offset();
    let start = w.offset();
    paths.seal(&mut *w, previous, PATHGRAM)?;
    dir.0[PATHGRAM - 1] = start..w.offset();
    let start = w.offset();
    content.seal(&mut *w, previous, CONTENT)?;
    dir.0[CONTENT - 1] = start..w.offset();
    let start = w.offset();
    ranked.write_to(&mut *w, previous)?;
    dir.0[RANK - 1] = start..w.offset();
    Ok((dir, files))
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
            let read = |_: &Path, path: &str, _: [u64; 7]| {
                let i = fixture.iter().position(|(name, _)| *name == path).unwrap();
                counts[i].fetch_add(1, Ordering::Relaxed);
                Ok(fixture[i].1.map(String::from))
            };
            let mut sink = MemorySink::default();
            let (dir, _) = build_with_allowance(
                Path::new("."),
                &mut obs,
                &read,
                None,
                false,
                &|_, _, text, id| {
                    Ok((
                        id,
                        text.map(|s| crate::adapters::repository::raw_blob_id(s.as_bytes(), false)),
                    ))
                },
                &mut sink,
                allowance,
            )
            .unwrap();
            assert_eq!(counts.map(AtomicUsize::into_inner), [0, 1, 1, 1, 1, 1, 1]);
            (dir.0, sink.0)
        };
        let heap = build(8192);
        assert_eq!(heap, build(1));
        assert_eq!(heap, build(64));
    }
}
