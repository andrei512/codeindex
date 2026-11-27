//! Ripgrep's newline-delimited events for UTF-8 text.
use super::{
    super::json::{array, object, quote},
    Lines, RecordAdmission, RegexScratch,
    line_matches::{Evidence, RecordInput, Totals},
    multiline::FullText,
    operands::FileSearch,
};
use std::{
    io::Write,
    ops::Range,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Stats {
    searches: usize,
    matched_files: usize,
    bytes: usize,
    printed: usize,
    lines: usize,
    matches: usize,
    elapsed: Duration,
}
fn duration(d: Duration) -> String {
    object(&[
        ("secs", d.as_secs().to_string()),
        ("nanos", d.subsec_nanos().to_string()),
        ("human", quote(&format!("{:.6}s", d.as_secs_f64()))),
    ])
}
fn text(s: &str) -> String {
    object(&[("text", quote(s))])
}

impl Stats {
    pub(super) fn add(&mut self, other: &Stats) {
        self.searches += other.searches;
        self.matched_files += other.matched_files;
        self.bytes += other.bytes;
        self.printed += other.printed;
        self.lines += other.lines;
        self.matches += other.matches;
        self.elapsed += other.elapsed;
    }
    pub(super) fn scanned(&mut self, totals: &Totals, printed: usize, start: Instant) {
        self.searches += 1;
        self.matched_files += usize::from(totals.hits > 0);
        self.bytes += totals.byte_end;
        self.printed += printed;
        self.lines += totals.hits;
        self.matches += totals.occurrences;
        self.elapsed += start.elapsed();
    }
    pub(super) fn plain_summary(&self, start: Instant, out: &mut Vec<u8>) {
        writeln!(out,"\n{} matches\n{} matched lines\n{} files contained matches\n{} files searched\n{} bytes printed\n{} bytes searched\n{:.6} seconds spent searching\n{:.6} seconds", self.matches,self.lines,self.matched_files,self.searches,self.printed,self.bytes,self.elapsed.as_secs_f64(),start.elapsed().as_secs_f64()).unwrap();
    }
    fn json(&self) -> String {
        object(&[
            ("elapsed", duration(self.elapsed)),
            ("searches", self.searches.to_string()),
            ("searches_with_match", self.matched_files.to_string()),
            ("bytes_searched", self.bytes.to_string()),
            ("bytes_printed", self.printed.to_string()),
            ("matched_lines", self.lines.to_string()),
            ("matches", self.matches.to_string()),
        ])
    }
    pub(super) fn summary(&self, start: Instant, out: &mut Vec<u8>) {
        event(
            out,
            "summary",
            object(&[
                ("elapsed_total", duration(start.elapsed())),
                ("stats", self.json()),
            ]),
        );
    }
}
fn event(out: &mut Vec<u8>, kind: &str, data: String) {
    writeln!(out, "{}", object(&[("type", quote(kind)), ("data", data)])).unwrap();
}
fn binary(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return text(s);
    }
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            encoded.push(if i > chunk.len() {
                '='
            } else {
                char::from(DIGITS[((n >> (18 - 6 * i)) & 63) as usize])
            });
        }
    }
    object(&[("bytes", quote(&encoded))])
}
struct Record<'a> {
    line: usize,
    offset: usize,
    body: &'a [u8],
}

impl Record<'_> {
    fn emit(&self, out: &mut Vec<u8>, path: &str, kind: &str, ranges: &[Range<usize>]) {
        let subs = ranges.iter().map(|r| {
            object(&[
                ("match", binary(&self.body[r.clone()])),
                ("start", r.start.to_string()),
                ("end", r.end.to_string()),
            ])
        });
        event(
            out,
            kind,
            object(&[
                ("path", path.into()),
                ("lines", binary(self.body)),
                ("line_number", self.line.to_string()),
                ("absolute_offset", self.offset.to_string()),
                ("submatches", array(subs)),
            ]),
        );
    }
}

/// One file's event stream owns its counters and pending context records.
struct FileEvents<'a, 'out> {
    out: &'out mut Vec<u8>,
    path: String,
    stats: Stats,
    context: super::output::Context<'a>,
    initial: usize,
    started: Instant,
}

impl<'a, 'out> FileEvents<'a, 'out> {
    fn new(q: &Lines<'_>, path: &str, out: &'out mut Vec<u8>) -> Self {
        Self {
            started: Instant::now(),
            initial: out.len(),
            out,
            path: text(path),
            context: super::output::Context::new(q.before, q.after, q.passthru),
            stats: Stats { searches: 1, ..Stats::default() },
        }
    }

    fn record(&mut self, record: Record<'a>, count: usize, ranges: &[Range<usize>], hit: bool) {
        self.stats.bytes += record.body.len();
        let mut emit = |row: &Record, kind: &str, spans: &[Range<usize>]| {
            if self.out.len() == self.initial {
                event(self.out, "begin", object(&[("path", self.path.clone())]));
            }
            row.emit(self.out, &self.path, kind, spans);
        };
        self.context.observe((record.line, record.offset, record.body), hit.then_some(record.line + (count - 1)), |line, offset, body| {
            emit(&Record { line, offset, body }, "context", &[])
        });
        if hit {
            self.stats.lines += count;
            self.stats.matches += ranges.len();
            emit(&record, "match", ranges);
        }
    }

