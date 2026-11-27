//! Render selected match spans against their original source bytes.
use super::{Lines, Range, offsets};

pub(super) struct MatchedLine<'a> {
    pub raw: &'a [u8],
    pub decoded: &'a str,
    pub hay: &'a str,
    pub ranges: &'a [Range<usize>],
    pub column: usize,
    pub terminated: bool,
    line: usize,
    byte_start: usize,
}

#[derive(Clone, Copy)]
enum Projection<'a, 'arg> {
    Source,
    DecoratedRow(Decoration<'a, 'arg>),
    WholeLine(Option<Decoration<'a, 'arg>>),
    Slices(Slices<'a, 'arg>),
}

#[derive(Clone, Copy)]
enum Slices<'a, 'arg> {
    Replacement(&'a super::replace::Template<'arg>),
    Decorated(Decoration<'a, 'arg>),
    Record,
    Projected,
}

impl<'a, 'arg> Slices<'a, 'arg> {
    fn select(multiline: bool, decoration: Option<Decoration<'a, 'arg>>) -> Self {
        match (multiline, decoration) {
            (false, Some(Decoration::Replacement(template))) => Self::Replacement(template),
            (_, Some(mode)) => Self::Decorated(mode),
            (true, None) => Self::Projected,
            (false, None) => Self::Record,
        }
    }

    fn render(
        self,
        line: MatchedLine<'_>,
        query: &Lines,
        styled: &mut super::replace::Buffer,
        mut emit: impl FnMut(&[u8], usize),
    ) {
        match self {
            Self::Replacement(template) => line.replace_slices(query, template, styled, emit),
            Self::Record => line.slices(query, emit),
            Self::Projected => line.projected_slices(query, emit),
            Self::Decorated(decoration) => line.decorated_slices(query, decoration, styled, &mut emit),
        }
    }
}

