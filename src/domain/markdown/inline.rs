mod code_span;
use code_span::{Backticks, append_code};
mod emphasis;
mod html;
use super::entities;
use emphasis::Emphasis;
use html::InlineHtml;
use std::collections::HashMap;

enum Escape<'a> {
    Text(&'a str),
    Scalar(char),
}

impl Escape<'_> {
    fn append(self, out: &mut String) {
        match self {
            Self::Text(text) => out.push_str(text),
            Self::Scalar(scalar) => out.push(scalar),
        }
    }
}

fn entity(s: &str) -> Option<(usize, Escape<'_>)> {
    let stop = s.as_bytes().iter().take(34).position(|&c| c == b';')?;
    let name = s.get(1..stop)?;
    let decoded = match name.strip_prefix('#') {
        Some(value) => Escape::Scalar(numeric_entity(value)?),
        None => Escape::Text(entities::entity(name)?),
    };
    Some((stop + 1, decoded))
}

fn numeric_entity(value: &str) -> Option<char> {
    let code = match value.strip_prefix(['x', 'X']) {
        Some(hex) if hex.len() <= 6 => u32::from_str_radix(hex, 16).ok()?,
        Some(_) => return None,
        None => decimal_entity(value)?,
    };
    Some(
        char::from_u32(code)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{fffd}'),
    )
}

fn decimal_entity(value: &str) -> Option<u32> {
    if !(1..=7).contains(&value.len()) {
        return None;
    }
    value.bytes().try_fold(0, |number, byte| {
        let digit = byte.wrapping_sub(b'0');
        (digit < 10).then_some(number * 10 + u32::from(digit))
    })
}

fn escaped_punctuation(bytes: &[u8], slash: usize) -> bool {
    bytes.get(slash + 1).is_some_and(u8::is_ascii_punctuation)
}

fn escape_at(s: &str) -> Option<(usize, Escape<'_>)> {
    match s.as_bytes()[0] {
        b'&' => entity(s),
        b'\\' if escaped_punctuation(s.as_bytes(), 0) => Some((2, Escape::Text(&s[1..2]))),
        _ => None,
    }
}

pub(super) fn unescape(s: &str) -> std::borrow::Cow<'_, str> {
    let mut result = String::new();
    let mut tail = s;
    while let Some((pos, len, replacement)) = tail_escape(tail) {
        result.push_str(&tail[..pos]);
        replacement.append(&mut result);
        tail = &tail[pos + len..];
    }
    if tail.len() == s.len() {
        return s.into();
    }
    result.push_str(tail);
    result.into()
}

fn tail_escape(tail: &str) -> Option<(usize, usize, Escape<'_>)> {
    tail.match_indices(['\\', '&']).find_map(|(pos, _)| {
        escape_at(&tail[pos..]).map(|(len, replacement)| (pos, len, replacement))
    })
}

fn run(b: &[u8], pos: usize) -> usize {
    b[pos..].iter().take_while(|&&x| x == b[pos]).count()
}
/// A Markdown link component may be separated by horizontal space and
/// at most one physical line ending. Other Unicode whitespace is text.
fn after_link_space(source: &str) -> &str {
    let row = source.trim_start_matches([' ', '\t']);
    let next = row
        .strip_prefix("\r\n")
        .or_else(|| row.strip_prefix(['\r', '\n']));
    next.map_or(row, |rest| rest.trim_start_matches([' ', '\t']))
}

pub(super) struct LinkTarget<'a> {
    url: &'a str,
    tail: &'a str,
    separated: bool,
    written: bool,
}

impl<'a> LinkTarget<'a> {
    fn read(source: &'a str) -> Option<Self> {
        let source = after_link_space(source);
        let (url, raw_tail) = if let Some(inner) = source.strip_prefix('<') {
            let end = angle_end(inner.as_bytes())?;
            (&inner[..end], &inner[end + 1..])
        } else {
            let end = bare_end(source.as_bytes())?;
            source.split_at(end)
        };
        let tail = after_link_space(raw_tail);
        Some(Self {
            url,
            tail,
            separated: raw_tail.len() != tail.len(),
            written: source.len() != raw_tail.len(),
        })
    }
    pub(super) fn definition(source: &'a str) -> Option<(&'a str, &'a str)> {
        let target = Self::read(source)?;
        (target.written && (target.tail.is_empty() || target.separated))
            .then_some((target.url, target.tail.trim_end_matches([' ', '\t'])))
    }
    fn inline_tail(self) -> Option<(&'a str, &'a str)> {
        let rest = if self.separated && self.tail.starts_with(['"', '\'', '(']) {
            after_link_space(after_link_title(self.tail)?)
        } else {
            self.tail
        };
        Some((self.url, rest.strip_prefix(')')?))
    }
}

