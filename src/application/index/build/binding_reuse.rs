//! Rebind dirty producers, preserving other providers' published edges. The old
//! directories locate contiguous runs of unchanged rows. New declaration
//! ordinals can shift even when the producer and every target are unchanged.
use super::{
    Previous,
    code::{self, ForwardNames, Site, Unit},
};
use crate::application::index::{
    binary::{Decoder, bad},
    graph::decode_location,
    layout::{REL, SYMBOLS},
    offsets::{lower_bound, read_bytes, table, text, word},
    public_labels,
    symbol_lookup::file_lower,
};
use crate::domain::content_lookup::ExtentRead;
use std::{io, ops::Range};

fn ordinal_end(
    reader: &mut impl ExtentRead,
    (rows, tail): (&Range<u64>, u64),
    index: usize,
    known: Option<(usize, usize)>,
) -> io::Result<usize> {
    if let Some((start, count)) = known {
        return start.checked_add(count).ok_or_else(bad);
    }
    usize::try_from(file_lower(reader, rows, tail, index + 1)?).map_err(|_| bad())
}

struct Directory<R> {
    reader: R,
    rows: Range<u64>,
    tail: u64,
    bases: Vec<usize>,
}
impl<R: ExtentRead> Directory<R> {
    fn open(
        mut reader: R,
        units: &[Unit],
        unchanged: &[Option<usize>],
        count: impl Fn(&Unit) -> usize,
    ) -> io::Result<Self> {
        let footer = reader.length().checked_sub(8).ok_or_else(bad)?;
        let tail = word(&mut reader, footer)?;
        let mut at = tail;
        let rows = table(&mut reader, &mut at)?;
        let mut bases = Vec::with_capacity(units.len() + 1);
        bases.push(0usize);
        for (i, u) in units.iter().enumerate() {
            let next = ordinal_end(&mut reader, (&rows, tail), i, unchanged[i].map(|_| (bases[i], count(u))))?;
            if next < bases[i] {
                return Err(bad());
            }
            bases.push(next);
        }
        if bases.last().copied().unwrap() as u64 != (rows.end - rows.start) / 8 - 1 {
            return Err(bad());
        }
        Ok(Self {
            reader,
            rows,
            tail,
            bases,
        })
    }
    fn site(&self, id: usize) -> io::Result<Site> {
        let file = self
            .bases
            .partition_point(|&s| s <= id)
            .checked_sub(1)
            .ok_or_else(bad)?;
        if file + 1 == self.bases.len() {
            return Err(bad());
        }
        Ok(Site {
            file,
            local: id - self.bases[file],
        })
    }
    fn batch_end(
        &mut self,
        id: usize,
        limit: usize,
        from: u64,
        budget: usize,
    ) -> io::Result<Option<(usize, u64)>> {
        // Up to 1024 adjacent records per read. An oversized row falls back
        // to binding once if its encoded targets would exceed the allowance.
        let mut next = limit.min(id + 1024);
        let mut to = self.offset(next)?;
        while to.checked_sub(from).ok_or_else(bad)? > budget as u64 {
            if next == id + 1 {
                return Ok(None);
            }
            next = id + (next - id) / 2;
            to = self.offset(next)?;
        }
        Ok(Some((next, to)))
    }

    fn restore_targets(
        &self,
        d: &mut Decoder,
        source: Site,
        unchanged: &[Option<usize>],
        names: &mut ForwardNames,
    ) -> io::Result<bool> {
        let location = decode_location(d, unchanged.len(), *self.bases.last().unwrap(), 0)?;
        if location.file != source.file {
            return Err(bad());
        }
        for encoded in d.words()? {
            let target = self.site(u64::from_le_bytes(*encoded) as usize)?;
            if unchanged[target.file].is_none() {
                return Ok(false);
            }
            names.bind_one(source, target);
        }
        Ok(true)
    }

    fn offset(&mut self, id: usize) -> io::Result<u64> {
        let pos = word(&mut self.reader, self.rows.start + id as u64 * 8)?;
        if pos > self.tail {
            return Err(bad());
        }
        Ok(pos)
    }
}

fn label_bound(r: &mut impl ExtentRead, labels: &Range<u64>, id: usize) -> io::Result<u64> {
    let count = (labels.end - labels.start) / 24;
    let lo = lower_bound(0..count, |mid| Ok(word(r, labels.start + 24 * mid)? < id as u64))?;
    Ok(labels.start + 24 * lo)
}

