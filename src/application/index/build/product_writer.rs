//! Write the captured source and its file, Markdown and evidence catalogs.
use super::*;
use std::{
    ops::Range,
    thread::{Scope, ScopedJoinHandle},
};

type Batches<'a> = Vec<(Range<usize>, ScopedJoinHandle<'a, evidence_batches::Batch>)>;

pub(super) struct CatalogWriter<'a> {
    pub previous: Option<&'a dyn Previous>,
    pub obs: &'a [ObservedPath],
    pub units: &'a [Unit<'a>],
    pub table: &'a CaptureTable,
    pub locals: &'a [Local],
}

impl<'a> CatalogWriter<'a> {
    pub fn evidence_batches<'s>(
        &self,
        scope: &'s Scope<'s, '_>,
        width: usize,
        capture_credit: usize,
    ) -> Option<Batches<'s>>
    where
        'a: 's,
    {
        evidence_batches::plan(self.units, width, capture_credit).map(|(ranges, cap)| {
            ranges
                .into_iter()
                .map(|range| {
                    let slice = &self.units[range.clone()];
                    let bodies = &self.table.sources[range.clone()];
                    (
                        range,
                        scope.spawn(move || evidence_batches::encode(slice, bodies, cap)),
                    )
                })
                .collect()
        })
    }
    fn write_sources(&self, w: &mut impl ExtentSink) -> io::Result<Vec<Entry>> {
        let start = w.offset();
        let mut files = Vec::with_capacity(self.obs.len());
        let mut writer = retained::Run::new(self.previous, w);
        for (id, ((o, unit), source)) in self
            .obs
            .iter()
            .zip(self.units)
            .zip(&self.table.sources)
            .enumerate()
        {
            let off = writer.offset() - start;
            let old = self.table.retained_rows[id].map(|id| {
                let prior = self.previous.expect("retained source has a predecessor");
                (prior, &prior.entries()[id])
            });
            let bytes = match (old, source) {
                (Some((prior, row)), _) => row
                    .bytes
                    .as_ref()
                    .map(|range| {
                        let base = prior.extent(SOURCE).start;
                        writer.borrow(base + range.start..base + range.end)?;
                        Ok::<_, io::Error>(off..writer.offset() - start)
                    })
                    .transpose()?,
                (None, Some(_)) => {
                    writer.bytes(unit.text.as_bytes())?;
                    Some(off..writer.offset() - start)
                }
                (None, None) => None,
            };
            let score_marker = old.map_or_else(
                || unit.text.contains(code::score::policy::MARKER),
                |(_, row)| row.score_marker,
            );
            files.push(Entry {
                score_marker,
                blob: self.table.blobs[id],
                score: self.locals[id].score.clone(),
                test_score: self.locals[id].test_score.clone(),
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

        Ok(files)
    }

    pub fn write(
        &self,
        w: &mut impl ExtentSink,
        dir: &mut Directory,
        batches: Option<Batches<'_>>,
        reuse: ProductReuse<'_>,
    ) -> io::Result<Vec<Entry>> {
        let mut files = dir.write_product(w, SOURCE, |w| self.write_sources(w))?;
        let markdown = reuse
            .markdown
            .then(|| self.previous.expect("retained Markdown has a predecessor"));
        dir.write_product(w, MD, |w| match markdown {
            Some(prior) => markdown_delta::Documents {
                prior,
                files: &files,
                locals: self.locals,
                reuse: reuse.source,
            }
            .write_product(w),
            None => catalog::write_markdown(w, self.locals),
        })?;
        let evidence = retained::EvidenceRows {
            prior: self.previous,
            locals: self.locals,
            files: &files,
            reuse: reuse.evidence,
        };
        let offsets = match batches {
            Some(batches) => evidence_batches::write(w, batches, evidence)?,
            None => evidence.write(w)?,
        };
        let rows = catalog::Rows {
            previous: self.previous,
            locals: self.locals,
            reuse: reuse.source,
            offsets: &offsets,
        };
        dir.write_product(w, FILES, |w| rows.write(w, &mut files))?;
        Ok(files)
    }
}

#[derive(Clone, Copy)]
pub(super) struct GraphInputs<'a> {
    pub units: &'a [Unit<'a>],
    pub before: Option<&'a std::collections::BTreeMap<usize, String>>,
}

