mod opaque;
use opaque::OpaqueBlocks;
mod definitions;
use definitions::definition;
mod render;
use super::{Link, Parsed, Section, inline};
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct Line {
    has_bracket: bool,
    start: usize,
    body: usize,
    end: usize,
    number: usize,
}
#[derive(Clone, Copy)]
enum HeadingStyle {
    Atx,
    Setext,
}

/// Candidates are emitted in source order. Multiline definitions can share a
/// paragraph with ordinary text, so binding may advance that block's first row.
enum ReferenceDefinition<'a> {
    Single { label: &'a str, url: &'a str },
    Paragraph { block: usize, bracket_end: usize },
}

struct ContentBlock {
    lines: std::ops::Range<usize>,
    heading: Option<HeadingStyle>,
}
#[derive(Default)]
struct Container {
    quotes: usize,
    list: usize,
    item_start: bool,
    indent: usize,
}

impl Container {
    fn indented_code(&self, line: &str) -> bool {
        self.indent > 3 || line.starts_with('\t')
    }

    fn entered_from(&self, previous: &Self) -> bool {
        self.item_start || self.quotes > previous.quotes
    }

    fn leaves_list(&self, previous: &Self) -> bool {
        self.list == 0 && previous.list > 0
    }
}

impl Line {
    fn title(mut self, line: &str, title: &str) -> Self {
        self.body += title.as_ptr() as usize - line.as_ptr() as usize;
        self.end = self.body + title.len();
        self
    }
}

fn indent(b: &[u8]) -> usize {
    b.iter().take_while(|&&c| c == b' ').count()
}

enum Prefix {
    Quote(usize),
    ContinuedItem(usize),
    Item(usize),
    Body(usize),
}

impl Prefix {
    fn classify(tail: &[u8], active_list: usize) -> Self {
        let spaces = indent(tail);
        let marker = &tail[spaces..];
        if let Some(width) = quote_prefix(marker, spaces) {
            return Self::Quote(spaces + width);
        }
        if (1..=spaces).contains(&active_list) {
            return Self::ContinuedItem(active_list);
        }
        if spaces <= 3 {
            return list_prefix(marker).map_or(Self::Body(spaces), |width| Self::Item(spaces + width));
        }
        Self::Body(spaces)
    }
}

fn content(s: &str, active_list: &mut usize) -> (usize, Container) {
    let b = s.as_bytes();
    let mut p = 0;
    let mut info = Container::default();
    loop {
        match Prefix::classify(&b[p..], *active_list) {
            Prefix::Quote(width) => {
                p += width;
                info.quotes += 1;
            }
            Prefix::ContinuedItem(width) => {
                *active_list = 0;
                info.list = width;
                p += width;
            }
            Prefix::Item(width) => {
                p += width;
                info.list += width;
                info.item_start = true;
            }
            Prefix::Body(spaces) => {
                info.indent = spaces;
                break;
            }
        }
    }
    if info.list > 0 {
        *active_list = info.list;
    }
    (p + info.indent, info)
}

fn quote_prefix(marker: &[u8], spaces: usize) -> Option<usize> {
    if spaces > 3 { return None; }
    let rest = marker.strip_prefix(b">")?;
    Some(1 + usize::from(matches!(rest.first(), Some(b' ' | b'\t'))))
}

/// Bytes occupied by a list marker and its padding, after leading indentation.
fn list_prefix(bytes: &[u8]) -> Option<usize> {
    let marker = match bytes.first()? {
        b'-' | b'*' | b'+' => 1,
        b'0'..=b'9' => {
            let digits = bytes
                .iter()
                .take(9)
                .take_while(|c| c.is_ascii_digit())
                .count();
            if !matches!(bytes.get(digits), Some(b'.' | b')')) {
                return None;
            }
            digits + 1
        }
        _ => return None,
    };
    if !matches!(bytes.get(marker), Some(b' ' | b'\t')) {
        return None;
    }
    let spaces = indent(&bytes[marker..]);
    Some(marker + if (1..=4).contains(&spaces) { spaces } else { 1 })
}

