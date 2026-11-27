//! Workers write directly into the ordinal columns borrowed by later stages.
use super::{BlobId, Captured, Local, Source, analyze, local_job};

pub(super) struct CaptureTable {
    pub sources: Vec<Option<Source>>,
    pub blobs: Vec<Option<BlobId>>,
    pub parsed: Vec<Option<Local>>,
    pub retained_rows: Vec<Option<usize>>,
}

pub(super) struct Row<'a> {
    source: &'a mut Option<Source>,
    blob: &'a mut Option<BlobId>,
    local: &'a mut Option<Local>,
    prior: &'a mut Option<usize>,
}

impl CaptureTable {
    pub fn new(len: usize) -> Self {
        Self {
            sources: (0..len).map(|_| None).collect(),
            blobs: vec![None; len],
            parsed: (0..len).map(|_| None).collect(),
            retained_rows: vec![None; len],
        }
    }

    pub fn row(&mut self, id: usize) -> Row<'_> {
        Row {
            source: &mut self.sources[id],
            blob: &mut self.blobs[id],
            local: &mut self.parsed[id],
            prior: &mut self.retained_rows[id],
        }
    }

    pub fn rows(&mut self) -> impl Iterator<Item = Row<'_>> {
        self.sources
            .iter_mut()
            .zip(&mut self.blobs)
            .zip(&mut self.parsed)
            .zip(&mut self.retained_rows)
            .map(|(((source, blob), local), prior)| Row {
                source,
                blob,
                local,
                prior,
            })
    }
}

pub(super) enum Work<'a> {
    Read(usize, &'a mut super::ObservedPath, Row<'a>, Option<usize>),
    Analyze(Box<super::local_job::Job<'a>>),
}

impl<'a> Row<'a> {
    /// A scratch body needs its mapped backing. A large heap body can instead
    /// yield between slices while the destination slot holds its text alive.
    pub(super) fn analyze(self, path: &'a str, capture: Captured) -> Option<Work<'a>> {
        let row = self.store(capture);
        match row.source.as_ref() {
            Some(Source::Scratch(_)) => {}
            Some(Source::Heap(text)) if text.len() > local_job::SLICE => {
                return Some(Work::Analyze(Box::new(local_job::Job::new(
                    row.local,
                    path,
                    Some(text),
                    None,
                ))));
            }
            source => *row.local = Some(analyze(path, source.map(|s| s.text(None)))),
        }
        None
    }
    pub fn store(self, capture: Captured) -> Self {
        *self.source = capture.source;
        *self.blob = capture.blob;
        *self.local = capture.local;
        *self.prior = capture.prior;
        self
    }
}
