//! Resolve emphasis delimiters in the visible title. Unmatched markers are
//! already literal text; matching a closer marks only its opener for removal.
use crate::domain::pattern::punctuation_or_symbol;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Neighbor {
    Space,
    Punctuation,
    Other,
}

impl Neighbor {
    fn at(c: Option<char>) -> Self {
        // The Markdown space class uses Zs, horizontal tab, LF, FF and CR.
        // These four Unicode White_Space values instead flank like text.
        match c.unwrap_or(' ') {
            '\u{b}' | '\u{85}' | '\u{2028}' | '\u{2029}' => Self::Other,
            c if c.is_whitespace() => Self::Space,
            c if punctuation_or_symbol(c) => Self::Punctuation,
            _ => Self::Other,
        }
    }
    fn equal_flanks(self, group: Marker) -> bool {
        match self {
            Self::Space => false,
            Self::Punctuation => true,
            Self::Other => group != Marker::Underscore,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Asterisk,
    Underscore,
    SingleTilde,
    DoubleTilde,
}
impl Marker {
    fn from_run(source: &[u8], pos: usize, width: usize) -> Option<Self> {
        match (source[pos], width) {
            (b'*', _) => Some(Self::Asterisk),
            (b'_', _) => {
                let word_byte = |byte: Option<&u8>| byte.is_some_and(u8::is_ascii_alphanumeric);
                let within_word =
                    word_byte(source[..pos].last()) && word_byte(source.get(pos + width));
                (!within_word).then_some(Self::Underscore)
            }
            (b'~', 1) => Some(Self::SingleTilde),
            (b'~', 2) => Some(Self::DoubleTilde),
            _ => None,
        }
    }
}

/// Flanking and width belong to the source run, even after one side has
/// consumed some of its delimiters.
#[derive(Clone, Copy)]
struct Delimiter {
    group: Marker,
    width: usize,
    opens: bool,
    closes: bool,
}
impl Delimiter {
    fn read(source: &str, pos: usize, width: usize) -> Option<Self> {
        let group = Marker::from_run(source.as_bytes(), pos, width)?;
        let before = Neighbor::at(source[..pos].chars().next_back());
        let after = Neighbor::at(source[pos + width..].chars().next());
        // Unequal flank classes point toward the word or punctuation. Equal
        // punctuation permits both directions; equal word characters do so
        // only for a marker that may occur within a word.
        let (opens, closes) = match before.cmp(&after) {
            std::cmp::Ordering::Less => (true, false),
            std::cmp::Ordering::Greater => (false, true),
            std::cmp::Ordering::Equal => {
                let both = before.equal_flanks(group);
                (both, both)
            }
        };
        Some(Self {
            group,
            width,
            opens,
            closes,
        })
    }
    fn class(self) -> usize {
        self.width % 3 + usize::from(self.closes) * 3
    }
    fn accepts(self, class: usize) -> bool {
        // A multiple-of-three sum can pair only if both widths are multiples
        // of three, or neither run is both left- and right-flanking.
        matches!(self.group, Marker::SingleTilde | Marker::DoubleTilde)
            || self.width % 3 == 0
            || (!self.opens && class < 3)
            || (self.width + class % 3) % 3 != 0
    }
}
struct Opener {
    at: usize,
    written: usize,
    remaining: usize,
    group: Marker,
    class: u8,
    prior: usize,
    previous_in_class: usize,
}

const NONE: usize = usize::MAX;

/// Three modulo classes, doubled for an opener that could also close.
/// Each candidate is pushed and popped once, even with many failed closers.
struct Openers {
    runs: Vec<Opener>,
    latest: [[usize; 6]; 4],
    top: usize,
    matched: bool,
}
impl Default for Openers {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            latest: [[NONE; 6]; 4],
            top: NONE,
            matched: false,
        }
    }
}
impl Openers {
    fn boundary(&self) -> usize {
        self.runs.len()
    }

    fn end_label(&mut self, start: usize) {
        while self.top != NONE && self.top >= start {
            self.pop();
        }
    }
    fn pop(&mut self) {
        let opener = &self.runs[self.top];
        self.latest[opener.group as usize][usize::from(opener.class)] = opener.previous_in_class;
        self.top = opener.prior;
    }
    fn candidate(&self, marker: Delimiter, base: usize) -> Option<usize> {
        let top = self.top;
        if top == NONE || top < base {
            return None;
        }
        let last = &self.runs[top];
        if last.group == marker.group && marker.accepts(usize::from(last.class)) {
            return Some(top);
        }
        self.latest[marker.group as usize]
            .iter()
            .copied()
            .enumerate()
            .filter(|&(class, id)| id != NONE && id >= base && marker.accepts(class))
            .map(|(_, id)| id)
            .max()
    }