fn fence(s: &str) -> Option<(u8, usize, &str)> {
    let c = *s.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = s.bytes().take_while(|&x| x == c).count();
    (n >= 3).then_some((c, n, &s[n..]))
}
fn atx(s: &str) -> Option<(usize, &str)> {
    let level = s.bytes().take_while(|&c| c == b'#').count();
    if !(1..=6).contains(&level)
        || s.as_bytes()
            .get(level)
            .is_some_and(|c| !matches!(c, b' ' | b'\t'))
    {
        return None;
    }
    Some((level, s[level..].trim()))
}
fn underline(s: &str) -> Option<usize> {
    let (&marker, _) = s.as_bytes().split_first()?;
    let level = match marker {
        b'=' => 1,
        b'-' => 2,
        _ => return None,
    };
    s[1..].trim_end().bytes().all(|byte| byte == marker).then_some(level)
}
fn thematic(s: &str) -> bool {
    let mut iter = s.bytes().filter(|c| !matches!(c, b' ' | b'\t'));
    let Some(c) = iter.next() else {
        return false;
    };
    matches!(c, b'-' | b'_' | b'*') && {
        let mut n = 1;
        for x in iter {
            if x != c {
                return false;
            }
            n += 1;
        }
        n >= 3
    }
}
/// Open sections extend to EOF until a sibling or ancestor heading closes them.
struct SectionTree {
    sections: Vec<Section>,
    open: Vec<usize>,
}

impl SectionTree {
    fn new(path: &str, end: usize) -> Self {
        let root = Section {
            title: path.rsplit('/').next().unwrap_or("").to_owned(),
            anchor: String::new(),
            level: 0,
            parent: None,
            start: 0,
            end,
            line: 1,
        };
        Self {
            sections: vec![root],
            open: vec![0],
        }
    }

    fn heading(&mut self, row: Line, level: usize) {
        let parent = loop {
            let id = *self.open.last().expect("document root stays open");
            if self.sections[id].level < level {
                break id;
            }
            self.sections[id].end = row.start;
            self.open.pop();
        };
        self.open.push(self.sections.len());
        self.sections.push(Section {
            title: String::new(),
            anchor: String::new(),
            level,
            parent: Some(parent),
            start: row.start,
            end: self.sections[0].end,
            line: row.number,
        });
    }
}

/// Committed blocks refer to contiguous rows. The tail is the current paragraph.
#[derive(Default)]
struct Blocks<'a> {
    entries: Vec<ContentBlock>,
    definitions: Vec<ReferenceDefinition<'a>>,
    lines: Vec<Line>,
    start: usize,
    // Exclusive row after the last opening bracket in the pending paragraph.
    bracket_end: usize,
}

impl Blocks<'_> {
    /// Definitions have been bound before rendering. Only rows that still
    /// contain heading or paragraph content are visible to the inline parser.
    fn content(&self) -> impl Iterator<Item = (Option<HeadingStyle>, &[Line])> {
        self.entries
            .iter()
            .filter(|block| !block.lines.is_empty())
            .map(|block| (block.heading, &self.lines[block.lines.clone()]))
    }

    fn pending(&self) -> &[Line] {
        &self.lines[self.start..]
    }
}

fn slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|&c| {
            crate::domain::pattern::letter_mark_or_number(c) || matches!(c, '_' | '-' | ' ')
        })
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}
fn attrs(s: &str) -> (&str, Option<&str>) {
    let s = s.trim_end();
    if !s.ends_with('}') {
        return (s, None);
    }
    let Some(pos) = s.rfind('{') else {
        return (s, None);
    };
    let args = &s[pos + 1..s.len() - 1];
    if args.contains(['{', '}', '<', '>', '\\', '\n', '\r']) {
        return (s, None);
    }
    let id = args
        .split_ascii_whitespace()
        .rev()
        .filter_map(|word| word.strip_prefix('#'))
        .find(|s| !s.is_empty());
    (s[..pos].trim_end(), id)
}

/// Markdown comments inside literal blocks are source examples. Ordinary HTML
/// containers, such as divs, can still contain a native comment directive.
#[derive(Default)]
pub(crate) struct CommentLines {
    opaque: OpaqueBlocks,
    list: usize,
}

impl CommentLines {
    pub(crate) fn allows_comment(&mut self, raw: &str) -> bool {
        let raw = raw.trim_end_matches(['\r', '\n']);
        let (body, container) = content(raw, &mut self.list);
        let line = &raw[body..];
        if self.opaque.continues(line, &container) || container.indented_code(line) {
            return false;
        }
        self.opaque.open(line, &container, false);
        line.starts_with("<!--")
    }
}

