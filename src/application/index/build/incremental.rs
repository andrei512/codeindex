//! Reuse product memberships independently of parser output. Ranked positions
//! and posting IDs in this codec are generation-relative; edits translate the
//! retained positions without tokenizing their owning documents again.
use super::{Local, Previous, retained};
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
    Merged(MergedPostings),
    Delta(super::content_delta::Delta),
    Retained(usize),
}

impl Postings {
    pub(super) fn seal(
        self,
        out: &mut impl ExtentSink,
        previous: Option<&dyn Previous>,
        kind: usize,
    ) -> io::Result<()> {
        match self {
            Self::Built(b) => b.seal_layout(out, kind == CONTENT),
            Self::Merged(m) => m.seal_layout(out, kind == CONTENT),
            Self::Delta(d) => d.write(previous.unwrap(), out),
            Self::Retained(kind) => {
                let p = previous.expect("reuse requires a predecessor");
                retained::range(p, out, p.extent(kind))
            }
        }
    }
}

pub(super) fn edited_sources(
    previous: Option<&dyn Previous>,
    units: &[Unit],
    reuse: &[Option<usize>],
    capture_allowance: usize,
    mut prepared: Option<(usize, String)>,
) -> io::Result<Option<BTreeMap<usize, String>>> {
    let Some(p) = previous else { return Ok(None) };
    if p.entries().len() != units.len()
        || p.entries()
            .iter()
            .zip(units)
            .any(|(f, u)| f.path() != u.path)
    {
        return Ok(None);
    }
    // Hashing/trigram and ranking producers share old changed sources. On a
    // bulk edit use the ordinary posting merge if that capture would exceed
    // the now-idle worker allowance.
    let mut available = capture_allowance as u64;
    for (entry, old) in p.entries().iter().zip(reuse) {
        if old.is_none()
            && let Some(r) = &entry.bytes
        {
            let Some(left) = available.checked_sub(r.end - r.start) else {
                return Ok(None);
            };
            available = left;
        }
    }
    let mut edited = BTreeMap::new();
    let base = p.extent(SOURCE).start;
    for (id, prior) in reuse.iter().enumerate() {
        if prior.is_none() {
            if let Some((id, text)) = prepared.take_if(|(file, _)| *file == id) {
                edited.insert(id, text);
                continue;
            }
            let mut bytes = Vec::new();
            if let Some(range) = &p.entries()[id].bytes {
                p.copy_bytes(base + range.start..base + range.end, &mut bytes)?
            }
            edited.insert(id, String::from_utf8(bytes).map_err(io::Error::other)?);
        }
    }
    Ok(Some(edited))
}