    fn close(&mut self, marker: Delimiter, base: usize, title: &mut String) -> usize {
        let mut remaining = marker.width;
        while remaining > 0 {
            let Some(id) = self.candidate(marker, base) else {
                break;
            };
            remaining -= self.consume_opener(id, remaining);
            self.mark_consumed(id, title);
        }
        remaining
    }
    fn consume_opener(&mut self, id: usize, remaining: usize) -> usize {
        while self.top != id {
            self.pop();
        }
        let opener = &mut self.runs[id];
        let count = remaining.min(opener.remaining);
        opener.remaining -= count;
        count
    }
    fn mark_consumed(&mut self, id: usize, title: &mut String) {
        if self.runs[id].remaining != 0 {
            self.matched = true;
            return;
        }
        self.pop();
        // No later delimiter position can be invalidated when the latest
        // record is reclaimed. Bound the shifted tail to keep nested work
        // linear, even for many delimiters around a very large title.
        let last = &self.runs[id];
        if id + 1 == self.runs.len() && title.len() - last.at <= 64 {
            title.replace_range(last.at..last.at + last.written, "");
            self.runs.pop();
        } else {
            self.matched = true;
        }
    }
    fn open(&mut self, marker: Delimiter, at: usize, remaining: usize) {
        let class = marker.class();
        let group = marker.group;
        let id = self.runs.len();
        self.runs.push(Opener {
            at,
            written: remaining,
            remaining,
            group,
            class: class as u8,
            prior: self.top,
            previous_in_class: self.latest[group as usize][class],
        });
        self.latest[group as usize][class] = id;
        self.top = id;
    }
    fn resolve(&mut self, marker: Delimiter, title: &mut String, base: usize) -> usize {
        let remaining = if marker.closes {
            self.close(marker, base, title)
        } else {
            marker.width
        };
        if marker.opens && remaining > 0 {
            self.open(marker, title.len(), remaining)
        }
        remaining
    }

    fn finish(self, mut title: String) -> String {
        if !self.matched {
            return title;
        }
        let mut omitted = self
            .runs
            .iter()
            .filter(|r| r.remaining < r.written)
            .map(|r| r.at + r.remaining..r.at + r.written);
        let mut current = omitted.next();
        let mut at = 0;
        // Compact the original UTF-8 buffer once, in source order.
        title.retain(|c| {
            if current.as_ref().is_some_and(|r| at >= r.end) {
                current = omitted.next();
            }
            let keep = current.as_ref().is_none_or(|r| at < r.start);
            at += c.len_utf8();
            keep
        });
        title
    }
}

/// A plain span contains only text that the title renderer copies verbatim.
/// Its closing run can settle immediately without creating a delimiter record.
impl Delimiter {
    fn plain_span(self, source: &str, start: usize) -> Option<usize> {
        let bytes = source.as_bytes();
        let body = start + self.width;
        let plain = bytes[body..]
            .iter()
            .take_while(|b| !b.is_ascii() || b.is_ascii_alphanumeric() || **b == b' ')
            .count();
        let close = body + plain;
        if plain == 0
            || bytes.get(close) != Some(&bytes[start])
            || super::run(bytes, close) != self.width
        {
            return None;
        }
        Self::read(source, close, self.width)?
            .closes
            .then_some(close)
    }
}

#[derive(Default)]
pub(super) struct Emphasis(Option<Box<Openers>>);
impl Emphasis {
    pub(super) fn boundary(&self) -> usize {
        self.0.as_ref().map_or(0, |s| s.boundary())
    }
    pub(super) fn end_label(&mut self, base: usize) {
        if let Some(s) = &mut self.0 {
            s.end_label(base)
        }
    }
    pub(super) fn delimiter(
        &mut self,
        title: &mut String,
        source: &str,
        pos: usize,
        base: usize,
    ) -> usize {
        let bytes = source.as_bytes();
        let width = super::run(bytes, pos);
        let end = pos + width;
        let Some(marker) = Delimiter::read(source, pos, width) else {
            title.push_str(&source[pos..end]);
            return end;
        };
        if marker.opens
            && (!marker.closes || self.0.as_ref().is_none_or(|s| s.top == NONE))
            && let Some(close) = marker.plain_span(source, pos)
        {
            title.push_str(&source[end..close]);
            return close + width;
        }
        let remaining = if marker.opens || self.0.is_some() {
            self.0
                .get_or_insert_with(Box::default)
                .resolve(marker, title, base)
        } else {
            width
        };
        title.push_str(&source[pos..pos + remaining]);
        end
    }
    pub(super) fn finish(self, title: String) -> String {
        match self.0 {
            Some(s) => s.finish(title),
            None => title,
        }
    }
}