pub(super) fn parse<const ANCHORS: bool>(
    path: &str,
    text: &str,
    link_limit: usize,
    address: impl FnMut(inline::Destination<'_>) -> Option<(String, String)>,
) -> Parsed {
    let mut tree = SectionTree::new(path, text.len());
    let fields = if link_limit > 0 {
        BlockContent::All
    } else {
        BlockContent::Headings
    };
    let blocks = scan(text, fields, |row, level| tree.heading(row, level));
    let mut parsed = Parsed {
        sections: tree.sections,
        ..Parsed::default()
    };
    render::resolve::<ANCHORS>(blocks, text, &mut parsed, link_limit, address);
    if ANCHORS {
        parsed.path.push_str(path);
    }
    parsed
}

#[derive(Clone, Copy)]
enum BlockContent {
    All,
    Headings,
    None,
}

impl BlockContent {
    fn keep_paragraph(self, heading: bool, definition: bool, bracket: bool) -> bool {
        match self {
            Self::None => false,
            Self::Headings => heading || definition,
            Self::All => heading || bracket,
        }
    }
}

pub(crate) fn visit_heading_lines(text: &str, mut visit: impl FnMut(usize)) {
    scan(text, BlockContent::None, |row, _| {
        visit(row.number);
    });
}

struct PendingDefinition<'a> {
    label: &'a str,
    url: &'a str,
    first: Line,
}

struct BlockScanner<'a> {
    text: &'a str,
    fields: BlockContent,
    blocks: Blocks<'a>,
    opaque: OpaqueBlocks,
    list_indent: usize,
    container: Container,
    pending_definition: Option<PendingDefinition<'a>>,
    lazy_quote: bool,
}

impl Blocks<'_> {
    fn push_paragraph_line(&mut self, mut row: Line, text: &str, fields: BlockContent) {
        if matches!(fields, BlockContent::None) {
            // Heading locations need only the first row of a setext paragraph.
            if self.pending().is_empty() {
                self.lines.push(row);
            }
            return;
        }
        row.has_bracket = text.as_bytes().contains(&b'[');
        self.lines.push(row);
        if row.has_bracket {
            self.bracket_end = self.lines.len();
        }
    }

    fn finish_paragraph(
        &mut self,
        fields: BlockContent,
        heading: Option<HeadingStyle>,
        text: &str,
    ) {
        let bracket_end = std::mem::take(&mut self.bracket_end);
        let Some(first) = self.pending().first() else { return };
        let definition_prefix = heading.is_none() && text[first.body..first.end].starts_with('[');
        let keep = fields.keep_paragraph(heading.is_some(), definition_prefix, bracket_end > self.start);
        if !keep {
            self.lines.truncate(self.start);
            return;
        }
        if definition_prefix {
            self.definitions
                .push(ReferenceDefinition::Paragraph { block: self.entries.len(), bracket_end });
        }
        self.entries.push(ContentBlock {
            lines: self.start..self.lines.len(),
            heading,
        });
        self.start = self.lines.len();
    }
}

impl<'a> BlockScanner<'a> {
    fn flush(&mut self, heading: Option<HeadingStyle>) {
        self.blocks
            .finish_paragraph(self.fields, heading, self.text);
    }

    /// Container transitions end the old paragraph before we interpret the
    /// new line as a heading, definition, or paragraph continuation.
    fn transition(&mut self, line: &str, current: &Container) -> bool {
        let left_quote = current.quotes < self.container.quotes;
        let new_container = current.entered_from(&self.container);
        self.observe_quote(line.is_empty(), current, left_quote);
        let thematic_break = || current.leaves_list(&self.container) && thematic(line);
        let container_changed = new_container || left_quote;
        if container_changed || thematic_break() {
            self.flush(None);
        }
        !new_container
    }

    fn observe_quote(&mut self, empty: bool, current: &Container, left: bool) {
        if empty && left {
            // The old quotation's list indentation cannot extend into a new block.
            self.list_indent = 0;
        }
        if empty || current.quotes > 0 {
            self.lazy_quote = false;
        } else if left && !self.blocks.pending().is_empty() {
            self.lazy_quote = true;
        }
    }

