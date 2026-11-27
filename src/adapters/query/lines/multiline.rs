use super::Lines;
use std::ops::Range;

/// A match selection and the decoded input whose byte offsets it uses.
pub(super) struct FullText<'a> {
    pub view: std::borrow::Cow<'a, str>,
    pub selection: Selection,
}

impl<'a> FullText<'a> {
    pub fn new(query: &Lines, bytes: &'a [u8], scratch: &mut super::RegexScratch) -> Option<Self> {
        if !query.multiline { return None; }
        let view = query.view(bytes);
        let selection = Selection::new(query, query.search_text(bytes, &view), scratch);
        Some(Self { view, selection })
    }

    pub fn original_offset(&self, bytes: &[u8], byte_input: bool, offset: usize) -> usize {
        if offset == self.view.len() { return bytes.len(); }
        super::offsets::cursor(bytes, &self.view, byte_input)(offset)
    }

    pub fn searched(&self, bytes: &[u8], byte_input: bool, term: u8) -> usize {
        let end = self.selection.searched(&self.view, char::from(term));
        self.original_offset(bytes, byte_input, end)
    }
}

/// The regex limit applies to full-text occurrences, before projection onto
/// whole records. Trailing context is checked one record at a time and may
/// never extend the context window or the last admitted cross-record match.
pub(super) struct Selection {
    pub spans: Vec<Range<usize>>,
    context_start: Option<usize>,
    pub end: usize,
}

fn context_body(line: &str, terminator: char, crlf: bool) -> &str {
    let body = line.strip_suffix(terminator).unwrap_or(line);
    if crlf { body.strip_suffix('\r').unwrap_or(body) } else { body }
}

impl Selection {
    pub fn new(query: &Lines, text: super::SearchText<'_>, scratch: &mut super::RegexScratch) -> Self {
        let source = text.decoded;
        let mut spans = query.ranges(text, scratch);
        let term = if query.null_data { '\0' } else { '\n' };
        let last = query.max.filter(|_| !query.invert)
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| spans.get(i));
        let Some(matched) = last else {
            return Self { spans, context_start: None, end: source.len() };
        };
        let start = record_end(source, matched, term);
        spans.truncate(spans.partition_point(|r| r.start < start && r.end <= start));
        let end = Self::rescan_context(query, text, scratch, (start, &mut spans));
        let end = if query.passthru { source.len() } else { end };
        Self { spans, context_start: Some(start), end }
    }

    fn rescan_context(
        query: &Lines,
        text: super::SearchText<'_>,
        scratch: &mut super::RegexScratch,
        (start, spans): (usize, &mut Vec<Range<usize>>),
    ) -> usize {
        let term = if query.null_data { '\0' } else { '\n' };
        let mut end = start;
        let mut ranges = Vec::new();
        let mut slice = text.slice_cursor();
        for line in text.decoded[start..].split_inclusive(term).take(query.after) {
            let body = context_body(line, term, query.crlf);
            query.fill_ranges(slice(end..end + body.len()), scratch, &mut ranges);
            spans.extend(ranges.iter().map(|r| end + r.start..end + r.end));
            end += line.len();
        }
        end
    }

    /// The full-text search reaches EOF. A matching trailing context record
    /// restarts the single-record search cursor at the end of that record.
    pub fn searched(&self, source: &str, term: char) -> usize {
        match (self.context_start, self.spans.last()) {
            (Some(start), Some(last)) if last.start >= start => record_end(source, last, term),
            _ => source.len(),
        }
    }

    pub fn blocks<'a>(&'a self, source: &'a str, term: char) -> Blocks<'a> {
        Blocks {
            source,
            spans: self.spans.iter().enumerate().peekable(),
            term,
            context_start: self.context_start.unwrap_or(usize::MAX),
        }
    }
}

impl Lines<'_> {
    pub(super) fn record_limit(&self) -> Option<usize> {
        self.max.filter(|_| !self.multiline || self.invert)
    }
}

/// Matched records retain their source coordinates through replacement, even
/// when the replacement inserts or removes a record terminator.
pub(super) struct Block {
    pub start: usize,
    pub end: usize,
    pub hits: Range<usize>,
}
/// Group the ordered match spans as the consumer advances through the file.
pub(super) struct Blocks<'a> {
    source: &'a str,
    spans: std::iter::Peekable<std::iter::Enumerate<std::slice::Iter<'a, Range<usize>>>>,
    term: char,
    context_start: usize,
}

