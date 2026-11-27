//! Resolve block content after forward reference definitions are known.
use super::{Blocks, HashMap, HeadingStyle, Line, Link, Parsed, attrs, inline, slug};

pub(super) fn resolve<const ANCHORS: bool>(
    mut blocks: Blocks,
    text: &str,
    parsed: &mut Parsed,
    limit: usize,
    address: impl FnMut(inline::Destination<'_>) -> Option<(String, String)>,
) {
    if limit == 0 && parsed.sections.len() == 1 {
        return;
    }
    let defs = super::definitions::bind(&mut blocks, text);
    let mut used = HashMap::new();
    if ANCHORS {
        used.insert("", 1);
    }
    // The scanner emits exactly one block per section in source order.
    let mut headings = parsed.sections.iter_mut().enumerate().skip(1);
    let mut links = Backlinks {
        destinations: &mut parsed.destinations,
        section: 0,
        limit,
        address,
    };
    for (heading, lines) in blocks.content() {
        let Some(style) = heading else {
            links.paragraph(text, lines, &defs);
            continue;
        };
        let (id, section) = headings.next().expect("heading block has a section");
        links.section = id;
        let content = InlineText::new(lines, text);
        let (source, explicit_anchor) = heading_source(content.as_str(), style);
        section.title = links.scan(source, &defs, true, |pos| content.location(lines, pos));
        trim_title(&mut section.title);
        assign_anchor::<ANCHORS>(section, explicit_anchor, &mut used);
    }
}

fn assign_anchor<'a, const ANCHORS: bool>(
    section: &'a mut crate::domain::markdown::Section, explicit: Option<&str>, used: &mut HashMap<&'a str, usize>,
) {
    if ANCHORS {
        section.anchor = explicit.map_or_else(|| slug(&section.title), str::to_owned);
        register_anchor(&mut section.anchor, used);
    }
}

fn trim_title(title: &mut String) {
    title.truncate(title.trim_end().len());
    let leading = title.len() - title.trim_start().len();
    if leading != 0 {
        title.drain(..leading);
    }
}

fn heading_source(raw: &str, style: HeadingStyle) -> (&str, Option<&str>) {
    let (title, id) = attrs(raw);
    if matches!(style, HeadingStyle::Atx) {
        let stripped = title.trim_end_matches('#');
        if stripped.is_empty() || stripped.ends_with([' ', '\t']) {
            return (stripped.trim_end(), id);
        }
    }
    (title, id)
}

fn is_table(text: &str, lines: &[Line]) -> bool {
    lines
        .get(1)
        .is_some_and(|row| table_delimiter(&text[row.body..row.end]))
        && text[lines[0].body..lines[0].end].contains('|')
}

struct Backlinks<'a, F> {
    destinations: &'a mut Vec<Link>,
    section: usize,
    limit: usize,
    address: F,
}

impl<F: FnMut(inline::Destination<'_>) -> Option<(String, String)>> Backlinks<'_, F> {
    fn full(&self) -> bool { self.destinations.len() >= self.limit }

    fn record(&mut self, url: inline::Destination, location: impl FnOnce() -> (usize, usize)) {
        if self.full() { return; }
        if let Some((target, anchor)) = (self.address)(url) {
            let (offset, line) = location();
            self.destinations.push(Link {
                target,
                anchor,
                offset,
                line,
                section: self.section,
            });
        }
    }

    fn scan(&mut self, source: &str, defs: &HashMap<String, String>, title: bool,
        mut location: impl FnMut(usize) -> (usize, usize)) -> String {
        inline::scan(source, defs, title, |pos, url| self.record(url, || location(pos)))
    }

    fn paragraph(&mut self, text: &str, lines: &[Line], defs: &HashMap<String, String>) {
        if self.full() { return; }
        if is_table(text, lines) {
            self.table(text, lines, defs);
            return;
        }
        let content = InlineText::new(lines, text);
        self.scan(content.as_str(), defs, false, |pos| content.location(lines, pos));
    }

    fn table(&mut self, text: &str, lines: &[Line], defs: &HashMap<String, String>) {
        for row in lines.first().into_iter().chain(lines.iter().skip(2)).filter(|r| r.has_bracket) {
            if self.full() { break; }
            table_cells(&text[row.body..row.end], |cell, start, removed| {
                self.scan(cell, defs, false, |pos| {
                    let offset = row.body + start + pos + removed.partition_point(|&r| r <= pos);
                    (offset, row.number)
                });
            });
        }
    }
}

/// Keys borrow the finalized section anchors. Each base remembers the next
/// suffix to try, including when an explicit ID already occupies that suffix.
fn register_anchor<'a>(anchor: &'a mut String, used: &mut HashMap<&'a str, usize>) {
    if let Some(&next) = used.get(anchor.as_str()) {
        let mut suffix = next;
        loop {
            let candidate = format!("{anchor}-{suffix}");
            suffix += 1;
            if !used.contains_key(candidate.as_str()) {
                *used.get_mut(anchor.as_str()).unwrap() = suffix;
                *anchor = candidate;
                break;
            }
        }
    }
    used.insert(anchor.as_str(), 1);
}