    fn line(
        &mut self,
        raw: &'a str,
        start: usize,
        number: usize,
        heading: &mut impl FnMut(Line, usize),
    ) {
        let (body, current) = content(raw, &mut self.list_indent);
        let line = &raw[body..];
        if self.opaque.continues(line, &current) {
            return;
        }
        let follows = self.transition(line, &current);
        let previous_list = self.container.list;
        self.container = current;
        if self.finish_definition(line, follows) {
            return;
        }
        let empty = self.blocks.pending().is_empty();
        if self.list_break(line, previous_list, empty) {
            self.flush(None);
            return;
        }
        let row = Line {
            has_bracket: false,
            start,
            body: start + body,
            end: start + raw.len(),
            number,
        };
        if self.container.indent <= 3 {
            if self.block_start(line, row, heading) {
                return;
            }
        } else if empty {
            return;
        }
        self.blocks.push_paragraph_line(row, line, self.fields);
    }

    /// A marker without content ends the paragraph but still establishes list indentation.
    fn list_break(&mut self, line: &str, previous_list: usize, empty: bool) -> bool {
        if line.is_empty() {
            return true;
        }
        if !matches!(line.as_bytes()[0], b'-' | b'*' | b'+' | b'[') {
            return false;
        }
        match line.trim_end() {
            "-" | "*" | "+" if previous_list > 0 || empty => {
                self.list_indent = 2;
                true
            }
            "[x]" | "[X]" | "[ ]" => self.container.item_start,
            _ => false,
        }
    }

    fn retain_atx(&mut self, row: Line, line: &str, title: &str) {
        if matches!(self.fields, BlockContent::None) { return; }
        self.blocks.lines.push(row.title(line, title));
        self.flush(Some(HeadingStyle::Atx));
    }

    fn begin_definition(&mut self, line: &'a str, first: Line) -> bool {
        if !self.blocks.pending().is_empty() {
            return false;
        }
        let Some((label, url, may_have_title)) = definition(line) else {
            return false;
        };
        let entry = PendingDefinition { label, url, first };
        if may_have_title {
            self.pending_definition = Some(entry);
        } else {
            self.publish_definition(entry);
        }
        true
    }

    fn publish_definition(&mut self, entry: PendingDefinition<'a>) {
        if !matches!(self.fields, BlockContent::None) {
            self.blocks.definitions.push(ReferenceDefinition::Single {
                label: entry.label,
                url: entry.url,
            });
        }
    }

    /// A destination may be followed by a title on the next physical row.
    /// The row has already been normalized against its container exactly once.
    fn finish_definition(&mut self, line: &str, follows: bool) -> bool {
        let Some(entry) = self.pending_definition.take() else {
            return false;
        };
        if follows
            && let Some((title, content)) = inline::LinkTitle::open(line.trim_matches([' ', '\t']))
        {
            match title.end(content) {
                Ok(Some(rest)) if rest.is_empty() => {
                    self.publish_definition(entry);
                    return true;
                }
                Ok(None) => {
                    let first = entry.first;
                    let source = &self.text[first.body..first.end];
                    self.blocks.push_paragraph_line(first, source, self.fields);
                    return false;
                }
                _ => {}
            }
        }
        self.publish_definition(entry);
        false
    }

    fn block_start(
        &mut self,
        line: &'a str,
        row: Line,
        heading: &mut impl FnMut(Line, usize),
    ) -> bool {
        if self.opaque.open(line, &self.container, true) {
            self.flush(None);
            return true;
        }
        if let Some((level, title)) = atx(line) {
            self.flush(None);
            heading(row, level);
            self.retain_atx(row, line, title);
            return true;
        }
        if let Some(&first) = self.blocks.pending().first()
            && !self.lazy_quote
            && let Some(level) = underline(line)
        {
            heading(first, level);
            self.flush(Some(HeadingStyle::Setext));
            return true;
        }
        if thematic(line) {
            self.flush(None);
            return true;
        }
        self.begin_definition(line, row)
    }
}

fn scan(
    text: &str,
    fields: BlockContent,
    mut heading: impl FnMut(Line, usize),
) -> Blocks<'_> {
    let mut parser = BlockScanner {
        text,
        fields,
        blocks: Blocks::default(),
        opaque: OpaqueBlocks::default(),
        list_indent: 0,
        container: Container::default(),
        pending_definition: None,
        lazy_quote: false,
    };
    let mut offset = 0;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        parser.line(
            line.strip_suffix('\r').unwrap_or(line),
            offset,
            i + 1,
            &mut heading,
        );
        offset += raw.len();
    }
    if let Some(entry) = parser.pending_definition.take() {
        parser.publish_definition(entry);
    }
    parser.flush(None);
    parser.blocks
}