impl Iterator for Blocks<'_> {
    type Item = Block;

    fn next(&mut self) -> Option<Block> {
        let (first, span) = self.spans.next()?;
        if span.start == self.source.len() { return None; }
        let start = self.source[..span.start].rfind(self.term).map_or(0, |at| at + 1);
        let mut end = record_end(self.source, span, self.term);
        let mut limit = first + 1;
        while let Some((i, span)) = self.spans.next_if(|(_, span)| {
            if span.start < end { return true; }
            span.start < self.source.len() && end < self.context_start
                && !self.source[end..span.start].contains(self.term)
        }) {
            if span.end > end || span.start == end { end = record_end(self.source, span, self.term); }
            limit = i + 1;
        }
        Some(Block { start, end, hits: first..limit })
    }
}

pub(super) struct ProjectedRow<'a> {
    pub row: usize,
    pub offset: usize,
    pub column: Option<usize>,
    pub bytes: &'a [u8],
}

/// Emit each selected block once, from the same source and spans that selected it.
pub(super) struct BlockOutput<'a, 'arg> {
    query: &'a Lines<'arg>,
    source: &'a str,
    raw: &'a [u8],
    spans: &'a [Range<usize>],
    template: Option<&'a super::replace::Template<'arg>>,
    pending: std::iter::Peekable<Blocks<'a>>,
}

impl<'a, 'arg> BlockOutput<'a, 'arg> {
    pub fn new(
        query: &'a Lines<'arg>,
        full: Option<&'a FullText<'_>>,
        raw: &'a [u8],
        styled: bool,
    ) -> Option<Self> {
        if query.invert {
            return None;
        }
        let full = full?;
        let template = match (query.replacement.as_ref(), query.only, styled) {
            (None, true, false) => None,
            (None, _, _) => return None,
            (template, _, _) => template,
        };
        let source = full.view.as_ref();
        let selection = &full.selection;
        let spans = &selection.spans;
        let pending = selection.blocks(source, if query.null_data { '\0' } else { '\n' }).peekable();
        Some(Self {
            query,
            source,
            raw,
            spans,
            template,
            pending,
        })
    }

    /// A block renders once on its first record; all of its records are covered.
    pub fn render(
        &mut self,
        record: &super::offsets::InputRecord,
        body: &mut super::replace::Buffer,
        emit: impl FnMut(ProjectedRow),
    ) -> bool {
        let line_start = record.decoded_range.start;
        while self
            .pending
            .next_if(|block| block.end <= line_start)
            .is_some()
        {}
        let Some(block) = self.pending.peek() else {
            return false;
        };
        if block.start == line_start {
            let text = BlockText {
                query: self.query, template: self.template, source: self.source,
                raw: &self.raw[record.byte_start..], bounds: block.start..block.end,
            };
            text.render(&self.spans[block.hits.clone()], body, emit);
        }
        block.start <= line_start
    }
}

fn record_end(source: &str, matched: &Range<usize>, term: char) -> usize {
    if !matched.is_empty() && source.as_bytes()[matched.end - 1] == term as u8 {
        matched.end
    } else {
        source[matched.end..]
            .find(term)
            .map_or(source.len(), |i| matched.end + i + 1)
    }
}

struct BlockText<'a, 'arg> {
    raw: &'a [u8],
    bounds: Range<usize>,
    query: &'a Lines<'arg>,
    template: Option<&'a super::replace::Template<'arg>>,
    source: &'a str,
}

