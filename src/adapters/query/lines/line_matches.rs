//! Choose the least match evidence that can answer this invocation.
use super::{Lines, Pattern, Range, RegexScratch};

use super::{
    RecordAdmission,
    multiline::FullText,
    offsets::{self, InputRecord},
};

#[derive(Default)]
pub(super) struct Evidence {
    pub render: bool,
    pub count: bool,
    pub earliest: bool,
}

#[derive(Default)]
pub(super) struct Totals {
    pub hits: usize,
    pub occurrences: usize,
    pub byte_end: usize,
    pub stopped: bool,
}

impl Totals {
    pub(super) fn exists(hit: bool) -> Self {
        Self { hits: usize::from(hit), ..Self::default() }
    }
}

pub(super) struct RecordInput<'q, 'arg, 'text> {
    query: &'q Lines<'arg>,
    bytes: &'text [u8],
    terminator: u8,
    pub full: Option<FullText<'text>>,
}

pub(super) struct RecordMatch<'row, 'src> {
    pub record: &'row InputRecord<'src>,
    pub hay: &'row str,
    pub matches: Matches<'row>,
    pub hit: bool,
}

impl<'row, 'src> RecordMatch<'row, 'src> {
    fn context(record: &'row InputRecord<'src>) -> Self {
        Self { record, hay: &record.decoded, matches: Matches::default(), hit: false }
    }

    fn occurrences(&self, query: &Lines) -> usize {
        if self.hit { self.matches.occurrences(query) } else { 0 }
    }

    pub(super) fn source_ranges<'a>(
        &'a self,
        byte_input: bool,
        buffer: &'a mut Vec<Range<usize>>,
    ) -> &'a [Range<usize>] {
        if !self.hit {
            return &[];
        }
        let ranges = self.matches.ranges;
        let r = self.record;
        if ranges.is_empty() || r.raw.len() == r.decoded.len() {
            return ranges;
        }
        let mut original = offsets::cursor(r.raw, &r.decoded, byte_input);
        buffer.clear();
        buffer.extend(ranges.iter().map(|r| original(r.start)..original(r.end)));
        buffer
    }
}

struct SearchRecord<'row, 'src> {
    source: &'row InputRecord<'src>,
    text: super::SearchText<'row>,
}

impl<'q, 'arg, 'text> RecordInput<'q, 'arg, 'text> {
    pub fn new(
        query: &'q Lines<'arg>,
        bytes: &'text [u8],
        terminator: u8,
        scratch: &mut RegexScratch,
    ) -> Self {
        Self {
            query,
            bytes,
            terminator,
            full: FullText::new(query, bytes, scratch),
        }
    }

    pub fn visit<'view>(
        &'view self,
        scratch: &mut RegexScratch,
        mut admission: RecordAdmission,
        evidence: Evidence,
        mut visit: impl FnMut(RecordMatch<'_, 'view>),
    ) -> Totals {
        let q = self.query;
        let full = self.full.as_ref();
        let selection = full.map(|f| &f.selection);
        let mut matcher = Matcher::new(q, selection.map(|s| s.spans.as_slice()), evidence);
        let mut ranges = Vec::new();
        let mut totals = Totals::default();
        let records = offsets::input_records(self.bytes, full.map(|f| &f.view), self.terminator, q)
            .take_while(|record| selection.is_none_or(|selected| record.decoded_range.start < selected.end));
        for record in records {
            totals.byte_end = record.byte_end;
            let allowed = admission.match_allowed(q.multiline, q.invert);
            let matched = matcher.select_admitted(q, &record, (scratch, &mut ranges), allowed);
            let hit = matched.hit;
            totals.occurrences += matched.occurrences(q);
            visit(matched);
            if admission.observe(hit, record.line, 1) {
                totals.stopped = true;
                break;
            }
        }
        totals.hits = admission.hits;
        if q.stats
            && let Some(full) = full
        {
            totals.byte_end = full.searched(self.bytes, q.byte_input, self.terminator);
        }
        totals
    }
}

