//! Ripgrep's newline-delimited events for UTF-8 text.
use super::{
    super::json::{array, object, quote},
    Lines,
};
use std::{
    collections::VecDeque,
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
    pub(super) fn scanned(
        &mut self,
        bytes: usize,
        printed: usize,
        lines: usize,
        matches: usize,
        start: Instant,
    ) {
        self.searches += 1;
        self.matched_files += usize::from(lines > 0);
        self.bytes += bytes;
        self.printed += printed;
        self.lines += lines;
        self.matches += matches;
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
fn record(
    out: &mut Vec<u8>,
    path: &str,
    body: &[u8],
    line: usize,
    offset: usize,
    matched: bool,
    ranges: &[Range<usize>],
) {
    let subs = ranges.iter().map(|r| {
        object(&[
            ("match", binary(&body[r.clone()])),
            ("start", r.start.to_string()),
            ("end", r.end.to_string()),
        ])
    });
    event(
        out,
        if matched { "match" } else { "context" },
        object(&[
            ("path", path.into()),
            ("lines", binary(body)),
            ("line_number", line.to_string()),
            ("absolute_offset", offset.to_string()),
            ("submatches", array(subs)),
        ]),
    );
}

pub(super) fn search(
    q: &Lines<'_>,
    path: &str,
    source: &[u8],
    out: &mut Vec<u8>,
    total: &mut Stats,
) -> bool {
    let start = Instant::now();
    let initial = out.len();
    let mut stats = Stats {
        searches: 1,
        ..Stats::default()
    };
    let path = text(path);
    let mut pending = VecDeque::new();
    let mut after_end = 0;
    let term = if q.null_data { 0 } else { b'\n' };
    let mut cursor = 0;
    let mut index = 0;
    let view = q.multiline.then(|| q.view(source));
    let spans = view.as_ref().map(|s| q.ranges(s)).unwrap_or_default();
    let blocks = view
        .as_ref()
        .map(|s| super::multiline::blocks(s, &spans))
        .unwrap_or_default();
    let mut groups = blocks.iter().peekable();
    while cursor < source.len() {
        let offset = cursor;
        let mut end = source[cursor..]
            .iter()
            .position(|&b| b == term)
            .map_or(source.len(), |i| cursor + i + 1);
        let mut ranges = Vec::new();
        if let Some(full) = &view {
            if let Some(block) = groups.peek()
                && q.original(source, full, block.start) == cursor
            {
                end = q.original(source, full, block.end);
                ranges.extend(spans[block.hits.clone()].iter().map(|r| {
                    q.original(source, full, r.start) - offset
                        ..q.original(source, full, r.end) - offset
                }));
                groups.next();
            }
        } else {
            let raw = &source[cursor..end];
            let body = raw.strip_suffix(&[term]).unwrap_or(raw);
            let body = if q.crlf {
                body.strip_suffix(b"\r").unwrap_or(body)
            } else {
                body
            };
            let decoded = q.view(body);
            ranges.extend(
                q.ranges(&decoded).into_iter().map(|r| {
                    q.original(body, &decoded, r.start)..q.original(body, &decoded, r.end)
                }),
            );
        }
        let raw = &source[cursor..end];
        let line = index + 1;
        let count =
            raw.iter().filter(|&&b| b == term).count() + usize::from(!raw.ends_with(&[term]));
        index += count;
        cursor = end;
        stats.bytes += raw.len();
        let hit = (ranges.is_empty() == q.invert) && stats.lines < q.max.unwrap_or(usize::MAX);
        if hit {
            if stats.lines == 0 {
                event(out, "begin", object(&[("path", path.clone())]));
            }
            stats.lines += count;
            stats.matches += ranges.len();
            for (n, at, s) in pending.drain(..) {
                record(out, &path, s, n, at, false, &[]);
            }
            record(out, &path, raw, line, offset, true, &ranges);
            after_end = line.saturating_add(q.after);
        } else if line <= after_end {
            record(out, &path, raw, line, offset, false, &[]);
        } else if q.before > 0 {
            if pending.len() == q.before {
                pending.pop_front();
            }
            pending.push_back((line, offset, raw));
        }
        if stats.lines == q.max.unwrap_or(usize::MAX) && line >= after_end {
            break;
        }
    }
    stats.elapsed = start.elapsed();
    stats.printed = out.len() - initial;
    if stats.lines > 0 {
        stats.matched_files = 1;
        event(
            out,
            "end",
            object(&[
                ("path", path),
                ("binary_offset", "null".into()),
                ("stats", stats.json()),
            ]),
        );
    }
    total.searches += 1;
    total.matched_files += stats.matched_files;
    total.bytes += stats.bytes;
    total.printed += stats.printed;
    total.lines += stats.lines;
    total.matches += stats.matches;
    total.elapsed += stats.elapsed;
    stats.lines > 0
}
