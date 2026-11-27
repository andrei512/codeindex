//! Byte formatting for an emitted source or context record.
use std::{io::Write, ops::Range};

pub(super) fn number(out: &mut Vec<u8>, value: usize, style: Option<&str>, separator: &[u8]) {
    if let Some(style) = style {
        write!(out, "\x1b[0m{style}{value}\x1b[0m").unwrap();
    } else {
        write!(out, "{value}").unwrap();
    }
    out.extend_from_slice(separator);
}

impl super::Lines<'_> {
    /// Summary records have no source terminator to preserve.
    pub(super) fn generated_record_ending(&self) -> &'static [u8] {
        if self.null_data { b"\0" } else if self.crlf { b"\r\n" } else { b"\n" }
    }
}

/// Source records waiting for a surrounding hit, and the last trailing line
/// promised by an earlier hit. The buffer borrows the current source.
pub(super) struct Context<'a> {
    previous: std::collections::VecDeque<(usize, usize, &'a [u8])>,
    before: usize,
    after: usize,
    passthru: bool,
    after_end: usize,
}

impl<'a> Context<'a> {
    pub fn new(before: usize, after: usize, passthru: bool) -> Self {
        Self { before, after, passthru, after_end: 0, previous: Default::default() }
    }

    pub fn observe(
        &mut self,
        record: (usize, usize, &'a [u8]),
        matched_through: Option<usize>,
        mut emit: impl FnMut(usize, usize, &[u8]),
    ) {
        let (line, offset, bytes) = record;
        if let Some(end) = matched_through {
            for (line, offset, bytes) in self.previous.drain(..) {
                emit(line, offset, bytes);
            }
            self.after_end = end.saturating_add(self.after);
            return;
        }
        if self.passthru || line <= self.after_end {
            emit(line, offset, bytes);
            return;
        }
        if self.before == 0 {
            return;
        }
        if self.previous.len() == self.before {
            self.previous.pop_front();
        }
        self.previous.push_back(record);
    }
}

pub(super) struct LineLength<'a> {
    pub matches: usize,
    pub spans: &'a [Range<usize>],
    pub tracked: bool,
}

enum ColumnLimit {
    Unlimited,
    Omit(usize),
    Preview(usize),
}

impl ColumnLimit {
    fn new(options: &super::Lines) -> Self {
        match (options.max_columns.filter(|&limit| limit > 0), options.preview) {
            (Some(limit), true) => Self::Preview(limit),
            (Some(limit), false) => Self::Omit(limit),
            (None, _) => Self::Unlimited,
        }
    }

    fn write(&self, row: RecordBody<'_>, out: &mut Vec<u8>) {
        match *self {
            Self::Preview(limit) if row.content.len() > limit => row.preview(limit, out),
            Self::Omit(limit) if row.content.len() > limit => row.omit(out),
            _ => out.extend_from_slice(row.content),
        }
    }
}

struct RecordBody<'a> {
    content: &'a [u8],
    lengths: &'a LineLength<'a>,
    terminator: u8,
    matched: bool,
    only: bool,
}

impl RecordBody<'_> {
    fn preview(&self, limit: usize, out: &mut Vec<u8>) {
        let preview = &self.content[..limit];
        out.extend_from_slice(preview.strip_suffix(&[self.terminator]).unwrap_or(preview));
        if !self.lengths.tracked || !self.matched {
            out.extend_from_slice(b" [... omitted end of long line]");
            return;
        }
        let spans = if self.only { &[] } else { self.lengths.spans };
        let remaining = spans.len() - spans.partition_point(|span| span.start < limit);
        write!(out, " [... {remaining} more match{}]", plural(remaining)).unwrap();
    }

    fn omit(&self, out: &mut Vec<u8>) {
        if !self.matched {
            out.extend_from_slice(b"[Omitted long context line]");
        } else if self.lengths.tracked && !self.only {
            write!(out, "[Omitted long line with {} matches]", self.lengths.matches).unwrap();
        } else {
            out.extend_from_slice(b"[Omitted long matching line]");
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "es" }
}

pub(super) struct Record<'a> {
    pub matched: bool,
    pub line: usize,
    pub offset: usize,
    pub bytes: &'a [u8],
    pub column: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathLabel {
    Hidden,
    Heading { show_path: bool },
    Prefix,
}

