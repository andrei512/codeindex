//! Ripgrep's newline-delimited events for UTF-8 text.
use super::{
    super::json::{array, object, quote},
    Lines,
};
use std::{
    collections::VecDeque,
    fmt::Write,
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
    pub(super) fn summary(&self, start: Instant, out: &mut String) {
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
fn event(out: &mut String, kind: &str, data: String) {
    writeln!(out, "{}", object(&[("type", quote(kind)), ("data", data)])).unwrap();
}
fn record(
    out: &mut String,
    path: &str,
    body: &str,
    line: usize,
    offset: usize,
    matched: bool,
    ranges: &[Range<usize>],
) {
    let subs = ranges.iter().map(|r| {
        object(&[
            ("match", text(&body[r.clone()])),
            ("start", r.start.to_string()),
            ("end", r.end.to_string()),
        ])
    });
    event(
        out,
        if matched { "match" } else { "context" },
        object(&[
            ("path", path.into()),
            ("lines", text(body)),
            ("line_number", line.to_string()),
            ("absolute_offset", offset.to_string()),
            ("submatches", array(subs)),
        ]),
    );
}

pub(super) fn search(
    q: &Lines<'_>,
    path: &str,
    source: &str,
    out: &mut String,
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
    for (index, raw) in source.split_inclusive('\n').enumerate() {
        let offset = stats.bytes;
        stats.bytes += raw.len();
        let line = index + 1;
        let body = raw.strip_suffix('\n').unwrap_or(raw);
        let body = if q.crlf {
            body.strip_suffix('\r').unwrap_or(body)
        } else {
            body
        };
        let ranges = q.ranges(body);
        let hit = (ranges.is_empty() == q.invert) && stats.lines < q.max.unwrap_or(usize::MAX);
        if hit {
            if stats.lines == 0 {
                event(out, "begin", object(&[("path", path.clone())]));
            }
            stats.lines += 1;
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