/// Escaped ASCII punctuation is literal in each part of a link target.
/// Return original byte offsets so the parser can borrow the source slice.
fn unescaped_bytes(bytes: &[u8]) -> impl Iterator<Item = (usize, u8)> {
    let mut bytes = bytes.iter().copied().enumerate().peekable();
    std::iter::from_fn(move || {
        loop {
            let (i, byte) = bytes.next()?;
            if byte == b'\\'
                && bytes
                    .next_if(|(_, next)| next.is_ascii_punctuation())
                    .is_some()
            {
                continue;
            }
            return Some((i, byte));
        }
    })
}

fn angle_end(bytes: &[u8]) -> Option<usize> {
    let (at, delimiter) =
        unescaped_bytes(bytes).find(|(_, byte)| matches!(byte, b'<' | b'>' | b'\r' | b'\n'))?;
    (delimiter == b'>').then_some(at)
}

fn bare_end(bytes: &[u8]) -> Option<usize> {
    let mut depth = 0;
    let mut end = bytes.len();
    for (at, byte) in unescaped_bytes(bytes) {
        match byte {
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b')' | 0..=b' ' | 0x7f => {
                end = at;
                break;
            }
            _ => {}
        }
    }
    (depth == 0).then_some(end)
}

#[derive(Clone, Copy)]
pub(super) struct LinkTitle(u8);

impl LinkTitle {
    pub(super) fn open(source: &str) -> Option<(Self, &str)> {
        let closer = match source.as_bytes().first()? {
            b'(' => b')',
            quote @ (b'\'' | b'"') => *quote,
            _ => return None,
        };
        Some((Self(closer), &source[1..]))
    }

    /// A valid unfinished title can continue on the next paragraph row.
    pub(super) fn end<'a>(self, content: &'a str) -> Result<Option<&'a str>, ()> {
        match unescaped_bytes(content.as_bytes())
            .find(|(_, byte)| *byte == self.0 || (self.0 == b')' && *byte == b'('))
        {
            Some((at, byte)) if byte == self.0 => Ok(Some(&content[at + 1..])),
            Some(_) => Err(()),
            None => Ok(None),
        }
    }
}

pub(super) fn after_link_title(rest: &str) -> Option<&str> {
    let (title, content) = LinkTitle::open(rest)?;
    title.end(content).ok().flatten()
}