#[derive(Clone, Copy)]
enum Decoration<'a, 'arg> {
    Replacement(&'a super::replace::Template<'arg>),
    Styled(&'a str),
}

impl Decoration<'_, '_> {
    fn append(&self, query: &Lines, line: &MatchedLine, matched: (&Range<usize>, Range<usize>), out: &mut super::replace::Buffer) {
        let (span, raw) = matched;
        match self {
            Self::Replacement(template) => template.append(out, query, line.matched(span.clone(), raw)),
            Self::Styled(style) => append_style(&mut out.bytes, style, &line.raw[raw]),
        }
    }
}

impl MatchedLine<'_> {
    fn matched(&self, span: Range<usize>, bytes: Range<usize>) -> super::replace::Match<'_> {
        super::replace::Match { text: self.hay, raw: self.raw, span, bytes }
    }

    fn eof_only(&self) -> bool {
        matches!(self.ranges, [r] if r.is_empty() && r.start == self.decoded.len())
    }

    /// A terminated row admits an empty span just before its terminator.
    /// An empty span at absolute EOF has no printable column.
    fn visible_spans(&self, byte_input: bool) -> impl Iterator<Item = (&Range<usize>, Range<usize>)> {
        let limit = self.raw.len() + usize::from(self.terminated);
        let mut offset = offsets::cursor(self.raw, self.decoded, byte_input);
        self.ranges.iter().map(move |span| (span, offset(span.start)..offset(span.end)))
            .take_while(move |(_, bytes)| bytes.start < limit)
    }

    fn decorate_whole<'r>(&self, query: &Lines, decoration: Decoration,
        spans: impl Iterator<Item = (&'r Range<usize>, Range<usize>)>,
        buffer: &mut super::replace::Buffer) {
        buffer.bytes.clear();
        buffer.columns.clear();
        let mut previous = 0;
        for (span, bytes) in spans {
            buffer.bytes.extend_from_slice(&self.raw[previous..bytes.start]);
            if query.vimgrep { buffer.columns.push(buffer.bytes.len() + 1); }
            previous = bytes.end;
            decoration.append(query, self, (span, bytes), buffer);
        }
        buffer.bytes.extend_from_slice(&self.raw[previous..]);
    }

    fn whole_rows(
        &self, query: &Lines, decoration: Option<Decoration<'_, '_>>,
        buffer: &mut super::replace::Buffer, mut emit: impl FnMut(&[u8], Option<usize>),
    ) {
        // A whole-input zero-width hit at a new record's start has no preceding
        // bytes in that record. Only the record-local renderer prints it.
        let spans = self.visible_spans(query.byte_input)
            .filter(|(_, bytes)| !query.multiline || bytes.end != 0);
        if let Some(replacement @ Decoration::Replacement(_)) = decoration {
            self.decorate_whole(query, replacement, spans, buffer);
            for &column in &buffer.columns { emit(&buffer.bytes, Some(column)); }
            return;
        }
        let style = match decoration { Some(Decoration::Styled(style)) => Some(style), _ => None };
        for (_, bytes) in spans {
            let column = bytes.start + 1;
            emit(styled_row(self.raw, bytes, style, &mut buffer.bytes), Some(column));
        }
    }

    fn replace_slices(
        &self, query: &Lines, template: &super::replace::Template<'_>,
        buffer: &mut super::replace::Buffer, mut emit: impl FnMut(&[u8], usize),
    ) {
        let mut source_end = 0;
        let mut output_end = 0;
        for (range, bytes) in self.visible_spans(query.byte_input) {
            let column = output_end + (bytes.start - source_end) + 1;
            source_end = bytes.end;
            buffer.bytes.clear();
            template.append(buffer, query, self.matched(range.clone(), bytes));
            emit(&buffer.bytes, column);
            output_end = column - 1 + buffer.bytes.len();
        }
    }

    fn projected_slices(&self, query: &Lines, mut emit: impl FnMut(&[u8], usize)) {
        let mut offset = offsets::cursor(self.raw, self.decoded, query.byte_input);
        for r in self.ranges.iter().filter(|r| !r.is_empty() || query.match_scope == super::MatchScope::Word) {
            let start = offset(r.start);
            emit(&self.raw[start..offset(r.end)], start + 1);
        }
    }

    fn decorated_slices(
        &self, query: &Lines, decoration: Decoration, styled: &mut super::replace::Buffer,
        mut emit: impl FnMut(&[u8], usize),
    ) {
        let include_empty = decoration.replaces() || query.match_scope == super::MatchScope::Word;
        for (range, bytes) in self.decoration_spans(query.byte_input, include_empty) {
            styled.bytes.clear();
            let column = bytes.start + 1;
            decoration.append(query, self, (range, bytes), styled);
            emit(&styled.bytes, column);
        }
    }

    fn empty_byte_columns(&self, range: Range<usize>, mut emit: impl FnMut(&[u8], usize)) {
        for byte in range.filter(|&i| self.raw[i] & 0xc0 == 0x80) {
            emit(&[], byte + 1);
        }
    }

    fn slices(&self, query: &Lines, mut emit: impl FnMut(&[u8], usize)) {
        let mut offset = offsets::cursor(self.raw, self.decoded, query.byte_input);
        let mut next_byte = 0;
        let limit = self.raw.len() + usize::from(self.terminated);
        for range in self.ranges {
            let start = offset(range.start);
            let end = offset(range.end);
            self.render_empty_columns(query, next_byte..start, &mut emit);
            next_byte = end + 1;
            Self::emit_slice(self.raw, start..end, limit, &mut emit);
        }
    }

    fn render_empty_columns(&self, query: &Lines, span: Range<usize>, emit: &mut impl FnMut(&[u8], usize)) {
        if query.empty_inside_scalar {
            self.empty_byte_columns(span, emit);
        }
    }

    fn emit_slice(raw: &[u8], span: Range<usize>, limit: usize, emit: &mut impl FnMut(&[u8], usize)) {
        if span.start < limit {
            let column = span.start + 1;
            emit(&raw[span], column);
        }
    }
}

#[derive(Clone, Copy)]
enum EofRow {
    Spans,
    Source(Option<usize>),
    Skip,
}

impl EofRow {
    fn render(self, line: &MatchedLine, emit: &mut impl FnMut(&[u8], Option<usize>)) -> bool {
        if line.terminated || !line.eof_only() {
            return false;
        }
        match self {
            Self::Spans => false,
            Self::Skip => true,
            Self::Source(column) => {
                emit(line.raw, column);
                true
            }
        }
    }
}

/// Output for an unterminated record depends on the requested projection.
pub(super) struct LineProjection<'a, 'arg> {
    mode: Projection<'a, 'arg>,
    eof: EofRow,
    span_offset: bool,
    endings: RecordEndings,
}

impl<'a, 'arg> LineProjection<'a, 'arg> {
    pub fn new(query: &'a Lines<'arg>, style: Option<&'a str>) -> Self {
        let mode = Projection::select(query, style);
        let (eof, span_offset) = match (query.multiline, mode) {
            (false, Projection::WholeLine(_) | Projection::Slices(_)) => (EofRow::Source(None), true),
            (true, Projection::WholeLine(_)) => (EofRow::Skip, false),
            (true, Projection::Slices(Slices::Projected)) => (EofRow::Source(Some(1)), true),
            (_, Projection::Slices(_)) => (EofRow::Spans, true),
            _ => (EofRow::Spans, false),
        };
        Self { mode, eof, span_offset, endings: RecordEndings::new(query, mode) }
    }

