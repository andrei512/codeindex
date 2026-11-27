//! Reuse product memberships independently of parser output. Ranked positions
//! and posting IDs in this codec are generation-relative; edits translate the
//! retained positions without tokenizing their owning documents again.
use super::{Local, Previous, path_order::PathOrder, retained};
use crate::application::index::layout::{CONTENT, ExtentSink, PATHGRAM, RANK, SOURCE};
use crate::domain::{
    Document, FileId,
    code::Unit,
    content::{ContentBuilder, ContentRun, MergedPostings},
    ranked::{self, RankedIndex},
};
use std::{collections::BTreeMap, io};

pub(super) enum Postings {
    Built(ContentBuilder),
    Fresh(crate::domain::content::fresh::Product),
    Merged(MergedPostings),
    Delta(super::content_delta::Delta),
    Retained(usize),
}

fn retain_product(
    previous: Option<&dyn Previous>,
    out: &mut impl ExtentSink,
    kind: usize,
) -> io::Result<()> {
    let prior = previous.expect("reuse requires a predecessor");
    retained::range(prior, out, prior.extent(kind))
}
impl Postings {
    pub(super) fn seal(
        self,
        out: &mut impl ExtentSink,
        previous: Option<&dyn Previous>,
        kind: usize,
    ) -> io::Result<()> {
        match self {
            Self::Fresh(p) => p.seal(out),
            Self::Built(b) => b.seal_layout(out, kind == CONTENT),
            Self::Merged(m) => m.seal_layout(out, kind == CONTENT),
            Self::Delta(d) => d.write(previous.unwrap(), out),
            Self::Retained(kind) => retain_product(previous, out, kind),
        }
    }
}

pub(super) fn edited_sources(
    previous: Option<&dyn Previous>,
    reuse: &[Option<usize>],
    capture_allowance: usize,
    mut prepared: Option<(usize, String)>,
) -> io::Result<Option<BTreeMap<usize, String>>> {
    let Some(p) = previous else { return Ok(None) };
    // Hashing/trigram and ranking producers share old changed sources. On a
    // bulk edit use the ordinary posting merge if that capture would exceed
    // the now-idle worker allowance.
    if !fits_old_sources(p, reuse, capture_allowance) {
        return Ok(None);
    }
    let mut edited = BTreeMap::new();
    let base = p.extent(SOURCE).start;
    for (id, prior) in reuse.iter().enumerate() {
        if prior.is_some() {
            continue;
        }
        let text = prepared
            .take_if(|(file, _)| *file == id)
            .map_or_else(|| original_source(p, id, base), |(_, text)| Ok(text))?;
        edited.insert(id, text);
    }
    Ok(Some(edited))
}

fn fits_old_sources(prior: &dyn Previous, reuse: &[Option<usize>], allowance: usize) -> bool {
    prior
        .entries()
        .iter()
        .zip(reuse)
        .filter(|(_, old)| old.is_none())
        .filter_map(|(entry, _)| entry.bytes.as_ref())
        .try_fold(allowance as u64, |available, range| {
            available.checked_sub(range.end - range.start)
        })
        .is_some()
}