/// The build chooses retention only after the captured source identities are verified.
#[derive(Clone, Copy)]
pub(super) enum GraphPlan<'a> {
    Retain {
        previous: &'a dyn Previous,
        end_patch: Option<(u64, u64)>,
    },
    Build {
        configured: bool,
    },
}

#[derive(Clone, Copy)]
pub(super) struct ProductReuse<'a> {
    pub source: &'a [Option<usize>],
    pub evidence: &'a [Option<usize>],
    pub markdown: bool,
}

pub(super) struct ProductPlan<'a> {
    pub catalog: CatalogWriter<'a>,
    pub paths: &'a PathOrder,
    pub reuse: ProductReuse<'a>,
    pub before: Option<&'a std::collections::BTreeMap<usize, String>>,
    pub graph: GraphPlan<'a>,
    pub searchable: incremental::Compatibility,
    pub evidence_width: Option<usize>,
    pub capture_credit: usize,
}
pub(super) struct SealedProducts {
    pub files: Vec<Entry>,
    pub rank_write: Option<Worker<u64>>,
    pub dir: Directory,
}

impl SealedProducts {
    /// Transfer captured line tables to the published rows once writers release their borrows.
    pub(super) fn finish(
        mut self,
        paths: PathOrder,
        mut retired: Vec<Local>,
        config: super::c_project::Refresh,
    ) -> super::Output {
        for (file, local) in self.files.iter_mut().zip(&mut retired) {
            file.line_ends = std::mem::take(&mut local.line_ends);
        }
        super::Output {
            paths,
            files: self.files,
            dir: self.dir,
            rank_write: self.rank_write,
            retired,
            sdk_catalog: config.catalog,
            sdk_receipts: config.receipts,
        }
    }
}

impl ProductPlan<'_> {
    fn encode_graph(&self, has_catalog: bool) -> io::Result<(Vec<u8>, Vec<u8>)> {
        let units = self.catalog.units;
        let projection = if has_catalog {
            code::Projection::new(units)?
        } else {
            code::Projection::default()
        };
        let projected = projection.units(units);
        let units = projected.as_deref().unwrap_or(units);
        let mut names = binding_reuse::bind(
            units,
            self.catalog
                .previous
                .filter(|_| !has_catalog && self.paths.stable()),
            binding_reuse::Inputs {
                unchanged: self.reuse.source,
                budget: self.capture_credit,
                before: self.before,
            },
        )?;
        projection.bind(&mut names);
        let bases = graph::declaration_bases(units);
        std::thread::scope(|scope| {
            let symbols = scope.spawn(|| {
                let mut bytes = Vec::new();
                graph::encode_symbols(units, &names, &bases, &mut bytes)?;
                Ok::<_, io::Error>(bytes)
            });
            let mut relations = Vec::new();
            graph::encode_relations(units, &names, &bases, &mut relations)?;
            let symbols = symbols
                .join()
                .map_err(|_| io::Error::other("symbol worker failed"))??;
            Ok((symbols, relations))
        })
    }

    /// Only the owned rank writer can leave this scope. All borrowed product
    /// jobs finish before the captured sources and locals are released.
    pub fn write(self, w: &mut impl ExtentSink, lanes: Lanes<'_>) -> io::Result<SealedProducts> {
        let previous = self.catalog.previous;
        let incremental = incremental::Products::from_plan(&self);
        let mut dir = Directory::default();
        std::thread::scope(|scope| {
            let provenance = lanes
                .provenance
                .map(|finish| scope.spawn(|| finish(self.catalog.obs, &self.catalog.table.blobs)));
            let batches = self.evidence_width.and_then(|width| {
                self.catalog
                    .evidence_batches(scope, width, self.capture_credit)
            });
            let graph = match self.graph {
                GraphPlan::Retain {
                    previous,
                    end_patch,
                } => GraphProduct::Retained {
                    previous,
                    end_patch,
                },
                GraphPlan::Build { configured } => GraphProduct::Building(scope.spawn({
                    let plan = &self;
                    move || plan.encode_graph(configured)
                })),
            };
            let trigrams = scope.spawn(|| incremental.trigrams(self.paths));
            let rank = scope
                .spawn(|| RankProduct::stage(incremental.ranked(self.catalog.locals)?, lanes.rank));
            let files = self.catalog.write(w, &mut dir, batches, self.reuse)?;
            let (paths, content) = trigrams
                .join()
                .map_err(|_| io::Error::other("posting worker failed"))??;
            dir.write_product(w, PATHGRAM, |w| paths.seal(w, previous, PATHGRAM))?;
            dir.write_product(w, CONTENT, |w| content.seal(w, previous, CONTENT))?;
            graph.write(w, &mut dir)?;
            let rank = rank
                .join()
                .map_err(|_| io::Error::other("ranking worker failed"))??;
            let rank_write = dir.write_product(w, RANK, |w| rank.write(w, previous))?;
            if let Some(provenance) = provenance {
                provenance
                    .join()
                    .map_err(|_| io::Error::other("provenance worker failed"))?;
            }
            Ok(SealedProducts {
                files,
                rank_write,
                dir,
            })
        })
    }
}
enum GraphProduct<'a> {
    Retained {
        previous: &'a dyn Previous,
        end_patch: Option<(u64, u64)>,
    },
    Building(std::thread::ScopedJoinHandle<'a, io::Result<(Vec<u8>, Vec<u8>)>>),
}