fn labels<R: ExtentRead>(
    decl: &mut Directory<R>,
    index: &Range<u64>,
    files: &Range<usize>,
    names: &mut ForwardNames,
) -> io::Result<()> {
    let start = label_bound(&mut decl.reader, index, decl.bases[files.start])?;
    let end = label_bound(&mut decl.reader, index, decl.bases[files.end])?;
    for at in (start..end).step_by(24) {
        let id = word(&mut decl.reader, at)? as usize;
        let site = decl.site(id)?;
        let mut prefix = word(&mut decl.reader, at + 8)?;
        let mut suffix = word(&mut decl.reader, at + 16)?;
        let mut label = text(&mut decl.reader, &mut prefix, index.start)?;
        label.push_str(&text(&mut decl.reader, &mut suffix, index.start)?);
        names.public_labels.insert(site, label);
    }
    Ok(())
}

struct RestoreTargets<'a> {
    unchanged: &'a [Option<usize>],
    names: &'a mut ForwardNames,
}

impl RestoreTargets<'_> {
    fn read_batch<R: ExtentRead, S: ExtentRead>(
        &mut self,
        uses: &mut Directory<R>,
        decl: &Directory<S>,
        (ids, from, budget): (Range<usize>, u64, usize),
    ) -> io::Result<Option<(usize, u64)>> {
        let Some((next, to)) = uses.batch_end(ids.start, ids.end, from, budget)? else {
            return Ok(None);
        };
        let bytes = read_bytes(&mut uses.reader, from..to)?;
        Ok(self.batch(uses, decl, ids.start..next, &bytes)?.then_some((next, to)))
    }

    fn batch(
        &mut self,
        uses: &Directory<impl ExtentRead>,
        declarations: &Directory<impl ExtentRead>,
        ids: Range<usize>,
        bytes: &[u8],
    ) -> io::Result<bool> {
        let mut decoder = Decoder(bytes);
        for ordinal in ids {
            let source = uses.site(ordinal)?;
            if !declarations.restore_targets(&mut decoder, source, self.unchanged, self.names)? {
                return Ok(false);
            }
        }
        decoder.end()?;
        Ok(true)
    }
}

fn edges<R: ExtentRead, S: ExtentRead>(
    uses: &mut Directory<R>,
    decl: &Directory<S>,
    files: Range<usize>,
    mut restore: RestoreTargets<'_>,
    budget: usize,
) -> io::Result<bool> {
    let mut id = uses.bases[files.start];
    let limit = uses.bases[files.end];
    let mut from = uses.offset(id)?;
    while id < limit {
        let Some(next) = restore.read_batch(uses, decl, (id..limit, from, budget))? else {
            return Ok(false);
        };
        (id, from) = next;
    }
    Ok(true)
}

pub(super) struct Inputs<'a> {
    pub unchanged: &'a [Option<usize>],
    pub budget: usize,
    pub before: Option<&'a std::collections::BTreeMap<usize, String>>,
}

pub(super) fn bind(
    units: &[Unit],
    previous: Option<&dyn Previous>,
    inputs: Inputs<'_>,
) -> io::Result<ForwardNames> {
    let Inputs { unchanged, budget, before } = inputs;
    let Some(p) = previous.filter(|p| p.compatible(REL) && p.compatible(SYMBOLS)) else {
        return Ok(ForwardNames::forward(units));
    };
    let project = code::ProjectView::with_changes(units, before);
    let Some(keep) =
        code::retained_files(&project, unchanged, |id| Ok(p.local(id, true, false)?.code))?
    else {
        return Ok(ForwardNames::bind_project(&project));
    };
    let mut decl = Directory::open(p.graph_reader(SYMBOLS)?, units, unchanged, |u| {
        u.analysis.declarations.len()
    })?;
    let mut uses = Directory::open(p.graph_reader(REL)?, units, unchanged, |u| {
        u.analysis.calls.len()
    })?;
    let index = public_labels::directory(&mut decl.reader, decl.tail)?;
    let mut names = ForwardNames::with_rows(units);
    let mut start = 0;
    for run in keep.chunk_by(|a, b| a == b) {
        let files = start..start + run.len();
        start = files.end;
        if run[0] {
            labels(&mut decl, &index, &files, &mut names)?;
            if !edges(&mut uses, &decl, files, RestoreTargets { unchanged, names: &mut names }, budget)? {
                return Ok(ForwardNames::bind_project(&project));
            }
        }
    }
    names.complete_partitions(&project, &keep);
    Ok(names)
}
