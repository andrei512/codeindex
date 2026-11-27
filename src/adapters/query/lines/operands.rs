use super::{Lines, ResultMode, events};
use crate::adapters::query::Outcome;
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
};

pub(super) fn unescape(value: &str) -> Vec<u8> {
    let mut remaining = value.as_bytes();
    let mut decoded = Vec::with_capacity(remaining.len());
    while let Some((byte, rest)) = next_byte(remaining) {
        decoded.push(byte);
        remaining = rest;
    }
    decoded
}

fn next_byte(bytes: &[u8]) -> Option<(u8, &[u8])> {
    let (&byte, tail) = bytes.split_first()?;
    if byte == b'\\' {
        return Some(escape_byte(tail).unwrap_or((byte, tail)));
    }
    Some((byte, tail))
}

fn escape_byte(tail: &[u8]) -> Option<(u8, &[u8])> {
    let (&code, rest) = tail.split_first()?;
    let simple = match code {
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'0' => 0,
        b'\\' => b'\\',
        b'x' => {
            let [hi, lo, rest @ ..] = rest else { return None };
            let hi = char::from(*hi).to_digit(16)?;
            let lo = char::from(*lo).to_digit(16)?;
            return Some(((hi * 16 + lo) as u8, rest));
        }
        _ => return None,
    };
    Some((simple, rest))
}

pub(super) fn separator(value: &str) -> Result<Option<u8>, String> {
    let decoded = unescape(value);
    match decoded.as_slice() {
        [] => Ok(None),
        [byte] => Ok(Some(*byte)),
        _ => Err(format!(
            "error parsing flag --path-separator: A path separator must be exactly one byte, but the given separator is {} bytes: {value}\nIn some shells on Windows '/' is automatically expanded. Use '//' instead.",
            decoded.len()
        )),
    }
}

impl Lines<'_> {
    pub(super) fn print_path(&self, out: &mut Vec<u8>, path: &str, suffix: &[u8]) {
        self.path_writer(path, false)(out, suffix);
    }

    /// Encode the filename on its first output record and share those bytes
    /// between matching lines. Quiet and pathless output never constructs it.
    pub(super) fn path_writer<'p>(&'p self, path: &'p str, color: bool)
        -> impl FnMut(&mut Vec<u8>, &[u8]) + 'p
    {
        let mut rendered = None;
        move |out, suffix| {
            out.extend_from_slice(rendered.get_or_insert_with(|| self.render_path(path, color)));
            out.extend_from_slice(if self.null_path { b"\0" } else { suffix });
        }
    }

    fn render_path<'p>(&self, path: &'p str, color: bool) -> std::borrow::Cow<'p, [u8]> {
        use std::borrow::Cow;
        let mut bytes = if color {
            Cow::Owned(format!("\x1b[0m{}{path}\x1b[0m", self.colors.path.ansi()).into_bytes())
        } else {
            Cow::Borrowed(path.as_bytes())
        };
        if let Some(separator) = self.path_separator.filter(|&c| c != b'/')
            && bytes.contains(&b'/')
        {
            for byte in bytes.to_mut() {
                if *byte == b'/' { *byte = separator; }
            }
        }
        bytes
    }

    pub(super) fn operand(&self, cwd: &Path, name: &str, outcome: &mut Outcome) -> Option<PathBuf> {
        match cwd.join(name).canonicalize() {
            Ok(path) => Some(path),
            Err(error) => {
                self.operand_error(name, error, &mut outcome.diagnostics);
                None
            }
        }
    }

    fn operand_error(&self, name: &str, error: std::io::Error, diagnostics: &mut String) {
        if self.paths.len() == 1 {
            writeln!(diagnostics, "cx: {name}: IO error for operation on {name}: {error}").unwrap();
        } else {
            writeln!(diagnostics, "cx: {name}: {error}").unwrap();
        }
    }

    pub(super) fn finish(&self, outcome: &mut Outcome, matched: bool) {
        let suppress_error = self.quiet && matched;
        outcome.code = if !outcome.diagnostics.is_empty() && !suppress_error {
            2
        } else {
            u8::from(!matched)
        };
    }

    pub(super) fn scan_input(
        &self,
        operand: Operand<'_>,
        bytes: &[u8],
        out: &mut Vec<u8>,
        stats: &mut events::Stats,
    ) -> bool {
        FileSearch { query: self, operand, out, stats }.input(bytes)
    }

    /// Record-local binary searches treat NUL as a line boundary. Multiline
    /// patterns can consume the original byte and therefore borrow the input.
    fn binary_records<'b>(&self, bytes: &'b [u8]) -> std::borrow::Cow<'b, [u8]> {
        if self.multiline {
            return bytes.into();
        }
        bytes.iter().map(|&byte| if byte == 0 { b'\n' } else { byte }).collect::<Vec<_>>().into()
    }

    fn binary_record_match(&self, bytes: &[u8]) -> bool {
        let mut scratch = super::RegexScratch::default();
        self.max != Some(0)
            && bytes
                .split_inclusive(|&b| b == b'\n' || b == 0)
                .any(|line| {
                    let body = line
                        .strip_suffix(b"\n")
                        .or_else(|| line.strip_suffix(b"\0"))
                        .unwrap_or(line);
                    let body = if self.crlf { body.strip_suffix(b"\r").unwrap_or(body) } else { body };
                    self.first_position(self.search_text(body, &self.view(body)), &mut scratch, false)
                        .is_some()
                        != self.invert
                })
    }
}