impl PathLabel {
    fn for_operand(options: &super::Lines, default_path: bool) -> Self {
        let show_path = options.filename.unwrap_or(default_path);
        if options.heading.unwrap_or(options.terminal) {
            Self::Heading { show_path }
        } else if show_path {
            Self::Prefix
        } else {
            Self::Hidden
        }
    }
}

impl super::Lines<'_> {
    /// A directory worker can render independently when a file's opening
    /// bytes do not depend on output from the preceding file.
    pub(super) fn separates_file_output(&self) -> bool {
        if self.search_output() != super::SearchOutput::Lines { return false; }
        matches!(PathLabel::for_operand(self, true), PathLabel::Heading { .. }) || self.context_group_separator().is_some()
    }

    fn context_group_separator(&self) -> Option<&[u8]> {
        if self.before == 0 && self.after == 0 { return None; }
        self.context_separator.as_deref()
    }
}

/// State shared by all records emitted for the same file.
pub(super) struct RecordWriter<'a, 'arg, P> {
    options: &'a super::Lines<'arg>,
    path: P,
    label: PathLabel,
    numbered: bool,
    prior_output: bool,
    emitted: usize,
    line_style: Option<String>,
    column_style: Option<String>,
    columns: ColumnLimit,
}

pub(super) struct RecordStyle {
    pub default_path: bool,
    pub color: bool,
    pub prior_output: bool,
}

impl<'a, 'arg, P: FnMut(&mut Vec<u8>, &[u8])> RecordWriter<'a, 'arg, P> {
    pub fn new(
        options: &'a super::Lines<'arg>,
        path: P,
        style: RecordStyle,
    ) -> Self {
        let RecordStyle { default_path, color, prior_output } = style;
        let numbered = options.numbered.unwrap_or(options.terminal);
        Self {
            options,
            path,
            label: PathLabel::for_operand(options, default_path),
            numbered,
            emitted: 0,
            prior_output,
            columns: ColumnLimit::new(options),
            line_style: (color && numbered).then(|| options.colors.line.ansi()),
            column_style: (color && options.column).then(|| options.colors.column.ansi()),
        }
    }

    // A replacement can collapse several source records into one output row.
    pub fn covered(&mut self, line: usize) {
        if self.emitted != 0 { self.emitted = self.emitted.max(line); }
    }

    fn begin_group(&mut self, out: &mut Vec<u8>, line: usize) {
        let options = self.options;
        let gap = match (self.emitted, self.label) {
            (0, PathLabel::Heading { show_path }) => {
                self.write_heading(out, show_path);
                false
            }
            (0, _) => self.prior_output,
            (previous, _) => line > previous + 1,
        };
        if gap && let Some(sep) = options.context_group_separator() {
            out.extend_from_slice(sep);
            out.extend_from_slice(options.generated_record_ending());
        }
        self.emitted = line;
    }

    fn write_heading(&mut self, out: &mut Vec<u8>, show_path: bool) {
        let ending = self.options.generated_record_ending();
        if self.prior_output { out.extend_from_slice(ending); }
        if show_path { (self.path)(out, ending); }
    }

    fn write_prefix(&mut self, out: &mut Vec<u8>, record: &Record) {
        let options = self.options;
        let sep = if record.matched {
            &options.match_separator
        } else {
            &options.context_field
        };
        if self.label == PathLabel::Prefix {
            (self.path)(out, sep);
        }
        if self.numbered {
            number(out, record.line, self.line_style.as_deref(), sep);
        }
        if options.column
            && let Some(column) = record.column
        {
            number(out, column, self.column_style.as_deref(), sep);
        }
        if options.byte_offset {
            write!(out, "{}", record.offset).unwrap();
            out.extend_from_slice(sep);
        }
    }

    pub fn record(
        &mut self,
        out: &mut Vec<u8>,
        record: Record,
        lengths: &LineLength,
        ending: &[u8],
    ) {
        self.begin_group(out, record.line);
        let options = self.options;
        self.write_prefix(out, &record);
        let content = if options.trim {
            record.bytes.trim_ascii_start()
        } else {
            record.bytes
        };
        let content_start = out.len();
        let term = ending[ending.len() - 1];
        let body = RecordBody {
            content,
            lengths,
            matched: record.matched,
            only: options.only,
            terminator: term,
        };
        self.columns.write(body, out);
        if !out[content_start..].ends_with(&[term]) { out.extend_from_slice(ending); }
    }
}