    pub fn needs_spans(&self) -> bool {
        !matches!(self.mode, Projection::Source)
    }

    fn render(
        &self,
        line: MatchedLine<'_>,
        query: &Lines,
        styled: &mut super::replace::Buffer,
        mut emit: impl FnMut(&[u8], Option<usize>),
    ) {
        if self.eof.render(&line, &mut emit) {
            return;
        }
        match self.mode {
            Projection::Source => emit(line.raw, Some(line.column)),
            Projection::Slices(slices) => slices.render(line, query, styled, |bytes, column| emit(bytes, Some(column))),
            Projection::DecoratedRow(decoration) => {
                let spans = line.decoration_spans(query.byte_input, decoration.replaces());
                line.decorate_whole(query, decoration, spans, styled);
                emit(&styled.bytes, Some(line.column));
            }
            Projection::WholeLine(decoration) => line.whole_rows(query, decoration, styled, emit),
        }
    }

}

fn styled_row<'a>(raw: &'a [u8], span: Range<usize>, style: Option<&str>, out: &'a mut Vec<u8>) -> &'a [u8] {
    let Some(style) = style.filter(|_| !span.is_empty()) else { return raw };
    out.clear();
    out.extend_from_slice(&raw[..span.start]);
    append_style(out, style, &raw[span.clone()]);
    out.extend_from_slice(&raw[span.end..]);
    out
}

fn append_style(out: &mut Vec<u8>, style: &str, bytes: &[u8]) {
    out.extend_from_slice(b"\x1b[0m");
    out.extend_from_slice(style.as_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\x1b[0m");
}

impl<'a, 'arg> Projection<'a, 'arg> {
    fn select(query: &'a Lines<'arg>, style: Option<&'a str>) -> Self {
        // Inverted hits have no occurrence to slice, highlight or replace.
        if query.invert { return Self::Source; }
        let decoration = Decoration::select(query, style);
        Self::for_matches(query, decoration)
    }

    fn for_matches(query: &Lines, decoration: Option<Decoration<'a, 'arg>>) -> Self {
        match (query.only, query.vimgrep) {
            (true, _) => Self::Slices(Slices::select(query.multiline, decoration)),
            (false, true) => Self::WholeLine(decoration),
            (false, false) => decoration.map_or(Self::Source, Self::DecoratedRow),
        }
    }
}

impl<'a, 'arg> Decoration<'a, 'arg> {
    fn select(query: &'a Lines<'arg>, style: Option<&'a str>) -> Option<Self> {
        query.replacement.as_ref().map(Self::Replacement)
            .or_else(|| style.filter(|style| !style.is_empty()).map(Self::Styled))
    }

    fn replaces(self) -> bool {
        matches!(self, Self::Replacement(_))
    }
}

impl MatchedLine<'_> {
    fn decoration_spans(&self, byte_input: bool, include_empty: bool) -> impl Iterator<Item = (&Range<usize>, Range<usize>)> {
        self.visible_spans(byte_input).filter(move |(r, _)| include_empty || !r.is_empty())
    }
}

/// Copied source already contains the CR of a CRLF pair. Generated rows may
/// supply their own CR. Choose the policy once per file.
struct RecordEndings {
    source: &'static [u8],
    generated: &'static [u8],
    transformed: bool,
    replace_cr: bool,
}

impl RecordEndings {
    fn new(query: &Lines, mode: Projection<'_, '_>) -> Self {
        let generated = query.generated_record_ending();
        let transformed = !matches!(mode, Projection::Source | Projection::WholeLine(None | Some(Decoration::Styled(_))));
        let replaces_row = matches!(mode, Projection::DecoratedRow(_) | Projection::WholeLine(Some(Decoration::Replacement(_))));
        Self {
            source: if query.null_data { b"\0" } else { b"\n" },
            generated,
            transformed,
            replace_cr: replaces_row && generated == b"\r\n",
        }
    }

    fn strip_cr(&self, record: &MatchedLine) -> bool {
        let replace_final = self.replace_cr && record.terminated;
        replace_final && record.raw.ends_with(b"\r")
    }

    fn copied(&self, terminated: bool) -> &'static [u8] {
        if terminated { self.source } else { self.generated }
    }

    fn projected(&self, terminated: bool) -> &'static [u8] {
        self.copied(terminated && !self.transformed)
    }
}

/// Ordinary rows report a record-local column. A contiguous multiline block
/// reports the column of its first occurrence on every row.
pub(super) enum SourceColumn {
    Hidden,
    Local,
    Block(Option<usize>),
}