pub(super) struct Operand<'a> {
    pub name: &'a str,
    pub show_path: bool,
    pub explicit: bool,
}

impl Operand<'static> {
    pub const STDIN: Self = Self { name: "<stdin>", show_path: false, explicit: true };
}

/// Output, statistics and identity for one input, including any visible prefix before a NUL.
pub(super) struct FileSearch<'a, 'q> {
    pub(super) query: &'a Lines<'q>,
    pub(super) operand: Operand<'a>,
    pub(super) out: &'a mut Vec<u8>,
    pub(super) stats: &'a mut events::Stats,
}

impl FileSearch<'_, '_> {
    fn input(&mut self, bytes: &[u8]) -> bool {
        let decoded = self.query.encoding.decode(bytes);
        let bytes = decoded.as_ref();
        if !self.query.text && !self.query.null_data
            && let Some(nul) = bytes.iter().position(|&b| b == 0)
        {
            return self.run(bytes, nul);
        }
        self.scan(bytes)
    }

    fn run(&mut self, bytes: &[u8], offset: usize) -> bool {
        use super::SearchOutput;
        let skip_binary = !self.operand.explicit && !self.query.binary;
        match self.query.search_output() {
            SearchOutput::Json => self.binary_events(bytes, offset, skip_binary),
            SearchOutput::Summary if skip_binary => self.scan_binary_prefix(bytes, offset),
            SearchOutput::Summary => self.binary_summary(bytes),
            SearchOutput::Lines => self.binary_lines(bytes, offset, skip_binary),
        }
    }

    fn scan_binary_prefix(&mut self, bytes: &[u8], offset: usize) -> bool {
        let prefix = &bytes[..complete_prefix(bytes, offset)];
        let result = self.query.result;
        if result.counts() || result == ResultMode::NonmatchingPaths {
            return self.skipped_binary_summary(prefix);
        }
        !prefix.is_empty() && self.scan_summary(prefix)
    }

    fn binary_events(&mut self, bytes: &[u8], offset: usize, skip_binary: bool) -> bool {
        let prefix = complete_prefix(bytes, offset);
        let source = if skip_binary { (&bytes[..prefix]).into() } else { self.query.binary_records(bytes) };
        self.json_events(&source, Some(events::BinaryBoundary { offset, prefix }))
    }

    fn binary_summary(&mut self, bytes: &[u8]) -> bool {
        let q = self.query;
        if !q.multiline && q.stops_at_first_hit() {
            return q.binary_record_match(bytes);
        }
        self.scan_summary(&q.binary_records(bytes))
    }

    fn binary_lines(&mut self, bytes: &[u8], offset: usize, skip_binary: bool) -> bool {
        if self.query.multiline && !skip_binary {
            return self.report_binary_match(self.query.summary_totals(bytes, false).hits != 0, offset);
        }
        let end = complete_prefix(bytes, offset);
        let totals = self.scan_lines(&bytes[..end]);
        let hit = totals.hits != 0;
        if totals.stopped { return hit; }
        if skip_binary {
            if hit { self.message("WARNING: stopped searching binary file after match", offset); }
            return hit;
        }
        self.report_binary_match(hit || self.query.binary_record_match(&bytes[end..]), offset)
    }

    fn report_binary_match(&mut self, matched: bool, offset: usize) -> bool {
        if matched {
            self.message("binary file matches", offset);
        }
        matched
    }

    fn skipped_binary_summary(&mut self, prefix: &[u8]) -> bool {
        let q = self.query;
        if !q.stats && !q.quiet {
            return q.result == ResultMode::NonmatchingPaths;
        }
        let clock = q.stats.then(std::time::Instant::now);
        let totals = q.summary_totals(prefix, q.stats);
        if let Some(started) = clock {
            self.stats.scanned(&totals, 0, started);
        }
        if q.quiet { totals.hits != 0 } else { q.result == ResultMode::NonmatchingPaths }
    }

    fn message(&mut self, message: &str, offset: usize) {
        if self.query.filename.unwrap_or(self.operand.show_path) {
            let color = self.query.color.unwrap_or(self.query.terminal_color);
            self.out.extend_from_slice(&self.query.render_path(self.operand.name, color));
            self.out.extend_from_slice(b": ");
        }
        writeln!(self.out, "{message} (found \"\\0\" byte around offset {offset})").unwrap();
    }
}

fn complete_prefix(bytes: &[u8], nul: usize) -> usize {
    let block = nul / (64 * 1024) * (64 * 1024);
    bytes[..block].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
}