fn inline_destination(s: &str) -> Option<(&str, usize)> {
    let (url, after) = LinkTarget::read(s.strip_prefix('(')?)?.inline_tail()?;
    Some((url, s.len() - after.len()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkState {
    Candidate,
    Inline,
    Reference,
    NoDestination,
    ContainsLink,
}

#[derive(Clone, Copy)]
struct Label {
    open: usize,
    close: usize, // zero until the closing bracket is seen
    slot: usize,  // parent while open; result index after a nested link resolves
    state: LinkState,
    image: bool,
    contains_label: bool,
}

impl Label {
    /// Closed labels inside an earlier destination or reference suffix are skipped.
    fn visible_after(self, skip: usize) -> bool {
        self.close != 0 && self.open >= skip
    }
}

/// A label token begins at '['; an image token begins one byte before it.
fn label_open(bytes: &[u8], token: usize) -> Option<usize> {
    let open = token + usize::from(bytes[token] == b'!');
    (bytes.get(open) == Some(&b'[')).then_some(open)
}

// Reference definitions are decoded when collected. Inline targets are
// decoded only when the caller actually indexes the link.
#[derive(Clone, Copy)]
pub(super) enum Destination<'a> {
    Inline(&'a str),
    Reference(&'a str),
}
impl Destination<'_> {
    pub(super) fn local_address(self) -> Option<(String, String)> {
        match self {
            Self::Inline(url) => super::target::local_address(&unescape(url)),
            Self::Reference(url) => super::target::local_address(url),
        }
    }
}

struct Labels<'a> {
    text: &'a str,
    defs: &'a HashMap<String, String>,
    pairs: Vec<Label>,
    resolved: Vec<(&'a str, usize)>,
}

impl<'a> Labels<'a> {
    fn new(text: &'a str, defs: &'a HashMap<String, String>) -> Self {
        Self {
            text,
            defs,
            pairs: Vec::new(),
            resolved: Vec::new(),
        }
    }

    fn scan(&mut self, ticks: &mut Backticks) {
        let source = self.text;
        let bytes = source.as_bytes();
        let mut top = usize::MAX;
        let mut html = InlineHtml::default();
        let mut pos = 0;
        while pos < bytes.len() {
            pos = match bytes[pos] {
                b'\\' => pos + 1 + usize::from(escaped_punctuation(bytes, pos)),
                b'`' => ticks.span(source, pos).end,
                b'<' => html.end(source, pos, None).unwrap_or(pos + 1),
                b'!' | b'[' => self.open(&mut top, bytes, pos),
                b']' => self.close_label(&mut top, pos),
                _ => pos + 1,
            };
        }
    }

    fn open(&mut self, top: &mut usize, bytes: &[u8], token: usize) -> usize {
        let Some(open) = label_open(bytes, token) else {
            return token + 1;
        };
        let index = self.pairs.len();
        self.pairs.push(Label {
            open,
            close: 0,
            slot: *top,
            state: LinkState::Candidate,
            image: open != token,
            contains_label: false,
        });
        *top = index;
        open + 1
    }

    fn close_label(&mut self, top: &mut usize, close: usize) -> usize {
        if *top == usize::MAX { close + 1 } else { self.close(top, close) }
    }

    fn close(&mut self, top: &mut usize, close: usize) -> usize {
        let index = *top;
        let contains_label = self.pairs.len() > index + 1;
        let pair = &mut self.pairs[index];
        *top = pair.slot;
        pair.close = close;
        pair.contains_label = contains_label;
        if *top == usize::MAX && !contains_label {
            // A flat label needs no cached decision. Its consumer resolves it once.
            return close + 1;
        }
        let pair = *pair;
        let has_link = pair.state == LinkState::ContainsLink;
        let mut end = close + 1;
        let passes_link = if has_link && !pair.image {
            true
        } else if let Some((dest, stop)) = self.parse_destination(pair) {
            self.remember(index, dest, stop);
            end = stop;
            !pair.image
        } else {
            self.pairs[index].state = LinkState::NoDestination;
            has_link
        };
        if passes_link && let Some(parent) = self.pairs.get_mut(*top) {
            parent.state = LinkState::ContainsLink;
        }
        end
    }

    fn remember(&mut self, index: usize, dest: Destination<'a>, end: usize) {
        let pair = &mut self.pairs[index];
        let (state, url) = match dest {
            Destination::Inline(url) => (LinkState::Inline, url),
            Destination::Reference(url) => (LinkState::Reference, url),
        };
        pair.state = state;
        pair.slot = self.resolved.len();
        self.resolved.push((url, end));
    }

    fn footnote(&self, pair: Label) -> Option<&'a str> {
        let id = &self.text[pair.open + 1..pair.close];
        let name = id.strip_prefix('^')?;
        self.defs
            .contains_key(super::reference::key(id).as_ref())
            .then_some(name)
    }

    fn visit_destination(
        &self,
        pair: Label,
        emit: &mut impl FnMut(usize, Destination),
    ) -> Option<usize> {
        let (destination, end) = self.destination(pair)?;
        if !pair.image {
            emit(pair.open, destination);
        }
        Some(end)
    }

    fn links(&self, mut emit: impl FnMut(usize, Destination)) {
        let mut skip = 0;
        for &pair in &self.pairs {
            if pair.visible_after(skip) {
                skip = self.visit_destination(pair, &mut emit)
                    .filter(|_| !pair.contains_label)
                    .unwrap_or(skip);
            }
        }
    }

    fn destination(&self, pair: Label) -> Option<(Destination<'a>, usize)> {
        match pair.state {
            LinkState::Candidate => self.parse_destination(pair),
            LinkState::Inline | LinkState::Reference => {
                let (url, end) = self.resolved[pair.slot];
                let dest = match pair.state {
                    LinkState::Inline => Destination::Inline(url),
                    _ => Destination::Reference(url),
                };
                Some((dest, end))
            }
            LinkState::NoDestination | LinkState::ContainsLink => None,
        }
    }

    fn parse_destination(&self, pair: Label) -> Option<(Destination<'a>, usize)> {
        let next = pair.close + 1;
        if let Some((url, len)) = inline_destination(&self.text[next..]) {
            return Some((Destination::Inline(url), next + len));
        }
        if self.defs.is_empty() {
            return None;
        }
        let (url, len) = super::reference::lookup(
            &self.text[pair.open + 1..pair.close],
            &self.text[next..],
            self.defs,
        )?;
        Some((Destination::Reference(url), next + len))
    }
}

#[derive(Default)]
struct Title {
    text: String,
    emphasis: Emphasis,
}

impl Title {
    fn character(&mut self, source: &str, pos: usize) -> usize {
        let c = source[pos..].chars().next().unwrap();
        if c == '\n' {
            self.text.push(' ');
            return pos + 1;
        }
        // Words and spaces after a literal character have no inline syntax.
        let next = pos + c.len_utf8();
        let ordinary = source.as_bytes()[next..]
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b' ')
            .count();
        let end = next + ordinary;
        self.text.push_str(&source[pos..end]);
        end
    }
}

struct Inline<'a> {
    labels: Labels<'a>,
    next_label: usize,
    ticks: Backticks,
    html: InlineHtml,
    title: Title,
    // Traverse a label in place, then jump over the destination.
    ends: Vec<(usize, usize, usize)>,
}