impl SourceColumn {
    pub fn new(query: &Lines) -> Self {
        let reports_source_column = query.column && !(query.only || query.vimgrep);
        match (reports_source_column, query.multiline, query.invert) {
            (false, _, _) => Self::Hidden,
            (true, true, false) => Self::Block(None),
            (true, _, _) => Self::Local,
        }
    }

    fn end_block(&mut self) {
        if let Self::Block(column) = self { *column = None; }
    }

    fn get(&mut self, record: &offsets::InputRecord, byte_input: bool, first: usize) -> usize {
        let column = || offsets::cursor(record.raw, &record.decoded, byte_input)(first) + 1;
        match self {
            Self::Hidden => 1,
            Self::Local => column(),
            Self::Block(saved) => *saved.get_or_insert_with(column),
        }
    }
}

/// Output state belongs to one file. A multiline replacement can cover a record
/// without ending the continuation window, even when the match limit is reached.
pub(super) struct LineOutput<'a, 'src, 'arg, P> {
    pub printer: super::output::RecordWriter<'a, 'arg, P>,
    pub context: super::output::Context<'src>,
    pub blocks: Option<super::multiline::BlockOutput<'a, 'arg>>,
    pub styled: super::replace::Buffer,
    pub projection: LineProjection<'a, 'arg>,
    pub tracked: bool,
    pub column: SourceColumn,
}

impl<'src, P: FnMut(&mut Vec<u8>, &[u8])> LineOutput<'_, 'src, '_, P> {
    pub fn write(
        &mut self,
        query: &Lines,
        out: &mut Vec<u8>,
        row: super::line_matches::RecordMatch<'_, 'src>,
    ) {
        use super::output::{LineLength, Record};
        let super::line_matches::RecordMatch { record, hay, matches, hit } = row;
        let lengths = LineLength { matches: matches.count, spans: matches.ranges, tracked: self.tracked };
        let line = record.line;
        let terminated = record.terminated();
        let copied = self.projection.endings.copied(terminated);
        self.context.observe((line, record.byte_start, record.raw), hit.then_some(line), |n, offset, bytes| {
            // Only the current row can be the unterminated final row.
            let ending = if n == line { copied } else { self.projection.endings.source };
            self.printer.record(out, Record { line: n, offset, bytes, matched: false, column: None }, &lengths, ending);
        });
        if !hit { self.column.end_block(); return; }
        if matches.continuation_only {
            self.printer.covered(line);
            return;
        }
        let Some(first) = matches.first else {
            // Inversion selects a complete source record. A CRLF edge match can
            // likewise select a record without a span in its printable body.
            let column = (!query.invert && !query.vimgrep).then_some(1);
            self.printer.record(out, Record { line, offset: record.byte_start, bytes: record.raw, matched: true, column }, &lengths, copied);
            return;
        };
        let column = self.column.get(record, query.byte_input, first);
        if self.write_block(record, column, &lengths, out) { return; }
        let matched = MatchedLine {
            raw: record.raw,
            decoded: &record.decoded,
            hay,
            column,
            terminated,
            ranges: matches.ranges,
            line,
            byte_start: record.byte_start,
        };
        self.write_projected(query, matched, &lengths, out);
    }

    fn write_projected(
        &mut self,
        query: &Lines,
        matched: MatchedLine,
        lengths: &super::output::LineLength,
        out: &mut Vec<u8>,
    ) {
        use super::output::Record;
        let line = matched.line;
        let byte_start = matched.byte_start;
        let strip_cr = self.projection.endings.strip_cr(&matched);
        let ending = self.projection.endings.projected(matched.terminated);
        self.projection.render(matched, query, &mut self.styled,
            |bytes, col| {
                let offset = if self.projection.span_offset { col.unwrap_or(1) - 1 } else { 0 };
                let bytes = if strip_cr { bytes.strip_suffix(b"\r").unwrap_or(bytes) } else { bytes };
                self.printer.record(out, Record { line, offset: byte_start + offset, bytes, matched: true, column: col }, lengths, ending)
            });
        if query.multiline { self.printer.covered(line); }
    }
    fn write_block(
        &mut self, record: &offsets::InputRecord, column: usize,
        lengths: &super::output::LineLength, out: &mut Vec<u8>,
    ) -> bool {
        let Some(blocks) = &mut self.blocks else { return false };
        let mut emitted = false;
        let rendered = blocks.render(record, &mut self.styled, |projection| {
            let line = record.line + projection.row;
            if emitted { self.printer.covered(line - 1); }
            self.printer.record(out, super::output::Record {
                line, offset: record.byte_start + projection.offset,
                bytes: projection.bytes, matched: true, column: projection.column.or(Some(column)),
            }, lengths, self.projection.endings.generated);
            emitted = true;
        });
        if rendered { self.printer.covered(record.line); }
        rendered
    }

}