impl GraphProduct<'_> {
    fn write(self, w: &mut impl ExtentSink, dir: &mut Directory) -> io::Result<()> {
        match self {
            Self::Building(job) => {
                let (symbols, relations) = job
                    .join()
                    .map_err(|_| io::Error::other("binding worker failed"))??;
                dir.write_product(w, SYMBOLS, |w| w.write_all(&symbols))?;
                dir.write_product(w, REL, |w| w.write_all(&relations))
            }
            Self::Retained {
                previous,
                end_patch,
            } => {
                dir.write_product(w, SYMBOLS, |w| write_retained_symbols(previous, w, end_patch))?;
                dir.write_product(w, REL, |w| {
                    retained::range(previous, w, previous.extent(REL))
                })
            }
        }
    }
}

fn write_retained_symbols(
    previous: &dyn Previous,
    w: &mut impl ExtentSink,
    end_patch: Option<(u64, u64)>,
) -> io::Result<()> {
    let extent = previous.extent(SYMBOLS);
    let Some((at, end)) = end_patch else {
        return retained::range(previous, w, extent);
    };
    retained::range(previous, w, extent.start..at)?;
    w.write_all(&end.to_le_bytes())?;
    retained::range(previous, w, at + 8..extent.end)
}

enum RankProduct {
    Inline(incremental::Rank),
    Separate(Worker<u64>),
}

impl RankProduct {
    fn stage(rank: incremental::Rank, lane: Option<RankLane>) -> io::Result<Self> {
        match (lane, rank) {
            (None, rank) => Ok(Self::Inline(rank)),
            (Some(emit), incremental::Rank::Bulk(rank)) => Ok(Self::Separate(emit(rank)?)),
            _ => Err(io::Error::other("rank lane requires a fresh build")),
        }
    }

    fn write(
        self,
        w: &mut impl ExtentSink,
        previous: Option<&dyn Previous>,
    ) -> io::Result<Option<Worker<u64>>> {
        match self {
            Self::Inline(rank) => {
                rank.write_to(w, previous)?;
                Ok(None)
            }
            Self::Separate(worker) => Ok(Some(worker)),
        }
    }
}
