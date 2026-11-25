use super::{Entry, Observation, ObservedPath, graph};
use crate::{
    application::index::layout::*,
    domain::{
        code::{self, Analysis, ForwardNames, Language, Unit},
        markdown::{self, Parsed},
    },
};
use std::{io, path::Path};

mod c_project;
mod capture;
mod catalog;
mod content_delta;
mod evidence_batches;
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
    pub(crate) score: code::score::Saved,
    pub(crate) test_score: code::score::Saved,
    pub(crate) markdown: Option<Parsed>,
    pub(crate) lf: Vec<usize>,
    pub(crate) partial_line: bool,
}
fn analyze(path: &str, body: Option<&str>) -> Local {
    analyze_with_project(path, body, None)
}
fn analyze_with_project(path: &str, body: Option<&str>, project: Option<&code::CProject>) -> Local {
    analyze_products(path, body, project, true, true, Some(true))
}
fn analyze_products(
    path: &str,
    body: Option<&str>,
    project: Option<&code::CProject>,
    code_needed: bool,
    markdown_needed: bool,
    score_policy: Option<bool>,
) -> Local {
    let text = body.unwrap_or("");
    let (code, mut score, mut test_score) = Language::for_path(path)
        .filter(|_| code_needed && body.is_some())
        .map_or_else(Default::default, |l| {
            if let Some(test_rules) = score_policy {
                return code::score_file(l, text, path, project, test_rules);
            }
            let facts = match project {
                Some(p) if matches!(l, Language::C | Language::Cpp) => p.analyze(l, path, text),
                _ => code::analyze(l, text),
            };
            (facts, None, None)
        });
    let markdown =
        (markdown_needed && body.is_some() && markdown::is_markdown(path)).then(|| Parsed::parse(path, text));
    let mut lf: Vec<_> = text.match_indices('\n').map(|(i, _)| i + 1).collect();
    let partial_line = !text.is_empty() && !text.ends_with('\n');
    if partial_line {
        lf.push(text.len());
    }
    if score_policy.is_some() && let Some(doc) = &markdown {
        score = code::score::markdown(text, doc);
    }
    if score_policy == Some(true) && body.is_some() && path.ends_with(".t") && score.is_none() && code::score::suite_nesting::perl_source(text) {
        let (_, perl_base, perl_test) = code::score_file(Language::Perl, text, path, None, true);
        test_score = perl_test.or(perl_base);
    }
    Local {
        test_score,
        code,
        score,
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
pub(crate) trait Previous: Sync {
    fn compatible(&self, _kind: usize) -> bool {
        true
    }
    fn entries(&self) -> &[Entry];
    fn extent(&self, product: usize) -> std::ops::Range<u64>;
    fn copy_bytes(&self, range: std::ops::Range<u64>, out: &mut dyn io::Write) -> io::Result<()>;
    fn local(&self, id: usize, decode_code: bool, decode_markdown: bool) -> io::Result<Local>;
    fn markdown_record(&self, _id: usize) -> io::Result<Option<std::ops::Range<u64>>> { Ok(None) }
    fn source_end_word(&self, _file: usize, _end: usize) -> io::Result<Option<u64>> { Ok(None) }
    fn product(&self, kind: usize) -> io::Result<Vec<u8>>;
}
mod worker;
pub(crate) use worker::Worker;
pub(crate) type RankLane =
    Box<dyn FnOnce(crate::domain::ranked::FreshRank) -> io::Result<Worker<u64>> + Send>;

pub(crate) type ProvenanceLane<'a> = Box<dyn FnOnce(&[ObservedPath]) -> io::Result<()> + Send + 'a>;

pub(crate) struct Output {
    pub(crate) dir: Directory,
    pub(crate) files: Vec<Entry>,
    pub(crate) rank_write: Option<Worker<u64>>,
    pub(crate) retired: Vec<Local>,
    pub(crate) sdk_receipts: Vec<code::CReceipt>,
}
pub(crate) fn build(
    root: &Path,
    obs: &mut [ObservedPath],
    read: &(impl Fn(&Path, &str, [u64; 7]) -> io::Result<Option<String>> + Sync),
    previous: Option<&dyn Previous>,
    forward_known: bool,
    identify: &Identify,
    w: &mut impl ExtentSink,
    rank_lane: Option<RankLane>,
    provenance_lane: Option<ProvenanceLane<'_>>,
) -> io::Result<Output> {
    build_with_allowance(
        root,
        obs,
        read,
        previous,
        forward_known,
        identify,
        w,
        source::source_allowance(),
        rank_lane,
        provenance_lane,
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
    rank_lane: Option<RankLane>,
    provenance_lane: Option<ProvenanceLane<'_>>,
) -> io::Result<Output> {
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
    let mut config = c_project::Refresh::plan(previous, obs, &reuse)?;
    let keep_graph =
        !config.invalidates_graph() && incremental::graph_unchanged(previous, obs, &reuse);
    let width = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(obs.len().max(1));
    let retained = allowance / 2;
    let capture_credit = allowance.saturating_sub(retained).max(1);
    let pool = Pool::new(retained);
    let mut slots: Vec<Slot> = (0..obs.len()).map(|_| None).collect();
    let prepared = if let Some(previous) = previous
        .filter(|_| stable_paths && forward_known && !keep_graph && !config.invalidates_graph())
    {
        script_edit::prepare(previous, obs, &reuse, &pool, capture_credit, identify)?
    } else {
        None
    };
    let mut old_body = None;
    let mut verified_graph = None;
    let mut end_patch = None;
    if let Some(edit) = prepared {
        verified_graph = Some((edit.id, edit.same_graph));
        end_patch = edit.end_patch;
        slots[edit.id] = Some(edit.capture);
        old_body = Some((edit.id, edit.old_text));
    }
    let keep_graph = keep_graph || verified_graph.is_some_and(|(_, equal)| equal);
    {
        let mut jobs = capture::Jobs::new(capture_credit);
        let direct =
            previous.filter(|_| forward_known && keep_graph && share_markdown && !read_all_text);
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
            jobs.push(o.stamp[2], (index, o, slot, prior));
        }
        jobs.run(width, |(index, o, slot, prior)| {
            // A graph-preserving edit has no consumer for the retained
            // source bodies until their bytes reach the new segment.
            let refresh_markdown = !reuse_markdown && markdown::is_markdown(&o.path);
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
            let same_path = prior.filter(|&id| previous.unwrap().entries()[id].path() == o.path);
            let defer =
                !forwarded && c_project::is_c(&o.path) && config.reparse(&o.path, same_path);
            let facts = if defer {
                None
            } else if let Some(id) = same_path {
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
                    source => Some(analyze(&o.path, source.as_ref().map(|s| s.text(None)))),
                }
            };
            *slot = Some(Captured {
                blob,
                source,
                local: facts,
                prior,
            });
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
        || !config.invalidates_graph() && incremental::graph_unchanged(previous, obs, &reuse);
    if keep_graph && !share_graph {
        return Err(io::Error::other(
            "a verified binding input changed during capture",
        ));
    }
    let mut evidence_reuse = std::borrow::Cow::Borrowed(reused_local.as_slice());
    if config.invalidates_graph() || config.catalog.is_some() {
        for (i, local) in parsed.iter().enumerate() {
            if local.is_none() && reused_local[i].is_some() && c_project::is_c(&obs[i].path) {
                evidence_reuse.to_mut()[i] = None;
            }
        }
    }
    if parsed.iter().any(Option::is_none) || config.rewrite_catalog() {
        let project = config.catalog.is_some().then(|| {
            code::CProject::new(
                root,
                obs.iter()
                    .zip(&sources)
                    .filter(|(o, _)| c_project::database(&o.path))
                    .filter_map(|(o, s)| {
                        Some((o.path.as_str(), s.as_ref()?.text(backing.as_ref())))
                    }),
                |path| {
                    let i = obs.binary_search_by(|o| o.path.as_str().cmp(path)).ok()?;
                    Some((
                        obs[i].path.as_str(),
                        sources[i].as_ref()?.text(backing.as_ref()),
                    ))
                },
            )
        });
        let jobs = std::sync::Mutex::new(
            parsed
                .iter_mut()
                .enumerate()
                .filter(|(_, f)| f.is_none())
                .collect::<Vec<_>>(),
        );
        std::thread::scope(|scope| {
            let count = width.min(jobs.lock().unwrap().len());
            let workers: Vec<_> = (0..count)
                .map(|_| {
                    scope.spawn(|| {
                        loop {
                            let job = jobs.lock().unwrap().pop();
                            let Some((i, facts)) = job else { break };
                            *facts = Some(analyze_with_project(
                                &obs[i].path,
                                sources[i].as_ref().map(|s| s.text(backing.as_ref())),
                                project.as_ref(),
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
        if let Some((id, project)) = config.catalog.zip(project) {
            let dependencies = config.merge(project.consulted(), obs);
            code::CProject::put_dependencies(
                &mut parsed[id].as_mut().unwrap().code,
                dependencies,
                config.receipts.clone(),
            );
            evidence_reuse.to_mut()[id] = None;
        }
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
    // Workers borrow the immutable capture until their product is ready. The
    // primary writer emits independent products before joining those workers.
    let before = incremental::edited_sources(previous, &units, &reuse, capture_credit, old_body)?;
    // Facts can survive a changed source. Reuse graph extents only after proving
    // both parser evidence and the source slices consumed by binding are equal.
    if end_patch.is_none() && let Some((id, true)) = verified_graph {
        evidence_reuse.to_mut()[id] = Some(id);
    }
    let share_graph = share_graph
        || !config.invalidates_graph()
            && verified_graph.is_none()
            && incremental::same_binding_inputs(previous, &units, &before, &mut evidence_reuse)?;
    let (mut files, rank_write) = std::thread::scope(|scope| -> io::Result<_> {
        let batches = if previous.is_none() && backing.is_none() {
            evidence_batches::plan(&units, width, capture_credit).map(|(ranges, cap)| {
                ranges
                    .into_iter()
                    .map(|range| {
                        let slice = &units[range.clone()];
                        let bodies = &sources[range.clone()];
                        (
                            range,
                            scope.spawn(move || evidence_batches::encode(slice, bodies, cap)),
                        )
                    })
                    .collect()
            })
        } else {
            None
        };
        let bind = (!share_graph).then(|| {
            scope.spawn(|| -> io::Result<_> {
                let names = ForwardNames::forward(&units);
                let bases = graph::declaration_bases(&units);
                std::thread::scope(|scope| {
                    let symbols = scope.spawn(|| {
                        let mut bytes = Vec::new();
                        graph::encode_symbols(&units, &names, &bases, &mut bytes)?;
                        Ok::<_, io::Error>(bytes)
                    });
                    let mut relations = Vec::new();
                    graph::encode_relations(&units, &names, &bases, &mut relations)?;
                    let symbols = symbols
                        .join()
                        .map_err(|_| io::Error::other("symbol worker failed"))??;
                    Ok((symbols, relations))
                })
            })
        });
        let trigrams = scope.spawn(|| incremental::trigrams(previous, &units, &reuse, &before));
        let rank = scope.spawn(|| -> io::Result<_> {
            let rank = incremental::ranked(previous, &units, &locals, &reuse, &before)?;
            match (rank_lane, rank) {
                (None, rank) => Ok((Some(rank), None)),
                (Some(emit), incremental::Rank::Bulk(rank)) => Ok((None, Some(emit(rank)?))),
                _ => Err(io::Error::other("rank lane requires a fresh build")),
            }
        });
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
                score: locals[id].score.clone(),
            test_score: locals[id].test_score.clone(),
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
        dir.0[SOURCE - 1] = start..w.offset();
        let start = w.offset();
        if share_markdown {
            markdown_delta::write(w, previous.unwrap(), &files, &locals, &reuse)?;
        } else {
            catalog::write_markdown(w, &locals)?;
        }
        dir.0[MD - 1] = start..w.offset();
        let offsets = if let Some(batches) = batches {
            evidence_batches::write(w, batches, &locals, &files, &evidence_reuse)?
        } else {
            retained::evidence(w, previous, &evidence_reuse, &locals, &files)?
        };
        let start = w.offset();
        catalog::write(w, previous, &mut files, &locals, &reuse, &offsets)?;
        dir.0[FILES - 1] = start..w.offset();
        // Identity rows are complete after capture. Verify their pinned commit
        // while the remaining immutable products reach their private segments.
        let observed = &*obs;
        let provenance = provenance_lane.map(|finish| scope.spawn(move || finish(observed)));
        let (paths, content) = trigrams
            .join()
            .map_err(|_| io::Error::other("posting worker failed"))??;
        let start = w.offset();
        paths.seal(&mut *w, previous, PATHGRAM)?;
        dir.0[PATHGRAM - 1] = start..w.offset();
        let start = w.offset();
        content.seal(&mut *w, previous, CONTENT)?;
        dir.0[CONTENT - 1] = start..w.offset();
        let names = bind
            .map(|job| {
                job.join()
                    .map_err(|_| io::Error::other("binding worker failed"))
            })
            .transpose()?;
        if let Some(graph) = names {
            let (symbols, relations) = graph?;
            let start = w.offset();
            w.write_all(&symbols)?;
            dir.0[SYMBOLS - 1] = start..w.offset();
            let start = w.offset();
            w.write_all(&relations)?;
            dir.0[REL - 1] = start..w.offset();
        } else {
            let p = previous.unwrap();
            for kind in [SYMBOLS, REL] {
                let start = w.offset();
                let extent = p.extent(kind);
                if kind == SYMBOLS && let Some((at, end)) = end_patch {
                    retained::range(p, w, extent.start..at)?;
                    w.write_all(&end.to_le_bytes())?;
                    retained::range(p, w, at + 8..extent.end)?;
                } else {
                    retained::range(p, w, extent)?;
                }
                dir.0[kind - 1] = start..w.offset();
            }
        }
        let ranked = rank
            .join()
            .map_err(|_| io::Error::other("ranking worker failed"))??;
        let start = w.offset();
        let (ranked, separate) = ranked;
        if let Some(ranked) = ranked {
            ranked.write_to(&mut *w, previous)?;
        }
        dir.0[RANK - 1] = start..w.offset();
        if let Some(provenance) = provenance {
            provenance
                .join()
                .map_err(|_| io::Error::other("provenance worker failed"))??;
        }
        Ok((files, separate))
    })?;
    drop(units);
    drop(sources);
    drop(backing);
    let mut retired = locals;
    for (f, local) in files.iter_mut().zip(&mut retired) {
        if local.partial_line {
            local.lf.pop();
        }
        f.line_ends = std::mem::take(&mut local.lf);
    }
    Ok(Output {
        dir,
        files,
        rank_write,
        retired,
        sdk_receipts: config.receipts,
    })
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
            let Output { dir, .. } = build_with_allowance(
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
                None,
                None,
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

mod memory;
pub(crate) use memory::{MemoryQuery, memory_index};
