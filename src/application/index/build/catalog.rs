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
    let documents = locals.iter().enumerate().filter_map(|(id, local)| {
        local.markdown.as_deref().map(|markdown| (id, markdown))
    });
    for (id, markdown) in documents {
        docs.push(w.offset() - start);
        put(w, id)?;
        markdown.write_to(&mut *w)?;
        add_backlinks(&mut backlinks, id, markdown);
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

fn add_backlinks<'a>(
    backlinks: &mut BTreeMap<(&'a str, &'a str), Vec<(usize, usize)>>,
    id: usize,
    markdown: &'a crate::domain::markdown::Parsed,
) {
    for (n, link) in markdown.links.iter().enumerate() {
        backlinks.entry((&link.target, &link.anchor)).or_default().push((id, n));
    }
}

pub(super) struct Rows<'a> {
    pub previous: Option<&'a dyn Previous>,
    pub locals: &'a [Local],
    pub reuse: &'a [Option<usize>],
    pub offsets: &'a [u64],
}

impl Rows<'_> {
    pub(super) fn write(self, w: &mut impl ExtentSink, files: &mut [Entry]) -> io::Result<()> {
        let start = w.offset();
        w.write_all(crate::application::index::CATALOG_MAGIC)?;
        put(w, files.len())?;
        let mut writer = retained::Run::new(self.previous, w);
        let mut row = Vec::new();
        for (i, ((file, local), prior)) in files.iter_mut().zip(self.locals).zip(self.reuse).enumerate() {
            let row_start = writer.offset() - start;
            file.evidence = self.offsets[i]..self.offsets[i + 1];
            let previous = self.prior(*prior);
            write_file(&mut writer, &mut row, (file, local), previous)?;
            file.catalog_row = row_start..writer.offset() - start;
        }
        writer.bytes(&self.offsets[0].to_le_bytes())?;
        self.write_evidence_lengths(&mut writer)?;
        writer.flush()
    }

    fn prior(&self, id: Option<usize>) -> Option<PriorRow<'_>> {
        let (previous, id) = self.previous.zip(id)?;
        let entry = &previous.entries()[id];
        (!entry.catalog_row.is_empty()).then_some(PriorRow { previous, id, entry })
    }

    fn write_evidence_lengths(&self, writer: &mut retained::Run<'_, impl ExtentSink>) -> io::Result<()> {
        for (pair, prior) in self.offsets.windows(2).zip(self.reuse) {
            let len = pair[1] - pair[0];
            let old_word = self.prior(*prior).and_then(|prior| prior.evidence_word(len));
            match old_word {
                Some(word) => writer.borrow(word..word + 8)?,
                None => writer.bytes(&len.to_le_bytes())?,
            }
        }
        Ok(())
    }
}

struct PriorRow<'a> {
    previous: &'a dyn Previous,
    id: usize,
    entry: &'a Entry,
}

impl PriorRow<'_> {
    fn matches(&self, file: &Entry) -> bool {
        let old = self.entry;
        let same_source = (old.path(), old.blob, old.catalog_stamp()) == (file.path(), file.blob, file.stamp());
        same_source && (&old.score, &old.test_score, old.score_marker) == (&file.score, &file.test_score, file.score_marker)
    }

    fn evidence_word(&self, length: u64) -> Option<u64> {
        (self.entry.evidence.end - self.entry.evidence.start == length).then(|| {
            self.previous.extent(FILES).end - 8 * (self.previous.entries().len() - self.id) as u64
        })
    }

    fn range(&self) -> std::ops::Range<u64> {
        let base = self.previous.extent(FILES).start;
        base + self.entry.catalog_row.start..base + self.entry.catalog_row.end
    }
}

fn write_file(
    writer: &mut retained::Run<'_, impl ExtentSink>,
    scratch: &mut Vec<u8>,
    (file, local): (&Entry, &Local),
    previous: Option<PriorRow<'_>>,
) -> io::Result<()> {
    if let Some(prior) = previous.filter(|prior| prior.matches(file)) {
        return writer.borrow(prior.range());
    }
    scratch.clear();
    file.write_with_lines(scratch, &local.line_ends)?;
    writer.bytes(scratch)
}
