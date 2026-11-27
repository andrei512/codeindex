mod arguments;
pub(crate) use arguments::{CallerOutput, ValueOption, Options, spellings as switch_spellings};
mod color;
mod encoding;
mod globs;
mod events;
mod multiline;
mod operands;
pub(crate) mod pcre;
mod patterns;
mod output;
mod offsets;
mod plain;
mod replace;
mod render;
mod line_matches;
mod summary_search;
mod scope;
mod transform;
mod types;
use crate::domain::pattern::{EmptyPosition, EpsilonClosure, MatchScope, Scratch as RegexScratch};
use super::{CONTENT, Cursor, Pin, Regex, query_trigrams};
use std::{
    collections::BTreeSet,
    io,
};
use std::{ops::{ControlFlow, Range}, time::Instant};

// A single posting intersection arrives sorted and unique. Only alternatives
// need a set union; the common one-pattern search borrows its original rows.
enum CandidateFiles {
    Sorted(Vec<usize>),
    Union(BTreeSet<usize>),
}

impl CandidateFiles {
    fn contains(&self, file: usize) -> bool {
        match self {
            Self::Sorted(ids) => ids.binary_search(&file).is_ok(),
            Self::Union(ids) => ids.contains(&file),
        }
    }
}

#[derive(Clone, Copy)]
struct SearchText<'a> {
    decoded: &'a str,
    invalid_source: Option<&'a [u8]>,
}

impl<'a> SearchText<'a> {
    fn slice_cursor(self) -> impl FnMut(Range<usize>) -> SearchText<'a> {
        let mut offset = offsets::cursor(self.invalid_source.unwrap_or_default(), self.decoded, false);
        move |range| SearchText {
            invalid_source: self.invalid_source.map(|raw| &raw[offset(range.start)..offset(range.end)]),
            decoded: &self.decoded[range],
        }
    }
}

enum Pattern<'a> {
    Literal(&'a str),
    Regex(Regex),
    Pcre(pcre::Pcre),
}

fn minimum_start(first: usize, positions: impl Iterator<Item = usize>) -> usize {
    let mut best = first;
    for pos in positions {
        if pos == 0 {
            return 0;
        }
        best = best.min(pos);
    }
    best
}

/// A zero-width match owns its start even when another expression also matches there.
fn non_overlapping() -> impl FnMut(&Range<usize>) -> bool {
    let mut next = 0;
    move |span| {
        if span.start < next { return false; }
        next = span.end.max(span.start + 1);
        true
    }
}

impl Pattern<'_> {
    fn needs_full_text(&self, terminator: char) -> bool {
        match self {
            Self::Literal(text) => text.contains(terminator),
            Self::Regex(regex) => regex.needs_full_text(terminator),
            Self::Pcre(_) => true,
        }
    }
    fn required_literal(&self) -> &str {
        match self {
            Self::Literal(text) => text,
            Self::Regex(re) => re.required_literal(),
            Self::Pcre(_) => "",
        }
    }

    fn first(&self, hay: SearchText<'_>, scratch: &mut RegexScratch) -> Option<usize> {
        match self.visit(hay, scratch, |range| ControlFlow::Break(range.start)) {
            ControlFlow::Break(position) => Some(position),
            ControlFlow::Continue(()) => None,
        }
    }

    /// Visit matches in source order. A consumer may stop at the first usable span.
    fn visit<B>(
        &self,
        hay: SearchText<'_>,
        scratch: &mut RegexScratch,
        mut visit: impl FnMut(Range<usize>) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        if let Self::Pcre(re) = self {
            return re.matches(hay.decoded).try_for_each(visit);
        }
        let mut chunk = |text: &str, base: usize, right_invalid: bool| {
            let emit = |range: Range<usize>| visit(base + range.start..base + range.end);
            match self {
                Self::Literal(lit) => text.match_indices(lit)
                    .map(|(at, matched)| at..at + matched.len()).try_for_each(emit),
                Self::Regex(re) => re.matches_in_chunk(text, scratch, base != 0, right_invalid).try_for_each(emit),
                Self::Pcre(_) => unreachable!(),
            }
        };
        let Some(raw) = hay.invalid_source else { return chunk(hay.decoded, 0, false) };
        let mut base = 0;
        let mut ends_invalid = false;
        for part in raw.utf8_chunks() {
            let invalid = !part.invalid().is_empty();
            ends_invalid = invalid;
            chunk(part.valid(), base, invalid)?;
            base += part.valid().len() + usize::from(invalid) * char::REPLACEMENT_CHARACTER.len_utf8();
        }
        if ends_invalid {
            chunk("", base, false)?;
        }
        ControlFlow::Continue(())
    }
}

