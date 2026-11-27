//! Copy runs of immutable bytes in one read, including across adjacent files.
use super::{Local, Previous};
use crate::application::index::{Entry, layout::ExtentSink};
use crate::domain::code::{Analysis, Language};
use std::{io, ops::Range};

pub(super) fn range(
    prior: &dyn Previous,
    sink: &mut impl ExtentSink,
    range: Range<u64>,
) -> io::Result<()> {
    if !range.is_empty() && !sink.retain(range.clone())? {
        prior.copy_bytes(range, sink)?;
    }
    Ok(())
}

pub(super) struct Run<'a, W> {
    prior: Option<&'a dyn Previous>,
    sink: &'a mut W,
    pending: Option<Range<u64>>,
}

impl<'a, W: ExtentSink> Run<'a, W> {
    pub(super) fn new(prior: Option<&'a dyn Previous>, sink: &'a mut W) -> Self {
        Self {
            prior,
            sink,
            pending: None,
        }
    }

    pub(super) fn offset(&self) -> u64 {
        self.sink.offset() + self.pending.as_ref().map_or(0, |r| r.end - r.start)
    }

    pub(super) fn borrow(&mut self, range: Range<u64>) -> io::Result<()> {
        if range.is_empty() {
            return Ok(());
        }
        if let Some(last) = &mut self.pending
            && last.end == range.start
        {
            last.end = range.end;
        } else {
            self.flush()?;
            self.pending = Some(range);
        }
        Ok(())
    }

    /// Keep the equal prefix and suffix even when a variable-length record grows.
    pub(super) fn replacement(&mut self, base: u64, old: &[u8], new: &[u8]) -> io::Result<()> {
        let (prefix, suffix) = crate::domain::matching_edges(old, new);
        self.borrow(base..base + prefix as u64)?;
        if prefix < new.len() - suffix {
            self.bytes(&new[prefix..new.len() - suffix])?;
        }
        self.borrow(base + (old.len() - suffix) as u64..base + old.len() as u64)
    }

    pub(super) fn bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.flush()?;
        self.sink.write_all(bytes)
    }

    pub(super) fn flush(&mut self) -> io::Result<()> {
        if let Some(range) = self.pending.take() {
            let prior = self
                .prior
                .expect("retained bytes have a previous generation");
            self::range(prior, self.sink, range)?;
        }
        Ok(())
    }
}

pub(super) enum Evidence<'a> {
    Retained(usize),
    Encoded(&'a Analysis),
    Absent,
}

impl<'a> Evidence<'a> {
    pub fn classify(path: &str, source: bool, code: &'a Analysis, old: Option<usize>) -> Self {
        if let Some(id) = old {
            Self::Retained(id)
        } else if source && (Language::for_path(path).is_some() || super::c_project::database(path)) {
            Self::Encoded(code)
        } else {
            Self::Absent
        }
    }
}

pub(super) struct EvidenceRows<'a> {
    pub prior: Option<&'a dyn Previous>,
    pub reuse: &'a [Option<usize>],
    pub locals: &'a [Local],
    pub files: &'a [Entry],
}

impl EvidenceRows<'_> {
    pub fn offsets(&self) -> io::Result<Vec<u64>> {
        let capacity = self.files.len().checked_add(1)
            .ok_or_else(|| io::Error::other("evidence table too large"))?;
        let mut offsets = Vec::new();
        offsets.try_reserve_exact(capacity).map_err(io::Error::other)?;
        Ok(offsets)
    }

    pub fn slice(&self, range: Range<usize>) -> Self {
        Self {
            prior: self.prior,
            reuse: &self.reuse[range.clone()],
            locals: &self.locals[range.clone()],
            files: &self.files[range],
        }
    }

    pub fn write(&self, sink: &mut impl ExtentSink) -> io::Result<Vec<u64>> {
        let mut offsets = self.offsets()?;
        self.append(sink, &mut offsets)?;
        offsets.push(sink.offset());
        Ok(offsets)
    }

    pub fn append(&self, sink: &mut impl ExtentSink, offsets: &mut Vec<u64>) -> io::Result<()> {
        let mut run = Run::new(self.prior, sink);
        let mut scratch = Vec::new();
        for ((local, file), &old) in self.locals.iter().zip(self.files).zip(self.reuse) {
            let evidence = Evidence::classify(file.path(), file.bytes.is_some(), &local.code, old);
            offsets.push(run.offset());
            match evidence {
                Evidence::Retained(id) => run.borrow(self.prior.unwrap().entries()[id].evidence.clone())?,
                Evidence::Encoded(code) => {
                    scratch.clear();
                    code.encode(&mut scratch);
                    run.bytes(&scratch)?;
                }
                Evidence::Absent => {}
            }
        }
        run.flush()
    }
}

impl<W: ExtentSink> std::io::Write for Run<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Run::flush(self)
    }
}
