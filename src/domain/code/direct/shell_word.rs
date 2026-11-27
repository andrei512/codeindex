//! Quote removal for one written shell word. Expansion is left to the caller.
pub(super) fn name_start(byte: u8) -> bool { byte == b'_' || byte.is_ascii_alphabetic() }

pub(super) fn name_byte(byte: u8) -> bool { name_start(byte) || byte.is_ascii_digit() }

/// Length of the POSIX portable variable name starting at a byte boundary.
pub(in crate::domain::code) fn name_len(bytes: &[u8]) -> Option<usize> {
    let first = *bytes.first()?;
    if !name_start(first) {
        return None;
    }
    Some(bytes.iter().take_while(|&&c| name_byte(c)).count())
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::domain::code) enum Quoting {
    #[default]
    Bare,
    Double,
    Literal,
}

impl Quoting {
    pub(super) fn toggle(&mut self, byte: u8) -> bool {
        *self = match (*self, byte) {
            (Self::Bare, b'"') => Self::Double,
            (Self::Bare, b'\'') => Self::Literal,
            (Self::Double, b'"') | (Self::Literal, b'\'') => Self::Bare,
            _ => return false,
        };
        true
    }

    fn escapes(self, next: Option<u8>) -> bool {
        match self {
            Self::Bare => true,
            Self::Double => matches!(next, Some(b'$' | b'\x60' | b'"' | b'\\' | b'\n')),
            Self::Literal => false,
        }
    }
}

pub(super) fn separator(c: u8) -> bool {
    c.is_ascii_whitespace() || matches!(c, b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>')
}

pub(in crate::domain::code) struct WordChars<'a> {
    source: &'a str,
    at: usize,
    quote: Quoting,
    quoted: bool,
    dangling_escape: bool,
}

struct DecodedRuns {
    current: std::ops::Range<usize>,
    joined: Option<String>,
}

impl DecodedRuns {
    fn observe(&mut self, source: &str, at: usize, end: usize) {
        if at != self.current.end {
            if !self.current.is_empty() {
                self.joined.get_or_insert_with(String::new).push_str(&source[self.current.clone()]);
            }
            self.current.start = at;
        }
        self.current.end = end;
    }

    fn finish(self, source: &str) -> std::borrow::Cow<'_, str> {
        let last = &source[self.current];
        match self.joined {
            None => last.into(),
            Some(mut text) => {
                text.push_str(last);
                text.into()
            }
        }
    }
}

impl<'a> WordChars<'a> {
    pub(in crate::domain::code) fn new(source: &'a str) -> Self {
        Self {
            source,
            at: 0,
            quote: Quoting::Bare,
            quoted: false,
            dangling_escape: false,
        }
    }

    pub(super) fn double_quoted_tail(source: &'a str) -> Self {
        Self {
            quote: Quoting::Double,
            quoted: true,
            ..Self::new(source)
        }
    }

    pub(in crate::domain::code) fn consumed(&self) -> usize {
        self.at
    }
    pub(in crate::domain::code) fn complete(&self) -> bool {
        self.quote == Quoting::Bare && !self.dangling_escape
    }
    pub(super) fn finished(&self) -> bool {
        self.complete() && self.at == self.source.len()
    }

    pub(in crate::domain::code) fn quoted(&self) -> bool {
        self.quoted
    }

    /// Remove quotes once. Contiguous literal bytes can still borrow the input.
    pub(in crate::domain::code) fn decoded(&mut self) -> Option<std::borrow::Cow<'a, str>> {
        let mut runs = DecodedRuns { current: self.at..self.at, joined: None };
        while let Some((ch, _)) = self.next() {
            runs.observe(self.source, self.at - ch.len_utf8(), self.at);
        }
        let absent = runs.current.is_empty() && !self.quoted;
        if !self.complete() || absent {
            return None;
        }
        Some(runs.finish(self.source))
    }

    fn take(&mut self) -> Option<char> {
        let ch = self.source[self.at..].chars().next()?;
        self.at += ch.len_utf8();
        Some(ch)
    }

    // An escaped newline contributes no character. An unfinished escape
    // terminates the word and invalidates it for callers requiring completeness.
    fn escaped_character(&mut self) -> WordStep {
        self.at += 1;
        let Some(literal) = self.take() else {
            self.dangling_escape = true;
            return WordStep::Stop;
        };
        if literal == '\n' {
            return WordStep::Skip;
        }
        self.quoted = true;
        WordStep::Character(literal, Quoting::Literal)
    }

    pub(in crate::domain::code) fn next_until(
        &mut self,
        boundary: impl Fn(&str) -> bool,
    ) -> Option<(char, Quoting)> {
        loop {
            match self.step(&boundary) {
                WordStep::Skip => {}
                WordStep::Stop => return None,
                WordStep::Character(ch, quote) => return Some((ch, quote)),
            }
        }
    }

    fn step(&mut self, boundary: &impl Fn(&str) -> bool) -> WordStep {
        let tail = &self.source[self.at..];
        let Some(&byte) = tail.as_bytes().first() else {
            return WordStep::Stop;
        };
        if self.quote == Quoting::Bare && boundary(tail) {
            return WordStep::Stop;
        }
        if self.quote.toggle(byte) {
            self.quoted = true;
            self.at += 1;
            return WordStep::Skip;
        }
        let next = tail.as_bytes().get(1).copied();
        if byte == b'\\' && self.quote.escapes(next) {
            return self.escaped_character();
        }
        self.take()
            .map_or(WordStep::Stop, |ch| WordStep::Character(ch, self.quote))
    }
}

enum WordStep {
    Skip,
    Stop,
    Character(char, Quoting),
}

impl Iterator for WordChars<'_> {
    type Item = (char, Quoting);

    fn next(&mut self) -> Option<Self::Item> {
        self.next_until(|tail| separator(tail.as_bytes()[0]))
    }
}