fn original_source(prior: &dyn Previous, id: usize, base: u64) -> io::Result<String> {
    let mut bytes = Vec::new();
    if let Some(range) = &prior.entries()[id].bytes {
        prior.copy_bytes(base + range.start..base + range.end, &mut bytes)?;
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

/// The postings and ranked workers consume the same captured source mapping.
#[derive(Clone, Copy)]
struct Reuse<'a> {
    rows: &'a [Option<usize>],
    any: bool,
    same: bool,
}
impl<'a> Reuse<'a> {
    fn remap(&self, previous_count: usize) -> Vec<Option<usize>> {
        let mut remap = vec![None; previous_count];
        for (new, old) in self.rows.iter().enumerate() {
            if let Some(old) = old {
                remap[*old] = Some(new);
            }
        }
        remap
    }

    fn inspect(previous: Option<&dyn Previous>, rows: &'a [Option<usize>]) -> Self {
        let Some(previous) = previous else {
            return Self {
                rows,
                any: false,
                same: false,
            };
        };
        let mut same = rows.len() == previous.entries().len();
        let mut any = false;
        for (id, &row) in rows.iter().enumerate() {
            any |= row.is_some();
            same &= row == Some(id);
            if any && !same {
                break;
            }
        }
        Self { rows, any, same }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Compatibility {
    pub content: bool,
    pub rank: bool,
}

impl Compatibility {
    pub fn inspect(previous: Option<&dyn Previous>) -> Self {
        Self {
            content: previous.is_some_and(|p| p.compatible(CONTENT)),
            rank: previous.is_some_and(|p| p.compatible(RANK)),
        }
    }
}

pub(super) struct Products<'a> {
    previous: Option<&'a dyn Previous>,
    units: &'a [Unit<'a>],
    source: Reuse<'a>,
    compatibility: Compatibility,
    before: Option<&'a BTreeMap<usize, String>>,
}
impl<'a> Products<'a> {
    pub fn from_plan(plan: &super::ProductPlan<'a>) -> Self {
        let previous = plan.catalog.previous;
        Self {
            previous,
            units: plan.catalog.units,
            source: Reuse::inspect(previous, plan.reuse.source),
            compatibility: plan.searchable,
            before: plan.before,
        }
    }
}

fn changed_postings(
    product: usize,
    units: &[Unit<'_>],
    has_previous: bool,
    reuse: &[Option<usize>],
) -> io::Result<ContentBuilder> {
    let mut builder = ContentBuilder::default();
    for (id, unit) in units.iter().enumerate() {
        if !has_previous || reuse[id].is_none() {
            builder.add(Document {
                file: FileId(id as u64),
                text: if product == PATHGRAM {
                    unit.path
                } else {
                    unit.text
                },
            })?;
        }
    }
    Ok(builder)
}

enum PostingRoute<'a> {
    Build,
    Retain,
    Merge(&'a dyn Previous),
    Delta(&'a dyn Previous, &'a BTreeMap<usize, String>),
}

impl<'a> PostingRoute<'a> {
    fn select(
        previous: Option<&'a dyn Previous>,
        product: usize,
        reuse: Reuse<'_>,
        before: Option<&'a BTreeMap<usize, String>>,
    ) -> Self {
        let delta = before.filter(|_| product == CONTENT);
        match (previous, delta) {
            (Some(_), _) if reuse.same => Self::Retain,
            (Some(prior), Some(delta)) => Self::Delta(prior, delta),
            (Some(prior), _) if reuse.any => Self::Merge(prior),
            _ => Self::Build,
        }
    }
}

impl Products<'_> {
    fn postings(
        &self,
        previous: Option<&dyn Previous>,
        product: usize,
        reuse: Reuse<'_>,
    ) -> io::Result<Postings> {
        let units = self.units;
        match PostingRoute::select(previous, product, reuse, self.before) {
            PostingRoute::Retain => Ok(Postings::Retained(product)),
            PostingRoute::Delta(prior, before) => {
                super::content_delta::Delta::plan(prior, units, before)
                    .map(|plan| plan.map_or(Postings::Retained(CONTENT), Postings::Delta))
            }
            PostingRoute::Build => {
                changed_postings(product, units, false, reuse.rows).map(Postings::Built)
            }
            PostingRoute::Merge(prior) => {
                let builder = changed_postings(product, units, true, reuse.rows)?;
                let remap = reuse.remap(prior.entries().len());
                let bytes = prior.product(product)?;
                builder
                    .merge_prior(&ContentRun::open(&bytes)?, &remap)
                    .map(Postings::Merged)
            }
        }
    }

    pub fn trigrams(&self, paths: &PathOrder) -> io::Result<(Postings, Postings)> {
        let previous = self.previous;
        let units = self.units;
        let Some(prior) = previous else {
            let (paths, bodies) = super::fresh_postings::build(units)?;
            return Ok((Postings::Built(paths), bodies));
        };
        let path_postings = match paths {
            PathOrder::Stable if prior.compatible(PATHGRAM) => Postings::Retained(PATHGRAM),
            PathOrder::Stable => self.postings(None, PATHGRAM, self.source)?,
            PathOrder::Changed(mapping) => {
                let compatible = previous.filter(|prior| prior.compatible(PATHGRAM));
                self.postings(compatible, PATHGRAM, Reuse::inspect(previous, mapping))?
            }
            PathOrder::Fresh => unreachable!("a previous generation has a path comparison"),
        };
        Ok((
            path_postings,
            self.postings(
                previous.filter(|_| self.compatibility.content),
                CONTENT,
                self.source,
            )?,
        ))
    }
}
pub(super) enum Rank {
    Built(RankedIndex),
    Bulk(ranked::FreshRank),
    Patch(ranked::patch::Patch),
    Retained(usize),
}

impl Rank {
    pub(super) fn write_to(
        self,
        out: &mut impl ExtentSink,
        previous: Option<&dyn Previous>,
    ) -> io::Result<()> {
        match self {
            Self::Built(b) => b.write_to(out),
            Self::Bulk(b) => b.write_to(out),
            Self::Patch(patch) => {
                let prior = previous.unwrap();
                let mut run = retained::Run::new(Some(prior), out);
                let base = prior.extent(RANK).start;
                for piece in patch.pieces {
                    match piece {
                        ranked::patch::Piece::Bytes(b) => run.bytes(&b)?,
                        ranked::patch::Piece::Borrow(range) => {
                            run.borrow(base + range.start..base + range.end)?
                        }
                    }
                }
                run.flush()
            }
            Self::Retained(kind) => retain_product(previous, out, kind),
        }
    }
}

pub(super) fn rank_chunks(local: &Local) -> Vec<ranked::ChunkRange> {
    ranked::from_sections(&local.line_ends, local.byte_len, |heading| {
        if let Some(doc) = &local.markdown {
            for section in doc.sections.iter().skip(1) {
                heading(section.line);
            }
        }
    })
}

fn rank_patch(
    previous: &dyn Previous,
    units: &[Unit],
    locals: &[Local],
    before: &BTreeMap<usize, String>,
) -> io::Result<Option<ranked::patch::Patch>> {
    let extent = previous.extent(RANK);
    let edits = before
        .iter()
        .map(|(&id, old)| ranked::patch::Edit {
            file: id,
            path: units[id].path,
            old,
            new: units[id].text,
            chunks: rank_chunks(&locals[id]),
        })
        .collect::<Vec<_>>();
    ranked::patch::patch(
        extent.end - extent.start,
        |r| {
            let mut bytes = Vec::new();
            previous.copy_bytes(extent.start + r.start..extent.start + r.end, &mut bytes)?;
            Ok(bytes)
        },
        &edits,
    )
}

impl Products<'_> {
    pub fn ranked(&self, locals: &[Local]) -> io::Result<Rank> {
        let previous = self
            .previous
            .filter(|_| self.compatibility.rank && self.source.any);
        let units = self.units;
        let chunks_for = |id: usize| rank_chunks(&locals[id]);
        if let Some(prior) = previous {
            return self.reuse_rank(prior, locals, |r, id| {
                r.add_file(units[id].path, units[id].text, &chunks_for(id));
            });
        }
        fresh_rank(units, chunks_for)
    }
    fn reuse_rank(
        &self,
        previous: &dyn Previous,
        locals: &[Local],
        add: impl FnMut(&mut RankedIndex, usize),
    ) -> io::Result<Rank> {
        if self.source.same {
            return Ok(Rank::Retained(RANK));
        }
        let patch = self
            .before
            .map(|before| rank_patch(previous, self.units, locals, before))
            .transpose()?
            .flatten();
        if let Some(patch) = patch {
            return Ok(Rank::Patch(patch));
        }
        let bytes = previous.product(RANK)?;
        let prior = RankedIndex::read_from(bytes.as_slice())?;
        drop(bytes);
        RankedIndex::refresh(prior, self.source.rows, add).map(Rank::Built)
    }
}
pub(super) fn fresh_rank(
    units: &[Unit<'_>],
    chunks_for: impl Fn(usize) -> Vec<ranked::ChunkRange> + Sync,
) -> io::Result<Rank> {
    let batch = |start: usize, end: usize| {
        let mut index = ranked::BulkRank::at(start);
        for i in start..end {
            index.add_file(units[i].path, units[i].text, &chunks_for(i));
        }
        index
    };
    let bytes: usize = units.iter().map(|u| u.text.len()).sum();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let index = if workers == 1 || bytes < 1024 * 1024 {
        ranked::FreshRank::Single(batch(0, units.len()))
    } else {
        let goal = bytes.div_ceil(workers).max(512 * 1024);
        std::thread::scope(|scope| -> io::Result<ranked::FreshRank> {
            let (mut start, mut size) = (0, 0);
            let mut jobs = Vec::new();
            for (i, unit) in units.iter().enumerate() {
                size += unit.text.len();
                if size >= goal || i + 1 == units.len() {
                    let first = start;
                    let f = &batch;
                    jobs.push(scope.spawn(move || f(first, i + 1).into_shard()));
                    start = i + 1;
                    size = 0;
                }
            }
            let shards = jobs
                .into_iter()
                .map(|j| j.join().map_err(|_| io::Error::other("rank batch failed")))
                .collect::<io::Result<_>>()?;
            ranked::ShardSet::new(shards).map(ranked::FreshRank::Parallel)
        })?
    };
    Ok(Rank::Bulk(index))
}