pub(super) enum Matcher<'a, 'arg> {
    First {
        earliest: bool,
    },
    Count(&'a Pattern<'arg>),
    CountByte(u8),
    Ranges,
    Multiline(MultilineCursor<'a>),
}

#[derive(Default)]
pub(super) struct Matches<'a> {
    pub continuation_only: bool,
    pub first: Option<usize>,
    pub count: usize,
    pub starts: usize,
    pub ranges: &'a [Range<usize>],
}

impl<'a, 'arg> Matcher<'a, 'arg> {
    pub fn new(
        lines: &'a Lines<'arg>,
        spans: Option<&'a [Range<usize>]>,
        evidence: Evidence,
    ) -> Self {
        let Evidence {
            render,
            count,
            earliest,
        } = evidence;
        if let Some(spans) = spans {
            return Self::Multiline(MultilineCursor {
                spans,
                render,
                display_eof: !lines.quiet && lines.result == super::ResultMode::Lines && !lines.json,
                previous_hit: false,
            });
        }
        if !render && !count {
            return Self::First { earliest };
        }
        match (render, lines.patterns.as_slice()) {
            (false, [pattern]) => Self::for_count(pattern),
            _ => Self::Ranges,
        }
    }

    fn for_count(pattern: &'a Pattern<'arg>) -> Self {
        if let Pattern::Literal(text) = pattern
            && let [byte] = text.as_bytes()
        {
            return Self::CountByte(*byte);
        }
        Self::Count(pattern)
    }

    fn select_admitted<'r, 'src>(
        &mut self,
        query: &Lines,
        record: &'r InputRecord<'src>,
        (scratch, ranges): (&mut RegexScratch, &'r mut Vec<Range<usize>>),
        allowed: bool,
    ) -> RecordMatch<'r, 'src> {
        if allowed {
            self.select(query, record, scratch, ranges)
        } else {
            RecordMatch::context(record)
        }
    }

    fn select<'r, 'src>(
        &mut self, query: &Lines, record: &'r InputRecord<'src>,
        scratch: &mut RegexScratch, ranges: &'r mut Vec<Range<usize>>,
    ) -> RecordMatch<'r, 'src> {
        let row = SearchRecord { source: record, text: record.search_view(query) };
        let matches = self.line(query, &row, scratch, ranges);
        let hit = matches.selects(query, record) != query.invert;
        RecordMatch { record, hay: row.text.decoded, matches, hit }
    }

    fn line<'r>(
        &mut self,
        lines: &Lines,
        row: &SearchRecord<'_, '_>,
        scratch: &mut RegexScratch,
        ranges: &'r mut Vec<Range<usize>>,
    ) -> Matches<'r> {
        let matched_text = row.text;
        let mut m = Matches::default();
        match self {
            Self::First { earliest } => m.first = lines.first_position(matched_text, scratch, *earliest),
            Self::CountByte(byte) => {
                let bytes = matched_text.decoded.as_bytes();
                m.first = bytes.iter().position(|b| b == byte);
                if let Some(first) = m.first {
                    m.count = 1 + bytes[first + 1..]
                        .iter()
                        .map(|b| usize::from(b == byte))
                        .sum::<usize>();
                }
            }
            Self::Count(pattern) => lines.visit_ranges(pattern, matched_text, scratch, |span| {
                m.first.get_or_insert(span.start);
                m.count += 1;
            }),
            Self::Ranges => {
                lines.fill_ranges(matched_text, scratch, ranges);
                m = Matches::spans(ranges, 0);
            }
            Self::Multiline(cursor) => {
                return cursor.project(row.source, matched_text.decoded.len(), lines, ranges);
            }
        }
        m
    }
}

pub(super) struct MultilineCursor<'a> {
    spans: &'a [Range<usize>],
    render: bool,
    display_eof: bool,
    previous_hit: bool,
}