impl Inline<'_> {
    fn link(
        &mut self,
        open: usize,
        emit: &mut impl FnMut(usize, Destination),
    ) -> Option<usize> {
        let labels = &self.labels;
        // Source and label positions both advance monotonically.
        while labels.pairs.get(self.next_label).is_some_and(|pair| pair.open < open) {
            self.next_label += 1;
        }
        let pair = *labels.pairs.get(self.next_label).filter(|pair| pair.open == open)?;
        let close = pair.close;
        if close == 0 || pair.state == LinkState::ContainsLink {
            return None;
        }
        if let Some(end) = labels.visit_destination(pair, emit) {
            self.ends.push((close, end, self.title.emphasis.boundary()));
            return Some(open + 1);
        }
        self.title.text.push_str(labels.footnote(pair)?);
        Some(close + 1)
    }

    fn code(&mut self, pos: usize) -> usize {
        let source = self.labels.text;
        let span = self.ticks.span(source, pos);
        if let Some(body) = span.body {
            append_code(&mut self.title.text, body);
        } else {
            self.title.text.push_str(&source[pos..span.end]);
        }
        span.end
    }

    fn escape(&mut self, source: &str, pos: usize) -> Option<usize> {
        let (len, replacement) = escape_at(&source[pos..])?;
        replacement.append(&mut self.title.text);
        Some(pos + len)
    }

    fn emphasis(&mut self, pos: usize) -> usize {
        let base = self.ends.last().map_or(0, |&(_, _, base)| base);
        self.title.emphasis.delimiter(&mut self.title.text, self.labels.text, pos, base)
    }

    fn close_label(&mut self, pos: usize) -> Option<usize> {
        let (_, end, base) = self.ends.pop_if(|(close, _, _)| *close == pos)?;
        self.title.emphasis.end_label(base);
        Some(end)
    }

    fn title_markup(&mut self, pos: usize) -> Option<usize> {
        let source = self.labels.text;
        match source.as_bytes()[pos] {
            b'\\' | b'&' => self.escape(source, pos),
            b'`' => Some(self.code(pos)),
            b'<' => self.html.end(source, pos, Some(&mut self.title.text)),
            b'*' | b'_' | b'~' => Some(self.emphasis(pos)),
            _ => None,
        }
    }

    fn advance(&mut self, pos: usize, emit: &mut impl FnMut(usize, Destination)) -> usize {
        let source = self.labels.text;
        let bytes = source.as_bytes();
        let next = match bytes[pos] {
            b']' => self.close_label(pos),
            b'!' | b'[' => label_open(bytes, pos).and_then(|open| self.link(open, emit)),
            _ => self.title_markup(pos),
        };
        next.unwrap_or_else(|| self.title.character(source, pos))
    }
}

pub(super) fn scan(
    source: &str,
    defs: &HashMap<String, String>,
    render: bool,
    mut emit: impl FnMut(usize, Destination),
) -> String {
    let mut ticks = Backticks::new();
    let mut labels = Labels::new(source, defs);
    if source
        .split_once('[')
        .is_some_and(|(_, rest)| rest.contains(']'))
    {
        labels.scan(&mut ticks);
    }
    if !render {
        labels.links(emit);
        return String::new();
    }
    let mut inline = Inline {
        labels,
        next_label: 0,
        ticks,
        html: InlineHtml::default(),
        title: Title::default(),
        ends: Vec::new(),
    };
    let mut pos = 0;
    while pos < source.len() {
        pos = inline.advance(pos, &mut emit);
    }
    inline.title.emphasis.finish(inline.title.text)
}