fn postings(
    previous: Option<&dyn Previous>,
    product: usize,
    units: &[Unit<'_>],
    reuse: &[Option<usize>],
    before: &Option<BTreeMap<usize, String>>,
) -> io::Result<Postings> {
    let previous = previous.filter(|p| p.compatible(product));
    if let Some(previous) = previous
        && previous.entries().len() == reuse.len()
        && reuse.iter().enumerate().all(|(i, id)| *id == Some(i))
    {
        return Ok(Postings::Retained(product));
    }
    if product == CONTENT
        && let Some(prior) = previous
        && let Some(before) = before
    {
        return Ok(
            match super::content_delta::Delta::plan(prior, units, before)? {
                Some(plan) => Postings::Delta(plan),
                None => Postings::Retained(CONTENT),
            },
        );
    }
    let mut builder = ContentBuilder::default();
    for (id, unit) in units.iter().enumerate() {
        if previous.is_none() || reuse[id].is_none() {
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
    if let Some(previous) = previous
        && reuse.iter().any(Option::is_some)
    {
        let mut remap = vec![None; previous.entries().len()];
        for (new, &old) in reuse.iter().enumerate() {
            if let Some(old) = old {
                remap[old] = Some(new);
            }
        }
        let bytes = previous.product(product)?;
        return builder
            .merge_prior(&ContentRun::open(&bytes)?, &remap)
            .map(Postings::Merged);
    }
    Ok(Postings::Built(builder))
}

pub(super) fn trigrams(
    previous: Option<&dyn Previous>,
    units: &[Unit<'_>],
    reuse: &[Option<usize>],
    before: &Option<BTreeMap<usize, String>>,
) -> io::Result<(Postings, Postings)> {
    let Some(prior) = previous else {
        let (paths, bodies) = super::fresh_postings::build(units)?;
        return Ok((Postings::Built(paths), Postings::Built(bodies)));
    };
    // Both inventories have the same path ordering. A merge walks them once.
    let mut old = prior.entries().iter().enumerate().peekable();
    let paths: Vec<_> = units
        .iter()
        .map(|unit| {
            while old.next_if(|(_, e)| e.path() < unit.path).is_some() {}
            old.next_if(|(_, e)| e.path() == unit.path).map(|(i, _)| i)
        })
        .collect();
    Ok((
        postings(previous, PATHGRAM, units, &paths, before)?,
        postings(previous, CONTENT, units, reuse, before)?,
    ))
}

pub(super) enum Rank {
    Built(RankedIndex),
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
            Self::Retained(kind) => {
                let p = previous.expect("reuse requires a predecessor");
                retained::range(p, out, p.extent(kind))
            }
        }
    }
}

pub(super) fn ranked(
    previous: Option<&dyn Previous>,
    units: &[Unit<'_>],
    locals: &[Local],
    reuse: &[Option<usize>],
    before: &Option<BTreeMap<usize, String>>,
) -> io::Result<Rank> {
    let previous = previous.filter(|p| p.compatible(RANK));
    let add = |r: &mut RankedIndex, id: usize| {
        let local = &locals[id];
        let headings = local.markdown.as_ref().map_or_else(Vec::new, |m| {
            m.sections.iter().skip(1).map(|s| s.line).collect()
        });
        let chunks = ranked::chunks_from_line_ends(&local.lf, &headings);
        r.add_file(units[id].path, units[id].text, &chunks);
    };
    if let Some(previous) = previous
        && reuse.iter().any(Option::is_some)
    {
        if previous.entries().len() == reuse.len()
            && reuse.iter().enumerate().all(|(i, old)| *old == Some(i))
        {
            return Ok(Rank::Retained(RANK));
        }
        if let Some(before) = before {
            let extent = previous.extent(RANK);
            let edits = before
                .iter()
                .map(|(&id, old)| {
                    let l = &locals[id];
                    let headings = l.markdown.as_ref().map_or_else(Vec::new, |m| {
                        m.sections.iter().skip(1).map(|s| s.line).collect()
                    });
                    ranked::patch::Edit {
                        file: id,
                        path: units[id].path,
                        old,
                        new: units[id].text,
                        chunks: ranked::chunks_from_line_ends(&l.lf, &headings),
                    }
                })
                .collect::<Vec<_>>();
            if let Some(plan) = ranked::patch::patch(
                extent.end - extent.start,
                |r| {
                    let mut bytes = Vec::new();
                    previous
                        .copy_bytes(extent.start + r.start..extent.start + r.end, &mut bytes)?;
                    Ok(bytes)
                },
                &edits,
            )? {
                return Ok(Rank::Patch(plan));
            }
        }
        let bytes = previous.product(RANK)?;
        let prior = RankedIndex::read_from(bytes.as_slice())?;
        drop(bytes);
        return RankedIndex::refresh(prior, reuse, add).map(Rank::Built);
    }
    let mut index = RankedIndex::default();
    for id in 0..units.len() {
        add(&mut index, id);
    }
    Ok(Rank::Built(index))
}

pub(super) fn graph_unchanged(
    prior: Option<&dyn Previous>,
    observed: &[crate::domain::ObservedPath],
    reuse: &[Option<usize>],
) -> bool {
    prior.is_some_and(|previous| {
        previous.compatible(crate::application::index::layout::SYMBOLS)
            && previous.compatible(crate::application::index::layout::REL)
            && previous.entries().len() == observed.len()
            && previous
                .entries()
                .iter()
                .zip(observed)
                .zip(reuse)
                .enumerate()
                .all(|(id, ((old, new), reused))| {
                    old.path() == new.path
                        && (*reused == Some(id)
                            || !crate::domain::code::Names::consumes_text(&new.path))
                })
    })
}

pub(super) fn same_binding_inputs(
    prior: Option<&dyn Previous>,
    units: &[Unit],
    edited: &Option<BTreeMap<usize, String>>,
    evidence_reuse: &mut std::borrow::Cow<'_, [Option<usize>]>,
) -> io::Result<bool> {
    use crate::application::index::layout::{REL, SYMBOLS};
    use crate::domain::code::Names;
    let (Some(p), Some(edited)) = (prior, edited) else {
        return Ok(false);
    };
    if !p.compatible(SYMBOLS) || !p.compatible(REL) {
        return Ok(false);
    }
    // edited_sources supplies a complete delta only when all paths and ordinals agree.
    for (&i, old_text) in edited {
        let unit = &units[i];
        if !Names::consumes_text(unit.path) {
            continue;
        }
        if !super::script_edit::same_inputs(p, i, unit, old_text)? {
            return Ok(false);
        }
        evidence_reuse.to_mut()[i] = Some(i);
    }
    Ok(true)
}