    fn finish(mut self, searched: Option<usize>, binary: Option<usize>, total: &mut Stats) -> bool {
        if let Some(bytes) = searched {
            self.stats.bytes = bytes;
        }
        self.stats.elapsed = self.started.elapsed();
        self.stats.printed = self.out.len() - self.initial;
        let hit = self.stats.lines > 0;
        if self.stats.printed == 0 { return hit; }
        self.stats.matched_files = usize::from(hit);
        event(self.out, "end", object(&[
            ("path", self.path),
            ("binary_offset", binary.map_or_else(|| "null".into(), |offset| offset.to_string())),
            ("stats", self.stats.json()),
        ]));
        total.add(&self.stats);
        hit
    }
}

/// Offset of the first NUL and the complete records available before its refill.
pub(super) struct BinaryBoundary {
    pub offset: usize,
    pub prefix: usize,
}

impl BinaryBoundary {
    fn reported(self, stopped: bool, cursor: usize) -> Option<usize> {
        (!stopped || cursor > self.prefix).then_some(self.offset)
    }
}

impl FileSearch<'_, '_> {
    pub(super) fn json_events(&mut self, source: &[u8], binary: Option<BinaryBoundary>) -> bool {
        let q = self.query;
        let mut events = FileEvents::new(q, self.operand.name, self.out);
        let term = if q.null_data { 0 } else { b'\n' };
        let mut scratch = RegexScratch::default();
        let input = RecordInput::new(q, source, term, &mut scratch);
        if let Some(full) = input.full.as_ref().filter(|_| !q.invert) {
            let (cursor, stopped) = events.grouped(q, source, term, full);
            let binary_offset = binary.and_then(|b| b.reported(stopped, cursor));
            return events.finish(Some(full.searched(source, q.byte_input, term)), binary_offset, self.stats);
        }
        let admission = RecordAdmission::new(q.record_limit(), q.after, q.stop_on_nonmatch);
        let evidence = Evidence { render: !q.multiline, ..Evidence::default() };
        let mut ranges = Vec::new();
        let mut cursor = 0;
        let totals = input.visit(&mut scratch, admission, evidence, |row| {
            let r = row.record;
            cursor = r.byte_end;
            events.record(Record {
                line: r.line, offset: r.byte_start, body: &source[r.byte_start..r.byte_end],
            }, 1, row.source_ranges(q.byte_input, &mut ranges), row.hit);
        });
        let binary_offset = binary.and_then(|b| b.reported(totals.stopped, cursor));
        // An inverted multiline query searched the whole input before visiting rows.
        events.finish(q.multiline.then_some(source.len()), binary_offset, self.stats)
    }
}

impl<'a> FileEvents<'a, '_> {
    /// Positive multiline events present each covered block as one match.
    fn grouped(&mut self, q: &Lines<'_>, source: &'a [u8], term: u8, full: &FullText<'_>) -> (usize, bool) {
        let spans = &full.selection.spans;
        let mut groups = full.selection.blocks(&full.view, char::from(term)).peekable();
        let mut original = super::offsets::cursor(source, &full.view, q.byte_input);
        let scan_end = full.original_offset(source, q.byte_input, full.selection.end);
        let mut admission = RecordAdmission::new(q.record_limit(), q.after, q.stop_on_nonmatch);
        let mut ranges = Vec::new();
        let mut cursor = 0;
        let mut line = 0;
        while cursor < scan_end {
            let offset = cursor;
            ranges.clear();
            let block = groups.next_if(|block| original(block.start) == offset);
            let range = block.map(|block| project_block(block, spans, &mut original, (offset, &mut ranges)));
            let (end, count) = grouped_record(source, term, offset, range);
            let hit = !ranges.is_empty();
            self.record(Record { line: line + 1, offset, body: &source[offset..end] }, count, &ranges, hit);
            line += count;
            cursor = end;
            if admission.observe(hit, line, count) {
                return (cursor, true);
            }
        }
        (cursor, false)
    }
}

fn project_block(
    block: super::multiline::Block,
    spans: &[Range<usize>],
    original: &mut impl FnMut(usize) -> usize,
    (offset, ranges): (usize, &mut Vec<Range<usize>>),
) -> Range<usize> {
    ranges.extend(spans[block.hits].iter().map(|r| original(r.start) - offset..original(r.end) - offset));
    offset..original(block.end)
}

fn grouped_record(source: &[u8], term: u8, offset: usize, block: Option<Range<usize>>) -> (usize, usize) {
    if let Some(block) = block {
        let end = block.end;
        let matched = &source[block];
        let count = matched.iter().filter(|&&b| b == term).count()
            + usize::from(!matched.ends_with(&[term]));
        return (end, count);
    }
    let end = source[offset..].iter().position(|&b| b == term)
        .map_or(source.len(), |i| offset + i + 1);
    (end, 1)
}
