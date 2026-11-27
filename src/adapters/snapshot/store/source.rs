//! Source reads borrow plain-folder data and assemble durable segment slices on demand.
use super::{Pin, binary, manifest};
use std::{
    borrow::Cow,
    io::{self, Write},
    ops::Range,
};

impl Pin {
    pub(crate) fn source_window(&self) -> SourceWindow<'_> {
        SourceWindow {
            pin: self,
            start: 0,
            data: Vec::new(),
        }
    }
    pub fn source(&self, id: usize) -> io::Result<String> {
        self.source_text(id).map(Cow::into_owned)
    }

    pub(crate) fn source_text(&self, id: usize) -> io::Result<Cow<'_, str>> {
        let range = self
            .source_extent(id)
            .ok_or_else(|| io::Error::other("path has no captured source"))?;
        self.read_text(range)
    }

    fn source_extent(&self, id: usize) -> Option<Range<u64>> {
        let file = self.files.get(id)?.bytes.as_ref()?;
        let start = self.dir.0[manifest::SOURCE - 1].start;
        Some(start + file.start..start + file.end)
    }

    /// Compare verified segment bytes directly with a captured UTF-8 source.
    pub(in crate::adapters::snapshot) fn same_source(
        &self,
        id: usize,
        text: &str,
    ) -> io::Result<bool> {
        let range = self.source_extent(id).ok_or_else(binary::bad)?;
        crate::application::index::build::Previous::same_bytes(self, range, text.as_bytes())
    }

    pub fn source_slice(&self, id: usize, range: Range<u64>) -> io::Result<String> {
        self.text_range(id, range).map(Cow::into_owned)
    }

    pub(crate) fn text_range(&self, id: usize, range: Range<u64>) -> io::Result<Cow<'_, str>> {
        let source = self.source_extent(id).ok_or_else(binary::bad)?;
        if range.start > range.end || range.end > source.end - source.start {
            return Err(binary::bad());
        }
        self.read_text(source.start + range.start..source.start + range.end)
    }

    fn read_text(&self, physical: Range<u64>) -> io::Result<Cow<'_, str>> {
        if let Some(bytes) = self.segment.memory_range(physical.clone())? {
            return std::str::from_utf8(bytes)
                .map(Cow::Borrowed)
                .map_err(io::Error::other);
        }
        let mut b = Vec::new();
        b.try_reserve_exact(
            (physical.end - physical.start)
                .try_into()
                .map_err(io::Error::other)?,
        )
        .map_err(io::Error::other)?;
        self.segment.visit_cached(physical, |s| b.write_all(s))?;
        String::from_utf8(b)
            .map(Cow::Owned)
            .map_err(io::Error::other)
    }
}

/// Sequential scans share verified read-ahead across neighboring documents.
/// Each returned str is still bounded by its file's exact byte range.
pub(crate) struct SourceWindow<'a> {
    pin: &'a Pin,
    start: u64,
    data: Vec<u8>,
}
impl SourceWindow<'_> {
    pub fn get(&mut self, id: usize) -> io::Result<&str> {
        let range = self.pin.source_extent(id).ok_or_else(binary::bad)?;
        let source = &self.pin.dir.0[manifest::SOURCE - 1];
        if let Some(bytes) = self.pin.segment.memory_range(range.clone())? {
            return std::str::from_utf8(bytes).map_err(io::Error::other);
        }
        if range.is_empty() {
            return Ok("");
        }
        if range.start < self.start || range.end - self.start > self.data.len() as u64 {
            self.start = range.start;
            self.data.clear();
            let end = range
                .end
                .max(range.start.saturating_add(256 * 1024))
                .min(source.end);
            self.pin
                .segment
                .visit(range.start..end, |bytes| self.data.write_all(bytes))?;
        }
        std::str::from_utf8(
            &self.data[(range.start - self.start) as usize..(range.end - self.start) as usize],
        )
        .map_err(io::Error::other)
    }
}