impl BlockText<'_, '_> {
    fn render(&self, spans: &[Range<usize>],
              body: &mut super::replace::Buffer, emit: impl FnMut(ProjectedRow)) {
        let raw = self.raw;
        let block = &self.bounds;
        let mut offset = super::offsets::cursor(raw, &self.source[block.start..], self.query.byte_input);
        let mut consumed = 0;
        let edits = spans.iter().map(|span| {
            let start = offset(span.start - block.start);
            let gap = &raw[consumed..start];
            consumed = offset(span.end - block.start);
            (gap, super::replace::Match { text: self.source, span: span.clone(), raw, bytes: start..consumed })
        });
        let Some(template) = self.template.filter(|_| !self.query.only) else {
            self.slices(edits, body, emit);
            return;
        };
        body.bytes.clear();
        body.columns.clear();
        let term = if self.query.null_data { 0 } else { b'\n' };
        for (gap, matched) in edits {
            body.bytes.extend_from_slice(gap);
            let start = body.bytes.len();
            template.append(body, self.query, matched);
            if self.query.vimgrep { body.mark_start(start, term); }
        }
        body.bytes.extend_from_slice(&raw[consumed..offset(block.end-block.start)]);
        body.rows(self.query.vimgrep, term, self.query.crlf && !self.query.null_data, emit);
    }

    fn slices<'a>(&self, edits: impl Iterator<Item=(&'a [u8], super::replace::Match<'a>)>,
                  body: &mut super::replace::Buffer, mut emit: impl FnMut(ProjectedRow)) {
        let mut position = SlicePosition::default();
        let ending = self.query.generated_record_ending();
        for (gap, matched) in edits {
            let bytes = self.render_span(body, matched);
            position.write(gap, bytes, ending, &mut emit);
        }
    }

    fn render_span<'a>(&self, body: &'a mut super::replace::Buffer, matched: super::replace::Match<'a>) -> &'a [u8] {
        let Some(template) = self.template else { return &matched.raw[matched.bytes] };
        body.bytes.clear();
        template.append(body, self.query, matched);
        &body.bytes
    }
}

/// Coordinate in the rendered block, after applying preceding replacements.
#[derive(Default)]
struct SlicePosition {
    line: usize,
    end: usize,
}

impl SlicePosition {
    fn write(&mut self, gap: &[u8], bytes: &[u8], ending: &[u8], mut emit: impl FnMut(ProjectedRow)) {
        let term = ending[ending.len() - 1];
        self.line += gap.iter().filter(|&&byte| byte == term).count();
        let start = self.end + gap.len();
        for record in output_records(bytes, term) {
            if !record.bytes.is_empty() {
                emit(ProjectedRow { row: self.line, offset: start, column: Some(start + 1), bytes: record.content(ending == b"\r\n") });
            }
            self.line += usize::from(record.terminated);
        }
        self.end = start + bytes.len();
    }
}

impl super::replace::Buffer {
    fn mark_start(&mut self, start: usize, term: u8) {
        let at_row_start = start == 0 || self.bytes[start-1] == term;
        if self.bytes.len() != start || !at_row_start { self.columns.push(start); }
    }

    fn rows(&self, repeated: bool, term: u8, crlf: bool, mut emit: impl FnMut(ProjectedRow)) {
        let records = output_records(&self.bytes, term).enumerate();
        if repeated {
            let mut starts = self.columns.iter().copied().peekable();
            for (row, record) in records {
                while let Some(start) = starts.next_if(|&at| at < record.end()) {
                    emit(ProjectedRow { row, offset: record.offset, column: Some(start - record.offset + 1), bytes: record.content(crlf) });
                }
            }
            return;
        }
        for (row, record) in records {
            emit(ProjectedRow { row, offset: record.offset, column: None, bytes: record.content(crlf) });
        }
    }
}

struct OutputRecord<'a> {
    offset: usize,
    bytes: &'a [u8],
    terminated: bool,
}

impl<'a> OutputRecord<'a> {
    fn content(&self, crlf: bool) -> &'a [u8] {
        if crlf && self.terminated { self.bytes.strip_suffix(b"\r").unwrap_or(self.bytes) } else { self.bytes }
    }

    fn end(&self) -> usize {
        self.offset + self.bytes.len() + usize::from(self.terminated)
    }
}

fn output_records(bytes: &[u8], term: u8) -> impl Iterator<Item = OutputRecord<'_>> {
    let mut next = 0;
    bytes.split_inclusive(move |&byte| byte == term).map(move |part| {
        let offset = next;
        next += part.len();
        let content = part.strip_suffix(&[term]);
        OutputRecord { offset, bytes: content.unwrap_or(part), terminated: content.is_some() }
    })
}