/// The last result-selection flag wins, including inside a short-option group.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ResultMode {
    #[default]
    Lines,
    CountLines,
    CountMatches,
    MatchingPaths,
    NonmatchingPaths,
}

impl ResultMode {
    fn counts(self) -> bool {
        matches!(self, Self::CountLines | Self::CountMatches)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchOutput { Lines, Summary, Json }

#[derive(Default)]
pub struct Lines<'a> {
    patterns: Vec<Pattern<'a>>,
    paths: Vec<&'a str>,
    before: usize,
    after: usize,
    numbered: Option<bool>,
    filename: Option<bool>,
    path_separator: Option<u8>,
    scope: scope::Scope,
    walk: crate::adapters::repository::search_paths::SearchRules<'a>,
    no_messages: bool,
    encoding: encoding::Encoding,
    binary: bool,
    mapped: bool,
    transform: transform::Transform<'a>,
    vimgrep: bool,
    byte_offset: bool,
    null_path: bool,
    pattern_stdin: bool,
    result: ResultMode,
    include_zero: bool,
    trim: bool,
    replacement: Option<replace::Template<'a>>,
    max_columns: Option<usize>,
    preview: bool,
    passthru: bool,
    stop_on_nonmatch: bool,
    match_separator: Vec<u8>,
    context_field: Vec<u8>,
    context_separator: Option<Vec<u8>>,

    column: bool,
    terminal: bool,
    heading: Option<bool>,
    invert: bool,
    match_scope: MatchScope,
    multiline: bool,
    dotall: bool,
    null_data: bool,
    only: bool,
    json: bool,
    hidden: bool,
    text: bool,
    types: types::Types,
    color: Option<bool>,
    colors: color::Colors,
    terminal_color: bool,
    files: bool,
    quiet: bool,
    stats: bool,
    threads: usize,
    ascii: bool,
    byte_input: bool,
    empty_inside_scalar: bool,
    crlf_edge: [bool; 2],
    max: Option<usize>,
    crlf: bool,
    globs: globs::Globs,
}

/// Select records before formatting them. The context window is anchored at
/// the record that reaches the limit. Later matches in that window still print.
struct RecordAdmission {
    maximum: Option<usize>,
    after: usize,
    stop_on_gap: bool,
    end: Option<usize>,
    hits: usize,
}

impl RecordAdmission {
    fn new(maximum: Option<usize>, after: usize, stop_on_gap: bool) -> Self {
        Self {
            maximum,
            after,
            stop_on_gap,
            end: None,
            hits: 0,
        }
    }

    fn match_allowed(&self, multiline: bool, invert: bool) -> bool {
        // Inverted search classifies each physical row through the after-context limit.
        invert || !multiline || self.end.is_none()
    }

