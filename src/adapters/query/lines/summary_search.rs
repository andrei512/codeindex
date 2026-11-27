//! Quiet, path and count outputs reduce matches to a per-file answer.
use super::{
    Lines, Pattern, RecordAdmission, RegexScratch, ResultMode,
    line_matches::{Evidence, RecordInput, Totals},
};
use std::{io::Write, time::Instant};

#[derive(Clone, Copy)]
enum CountUnit {
    SelectedRecords,
    Occurrences,
}

impl CountUnit {
    fn for_query(q: &Lines<'_>) -> Self {
        match (q.result, q.only, q.invert, q.multiline) {
            (ResultMode::CountLines, true, _, _) |
            (_, _, _, true) |
            (ResultMode::CountMatches, _, false, _) => Self::Occurrences,
            _ => Self::SelectedRecords,
        }
    }

    fn total(self, totals: &Totals) -> usize {
        match self {
            Self::SelectedRecords => totals.hits,
            Self::Occurrences => totals.occurrences,
        }
    }
}

impl super::operands::FileSearch<'_, '_> {
    pub(super) fn scan_summary(&mut self, text: &[u8]) -> bool {
        let q = self.query;
        let path = self.operand.name;
        let show_path = self.operand.show_path;
        let out = &mut *self.out;
        let stats = &mut *self.stats;
        if q.max == Some(0) {
            return false;
        }
        let clock = q.stats.then(Instant::now);
        let unit = CountUnit::for_query(q);
        let totals = q.summary_totals(text, matches!(unit, CountUnit::Occurrences));
        if let Some(clock) = clock {
            stats.scanned(&totals, 0, clock);
        }
        // Full-text summaries select files by regex occurrences within selected
        // records. Inversion can select rows while leaving that count at zero.
        let has_match = if q.multiline { totals.occurrences } else { totals.hits } != 0;
        if q.quiet {
            return has_match;
        }
        let selected = has_match != (q.result == ResultMode::NonmatchingPaths);
        q.write_summary(out, (path, show_path), (selected, unit.total(&totals)));
        selected
    }

}

enum SummaryInput<'a> {
    Records(&'a [u8]),
    Byte(&'a [u8], u8),
    Literal(&'a str, &'a str),
}

impl<'a> SummaryInput<'a> {
    fn select(text: &'a [u8], literal: &'a str, first_hit_decides: bool) -> Self {
        // An ASCII octet never appears inside another UTF-8 scalar. Invalid UTF-8
        // maps to the replacement character and cannot introduce an ASCII match.
        if let (true, [byte]) = (first_hit_decides, literal.as_bytes()) {
            return Self::Byte(text, *byte);
        }
        std::str::from_utf8(text).map_or(Self::Records(text), |input| Self::Literal(input, literal))
    }

    fn scan(self, query: &Lines<'_>, options: SummaryOptions) -> Totals {
        let SummaryOptions { terminator, first_hit_decides, limit, count_occurrences } = options;
        match self {
            Self::Records(text) => query.record_totals(text, terminator, limit, count_occurrences),
            Self::Byte(text, byte) => Totals::exists(text.contains(&byte)),
            Self::Literal(input, literal) if first_hit_decides => Totals::exists(input.contains(literal)),
            Self::Literal(input, literal) => literal_totals(input, literal, SummaryOptions {
                count_occurrences: count_occurrences || query.multiline,
                ..options
            }),
        }
    }
}

#[derive(Clone, Copy)]
struct SummaryOptions {
    terminator: u8,
    first_hit_decides: bool,
    limit: Option<usize>,
    count_occurrences: bool,
}

impl Lines<'_> {
    fn write_summary(&self, out: &mut Vec<u8>, (path, show_path): (&str, bool), (selected, count): (bool, usize)) {
        let mut write_path = self.path_writer(path, self.color.unwrap_or(self.terminal_color));
        let line_end = self.generated_record_ending();
        if !self.result.counts() {
            if selected { write_path(out, line_end); }
            return;
        }
        if !(selected || self.include_zero) { return; }
        if self.filename.unwrap_or(show_path) { write_path(out, b":"); }
        write!(out, "{count}").unwrap();
        out.extend_from_slice(line_end);
    }

    pub(super) fn summary_totals(&self, text: &[u8], count_occurrences: bool) -> Totals {
        let terminator = if self.null_data { 0 } else { b'\n' };
        let first_hit_decides = !self.stats && (self.quiet || !self.result.counts());
        let limit = if first_hit_decides { Some(1) } else { self.record_limit() };
        let input = self.summary_literal(terminator)
            .map_or(SummaryInput::Records(text), |literal| SummaryInput::select(text, literal, first_hit_decides));
        let count_occurrences = !self.invert && (self.stats || count_occurrences);
        input.scan(self, SummaryOptions { terminator, first_hit_decides, limit, count_occurrences })
    }

    // Only an unrestricted, record-local literal can skip directly from hit to hit.
    fn summary_literal(&self, terminator: u8) -> Option<&str> {
        let [Pattern::Literal(literal)] = self.patterns.as_slice() else {
            return None;
        };
        let eligible = !(self.invert
            || self.stop_on_nonmatch
            || self.byte_input
            || literal.is_empty()
            || literal.as_bytes().contains(&terminator)
            || self.crlf && literal.contains('\r'));
        eligible.then_some(literal)
    }

    fn record_totals(
        &self,
        text: &[u8],
        terminator: u8,
        limit: Option<usize>,
        count_occurrences: bool,
    ) -> Totals {
        let mut scratch = RegexScratch::default();
        let input = RecordInput::new(self, text, terminator, &mut scratch);
        let evidence = Evidence {
            count: count_occurrences,
            ..Default::default()
        };
        let admission = RecordAdmission::new(limit, 0, self.stop_on_nonmatch);
        input.visit(&mut scratch, admission, evidence, |_| {})
    }
}

fn literal_totals(input: &str, pattern: &str, options: SummaryOptions) -> Totals {
    let terminator = char::from(options.terminator);
    let mut totals = Totals::default();
    while let Some(first) = input[totals.byte_end..].find(pattern) {
        totals.hits += 1;
        let tail = totals.byte_end + first + pattern.len();
        totals.byte_end = input[tail..]
            .find(terminator)
            .map_or(input.len(), |i| tail + i + 1);
        totals.occurrences += literal_occurrences(input, tail..totals.byte_end, pattern, options.count_occurrences);
        if Some(totals.hits) == options.limit {
            return totals;
        }
    }
    totals.byte_end = input.len();
    totals
}

fn literal_occurrences(input: &str, remaining: std::ops::Range<usize>, pattern: &str, count: bool) -> usize {
    if count { 1 + input[remaining].matches(pattern).count() } else { 0 }
}