impl<'s> MultilineCursor<'s> {
    /// Leave a crossing span at the front of the cursor for the next row.
    fn take_row(&mut self, record: &InputRecord, only: bool) -> &'s [Range<usize>] {
        let decoded = &record.decoded_range;
        // The line printer selects a final unterminated record for an EOF hit.
        // In only-matching output an EOF-only fallback starts a new match block.
        let include_eof = self.display_eof && !record.terminated()
            && !(only && self.previous_hit);
        let limit = decoded.end + usize::from(include_eof);
        let count = crate::domain::prefix_len(self.spans, |s| s.start < limit);
        self.previous_hit = count != 0;
        let visible = &self.spans[..count];
        // Non-overlapping, ordered matches leave at most one spanning the next record.
        let carried = visible.last().is_some_and(|span| span.end > decoded.end);
        self.spans = &self.spans[count - usize::from(carried)..];
        visible
    }

    fn project<'r>(
        &mut self,
        record: &InputRecord<'_>,
        body_len: usize,
        lines: &Lines,
        ranges: &'r mut Vec<Range<usize>>,
    ) -> Matches<'r> {
        let visible = self.take_row(record, lines.only);
        let decoded = &record.decoded_range;
        let continued = visible.first().is_some_and(|span| span.start < decoded.start);
        let starts = visible.len() - usize::from(continued);
        let selected = if lines.vimgrep && continued {
            &visible[1..]
        } else {
            visible
        };
        let clip = |offset: usize| offset.saturating_sub(decoded.start).min(body_len);
        let mut matches = if self.render {
            ranges.clear();
            ranges.extend(selected.iter().map(|span| clip(span.start)..clip(span.end)));
            Matches::spans(ranges, starts)
        } else {
            Matches {
                first: selected.first().map(|span| clip(span.start)),
                count: selected.len(),
                starts,
                ranges: &[],
                ..Matches::default()
            }
        };
        matches.continuation_only = continued && selected.is_empty();
        matches
    }
}

impl<'a> Matches<'a> {
    fn selects(&self, query: &Lines, record: &InputRecord) -> bool {
        self.continuation_only || self.first.is_some() || query.matched_crlf_edge(record.raw, record.terminated())
    }

    pub fn occurrences(&self, lines: &Lines) -> usize {
        if lines.multiline { self.starts } else { self.count }
    }

    fn spans(ranges: &'a [Range<usize>], starts: usize) -> Self {
        Self {
            continuation_only: false,
            first: ranges.first().map(|r| r.start),
            count: ranges.len(),
            starts,
            ranges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_cursor_carries_only_matches_crossing_the_record_boundary() {
        for (render, vimgrep) in [(false, false), (false, true), (true, false), (true, true)] {
            check_multiline_cursor(render, vimgrep);
        }
    }

    fn check_multiline_cursor(render: bool, vimgrep: bool) {
        let spans = [0..0, 1..8, 8..9, 9..9, 10..12];
        let lines = Lines {
            vimgrep,
            ..Default::default()
        };
        let mut matcher = Matcher::new(
            &lines,
            Some(&spans),
            Evidence {
                render,
                count: true,
                earliest: true,
            },
        );
        let mut scratch = RegexScratch::default();
        let mut ranges = Vec::new();
        for start in (0..12).step_by(3) {
            let end = start + 3;
            let record = InputRecord {
                line: start / 3 + 1, raw: b"xx", decoded: "xx".into(),
                byte_start: start, byte_end: end, decoded_range: start..end,
            };
            let row = SearchRecord { source: &record, text: record.search_view(&lines) };
            let matches = matcher.line(&lines, &row, &mut scratch, &mut ranges);
            let active = spans
                .iter()
                .filter(|s| s.start < end && (s.end > start || s.start == start));
            let starts = active.clone().filter(|s| s.start >= start).count();
            let clip = |at: usize| at.saturating_sub(start).min(2);
            let wanted: Vec<_> = active
                .filter(|s| !vimgrep || s.start >= start)
                .map(|s| clip(s.start)..clip(s.end))
                .collect();
            assert_eq!(matches.first, wanted.first().map(|s| s.start));
            assert_eq!((matches.count, matches.starts), (wanted.len(), starts));
            let expected = rendered_ranges(render, &wanted);
            assert_eq!(matches.ranges, expected);
        }
    }

    fn rendered_ranges(render: bool, wanted: &[Range<usize>]) -> &[Range<usize>] {
        if render { wanted } else { &[] }
    }
}