    fn observe(&mut self, hit: bool, last_line: usize, count: usize) -> bool {
        if hit {
            self.hits += count;
            if self.maximum == Some(self.hits) {
                self.end = Some(last_line.saturating_add(self.after));
            }
        } else if self.stop_on_gap && self.hits > 0 {
            return true;
        }
        self.end.is_some_and(|end| last_line >= end)
    }
}

impl<'a> Lines<'a> {
    fn view<'b>(&self, bytes: &'b [u8]) -> std::borrow::Cow<'b, str> {
        if self.byte_input && !bytes.is_ascii() {
            std::borrow::Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect())
        } else {
            String::from_utf8_lossy(bytes)
        }
    }
    fn empty_match(&self, position: EmptyPosition, closure: &mut EpsilonClosure) -> bool {
        self.patterns.iter().any(|p| match p {
            Pattern::Literal(s) => s.is_empty(),
            Pattern::Regex(re) => closure.accepts_empty(re, position),
            Pattern::Pcre(_) => false,
        })
    }

    fn matched_crlf_edge(&self, body: &[u8], terminated: bool) -> bool {
        self.crlf_edge[usize::from(terminated)] && body.ends_with(b"\r")
    }

    fn search_text<'b>(&self, raw: &'b [u8], decoded: &'b str) -> SearchText<'b> {
        // Lossy decoding can replace invalid input with a searchable Unicode scalar.
        // Only transformed nonempty input needs its raw boundaries during matching.
        let needs_source = !self.byte_input && !raw.is_empty() && raw.as_ptr() != decoded.as_ptr();
        SearchText { decoded, invalid_source: needs_source.then_some(raw) }
    }
    fn first_position(&self, hay: SearchText<'_>, scratch: &mut RegexScratch, earliest: bool) -> Option<usize> {
        let mut positions = self.patterns.iter().filter_map(|pattern| pattern.first(hay, scratch));
        let first = positions.next()?;
        if !earliest || first == 0 {
            return Some(first);
        }
        Some(minimum_start(first, positions))
    }
    fn ranges(&self, hay: SearchText<'_>, scratch: &mut RegexScratch) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        self.fill_ranges(hay, scratch, &mut ranges);
        ranges
    }

    fn fill_ranges(&self, hay: SearchText<'_>, scratch: &mut RegexScratch, ranges: &mut Vec<Range<usize>>) {
        ranges.clear();
        if let [pattern] = self.patterns.as_slice() {
            self.visit_ranges(pattern, hay, scratch, |range| ranges.push(range));
            return;
        }
        for pattern in &self.patterns {
            let _ = pattern.visit(hay, scratch, |r| {
                ranges.push(r);
                ControlFlow::<()>::Continue(())
            });
        }
        // Each matcher produces ranges in source order. Only multiple
        // independently ordered streams need merging.
        ranges.sort_by_key(|r| r.start);
        ranges.retain(non_overlapping());
    }
    fn visit_ranges(
        &self, pattern: &Pattern, hay: SearchText<'_>, scratch: &mut RegexScratch,
        mut visit: impl FnMut(Range<usize>),
    ) {
        let mut admit = non_overlapping();
        let _ = pattern.visit(hay, scratch, |range| {
            if admit(&range) { visit(range); }
            ControlFlow::<()>::Continue(())
        });
    }

    fn search_output(&self) -> SearchOutput {
        if self.quiet { return SearchOutput::Summary; }
        if self.json { return SearchOutput::Json; }
        if self.result != ResultMode::Lines { return SearchOutput::Summary; }
        SearchOutput::Lines
    }

    fn stops_at_first_hit(&self) -> bool { self.quiet && !self.stats }

    pub fn requires_caller(&self) -> bool {
        self.transform.active()
    }
    pub fn type_listing(&self) -> Option<&str> {
        self.types.listing.as_deref()
    }
    pub(crate) fn empty_search(&self) -> bool {
        !self.files && self.max == Some(0)
    }
    pub fn stdin_only(&self) -> bool {
        self.paths.as_slice() == ["-"]
            || self.paths.is_empty() && !self.files && !self.pattern_stdin
    }
    pub(crate) fn reads_stdin(&self) -> bool {
        use std::os::unix::fs::FileTypeExt;
        self.paths.as_slice() == ["-"] || self.stdin_only() && std::fs::metadata("/dev/stdin")
            .is_ok_and(|m| m.is_file() || m.file_type().is_fifo() || m.file_type().is_socket())
    }
    pub fn stdin_warnings(&self) -> std::io::Result<&str> {
        if self.no_messages || self.walk.files.is_empty() { return Ok(""); }
        Ok(self.walk.ignore_file_warnings(&std::env::current_dir()?))
    }

    pub fn stream(&self, text: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::new();
        let start = Instant::now();
        let mut stats = events::Stats::default();
        let hit = self.scan_input(operands::Operand::STDIN, text, &mut out, &mut stats);
        self.summary(&stats, start, &mut out);
        (out, hit)
    }
    fn summary(&self, stats: &events::Stats, start: Instant, out: &mut Vec<u8>) {
        if self.files {
            return;
        }
        match (self.json, self.quiet, self.stats) {
            (true, false, _) | (true, _, true) => stats.summary(start, out),
            (_, _, true) => stats.plain_summary(start, out),
            _ => {}
        }
    }
}

impl operands::FileSearch<'_, '_> {
    fn scan(&mut self, text: &[u8]) -> bool {
        let q = self.query;
        match q.search_output() {
            SearchOutput::Json => self.json_events(text, None),
            SearchOutput::Summary => self.scan_summary(text),
            SearchOutput::Lines => self.scan_lines(text).hits > 0,
        }
    }

