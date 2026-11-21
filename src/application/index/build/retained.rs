//! Copy runs of immutable bytes in one read, including across adjacent files.
use super::{Local, Previous};
use crate::application::index::{Entry, layout::ExtentSink};
use crate::domain::code::Language;
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
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
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

pub(super) fn evidence(
    sink: &mut impl ExtentSink,
    prior: Option<&dyn Previous>,
    reuse: &[Option<usize>],
    locals: &[Local],
    files: &[Entry],
) -> io::Result<Vec<u64>> {
    let mut run = Run::new(prior, sink);
    let mut offsets = Vec::with_capacity(files.len() + 1);
    let mut scratch = Vec::new();
    for ((local, file), &old) in locals.iter().zip(files).zip(reuse) {
        offsets.push(run.offset());
        if let Some(id) = old {
            run.borrow(prior.unwrap().entries()[id].evidence.clone())?;
        } else if file.bytes.is_some() && Language::for_path(file.path()).is_some() {
            scratch.clear();
            local.code.encode(&mut scratch);
            run.bytes(&scratch)?;
        }
    }
    offsets.push(run.offset());
    run.flush()?;
    Ok(offsets)
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