fn table_delimiter(line: &str) -> bool {
    let line = line.trim().trim_matches('|');
    !line.is_empty()
        && line.split('|').all(|cell| {
            let marker = cell.trim().trim_matches(':');
            !marker.is_empty() && marker.bytes().all(|b| b == b'-')
        })
}
#[derive(Default)]
struct CellText {
    start: usize,
    copied: usize,
    removals: Vec<usize>,
    decoded: String,
}

impl CellText {
    fn escaped_pipe(&mut self, line: &str, pipe: usize) {
        self.decoded.push_str(&line[self.copied..pipe - 1]);
        self.removals.push(self.decoded.len());
        self.copied = pipe;
    }

    fn emit(&mut self, line: &str, end: usize, scan: &mut impl FnMut(&str, usize, &[usize])) {
        let text = if self.removals.is_empty() {
            &line[self.start..end]
        } else {
            self.decoded.push_str(&line[self.copied..end]);
            self.decoded.as_str()
        };
        scan(text, self.start, &self.removals);
        self.decoded.clear();
        self.removals.clear();
        self.start = end + 1;
        self.copied = self.start;
    }
}

fn table_cells(line: &str, mut scan: impl FnMut(&str, usize, &[usize])) {
    let mut cell = CellText::default();
    for (pipe, _) in line.match_indices('|') {
        if line[cell.start..pipe].ends_with('\\') {
            cell.escaped_pipe(line, pipe);
            continue;
        }
        cell.emit(line, pipe, &mut scan);
    }
    cell.emit(line, line.len(), &mut scan);
}
/// Inline parsing expects one LF between content lines. Ordinary adjacent
/// lines already have that representation in the input. Container prefixes
/// and CRLF require a joined view and an offset map for backlinks.
enum InlineText<'a> {
    Source(&'a str),
    Joined { text: String, starts: Vec<usize> },
}

impl<'a> InlineText<'a> {
    fn new(lines: &[Line], source: &'a str) -> Self {
        if lines.windows(2).all(|w| w[0].end + 1 == w[1].body) {
            return Self::Source(&source[lines[0].body..lines.last().unwrap().end]);
        }
        let mut text = String::new();
        let mut starts = Vec::with_capacity(lines.len());
        for line in lines {
            if !starts.is_empty() {
                text.push('\n');
            }
            starts.push(text.len());
            text.push_str(&source[line.body..line.end]);
        }
        Self::Joined { text, starts }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Source(text) => text,
            Self::Joined { text, .. } => text,
        }
    }

    fn location(&self, lines: &[Line], pos: usize) -> (usize, usize) {
        match self {
            Self::Source(_) => {
                let byte = lines[0].body + pos;
                let i = lines.partition_point(|line| line.body <= byte) - 1;
                (byte, lines[i].number)
            }
            Self::Joined { starts, .. } => {
                let i = starts.partition_point(|&start| start <= pos) - 1;
                (lines[i].body + pos - starts[i], lines[i].number)
            }
        }
    }
}