    fn scan_lines(&mut self, text: &[u8]) -> line_matches::Totals {
        let q = self.query;
        let path = self.operand.name;
        let show_path = self.operand.show_path;
        let out = &mut *self.out;
        let stats = &mut *self.stats;
        let clock = q.stats.then(Instant::now);
        let color = q.color.unwrap_or(q.terminal_color);
        let style = output::RecordStyle { default_path: show_path, color, prior_output: !out.is_empty() };
        let printer = output::RecordWriter::new(q, q.path_writer(path, color), style);
        let match_style = color.then(|| q.colors.matched.ansi());
        let admission =
            RecordAdmission::new(q.record_limit(), q.after, q.stop_on_nonmatch);
        let start = out.len();
        let terminator = if q.null_data { 0 } else { b'\n' };
        let mut scratch = RegexScratch::default();
        let input = line_matches::RecordInput::new(q, text, terminator, &mut scratch);
        let projection = render::LineProjection::new(q, match_style.as_deref());
        let preview = color && q.max_columns.is_some_and(|limit| limit > 0);
        let render_matches = projection.needs_spans() || preview;
        let evidence = line_matches::Evidence {
            render: render_matches,
            count: q.stats && !q.invert,
            earliest: q.column,
        };
        let mut output = render::LineOutput {
            printer,
            context: output::Context::new(q.before, q.after, q.passthru),
            blocks: multiline::BlockOutput::new(q, input.full.as_ref(), text, color),
            styled: replace::Buffer::default(),
            projection,
            tracked: render_matches,
            column: render::SourceColumn::new(q),
        };
        let totals = input.visit(
            &mut scratch,
            admission,
            evidence,
            |row| output.write(q, out, row),
        );
        if let Some(clock) = clock {
            stats.scanned(&totals, out.len() - start, clock);
        }
        totals
    }
}

impl Lines<'_> {
    /// Plain statistics count every searched file. JSON statistics count only
    /// files that emitted events. Both may report files with zero matches.
    fn reports_unmatched_files(&self) -> bool {
        self.invert || self.result == ResultMode::NonmatchingPaths || self.include_zero
            || self.passthru || (self.stats && self.search_output() != SearchOutput::Json)
    }

    fn candidate_literals(&self) -> Option<impl Iterator<Item=&str>> {
        if self.reports_unmatched_files() || matches!(self.encoding, encoding::Encoding::Legacy(_)) {
            return None;
        }
        let literals = self.patterns.iter().map(Pattern::required_literal);
        if !literals.clone().all(|text| indexable_literal(text, self.byte_input)) { return None; }
        let mut searched = std::collections::HashSet::new();
        Some(literals.filter(move |&literal| self.patterns.len() == 1 || searched.insert(literal)))
    }

    fn candidate_ids(&self, pin: &Pin) -> io::Result<Option<CandidateFiles>> {
        let Some(mut literals) = self.candidate_literals() else { return Ok(None) };
        let Some(first) = literals.next() else {
            return Ok(Some(CandidateFiles::Sorted(Vec::new())));
        };
        let count = pin.files.len();
        let mut cursor = Cursor::open(pin.reader(CONTENT)?)?;
        let first = cursor.candidates(query_trigrams(first), count)?;
        if first.len() == count { return Ok(None); }
        let Some(second) = literals.next() else {
            return Ok(Some(CandidateFiles::Sorted(first)));
        };
        let mut union = BTreeSet::from_iter(first);
        for literal in std::iter::once(second).chain(literals) {
            union.extend(cursor.candidates(query_trigrams(literal), count)?);
            if union.len() == count { return Ok(None); }
        }
        Ok(Some(CandidateFiles::Union(union)))
    }

    fn candidate_file(&self, pin: &Pin, id: usize) -> io::Result<bool> {
        let Some(literals) = self.candidate_literals() else { return Ok(true) };
        if self.patterns.is_empty() { return Ok(false); }
        let mut cursor = Cursor::open(pin.reader(CONTENT)?)?;
        for literal in literals {
            if cursor.contains(query_trigrams(literal), id, pin.files.len())? { return Ok(true); }
        }
        Ok(false)
    }
}

/// Two UTF-8 scalars occupy at most eight bytes. Byte-mode trigrams must
/// also have the same spelling in the raw source and the decoded index.
fn indexable_literal(text: &str, byte_input: bool) -> bool {
    (!byte_input || text.is_ascii()) && (text.len() > 8 || text.chars().nth(2).is_some())
}
