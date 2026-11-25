//! Serialize the Markdown directory and file catalog against shared parser facts.
use super::{Entry, Local, Previous, retained};
use crate::application::index::{
    binary::{put, string},
    layout::{ExtentSink, FILES},
};
use std::{collections::BTreeMap, io};

pub(super) fn write_markdown(w: &mut impl ExtentSink, locals: &[Local]) -> io::Result<()> {
    let start = w.offset();
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
    crate::application::index::offsets::finish(w, start, [docs, destinations])?;
    Ok(())
}

pub(super) fn write(
    w: &mut impl ExtentSink,
    previous: Option<&dyn Previous>,
    files: &mut [Entry],
    locals: &[Local],
    reuse: &[Option<usize>],
    offsets: &[u64],
) -> io::Result<()> {
    let start = w.offset();
    w.write_all(crate::application::index::CATALOG_MAGIC)?;
    put(w, files.len())?;
    let mut writer = retained::Run::new(previous, w);
    let mut row = Vec::new();
    for (i, ((f, l), prior)) in files
        .iter_mut()
        .zip(locals)
        .zip(reuse.iter().copied())
        .enumerate()
    {
        let row_start = writer.offset() - start;
        f.evidence = offsets[i]..offsets[i + 1];
        let ends = &l.lf[..l.lf.len() - usize::from(l.partial_line)];
        if let Some((p, old)) = previous.zip(prior)
            && let old = &p.entries()[old]
            && !old.catalog_row.is_empty()
            && old.path() == f.path()
            && old.score == f.score
            && old.test_score == f.test_score
            && old.blob == f.blob
            && old.catalog_stamp() == f.stamp()
        {
            let base = p.extent(FILES).start;
            writer.borrow(base + old.catalog_row.start..base + old.catalog_row.end)?;
        } else {
            row.clear();
            f.write_with_lines(&mut row, ends)?;
            writer.bytes(&row)?;
        }
        f.catalog_row = row_start..writer.offset() - start;
    }
    writer.bytes(&offsets[0].to_le_bytes())?;
    for (pair, prior) in offsets.windows(2).zip(reuse) {
        let len = pair[1] - pair[0];
        if let Some((p, id)) = previous.zip(*prior)
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
    Ok(())
}
