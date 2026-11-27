//! Translate ordered regex match boundaries back to the bytes printed by cx.

use std::borrow::Cow;

/// Multiline matching already decoded the whole input. Unchanged bytes keep
/// their offsets; transformed text keeps its ASCII record delimiters.
pub(super) enum Records<'a> {
    PerLine,
    Original(&'a str),
    Transformed {
        lines: std::str::SplitInclusive<'a, char>,
        terminator: char,
    },
}

impl<'a> Records<'a> {
    pub fn new(view: Option<&'a Cow<'_, str>>, terminator: u8) -> Self {
        let terminator = char::from(terminator);
        match view {
            None => Self::PerLine,
            Some(Cow::Borrowed(text)) => Self::Original(text),
            Some(Cow::Owned(text)) => Self::Transformed {
                lines: text.split_inclusive(terminator),
                terminator,
            },
        }
    }

    pub fn decoded(&mut self, start: usize, raw_len: usize) -> Option<&'a str> {
        match self {
            Self::PerLine => None,
            Self::Original(text) => Some(&text[start..start + raw_len]),
            Self::Transformed { lines, terminator } => {
                let line = lines.next().expect("ASCII terminators survive decoding");
                Some(line.strip_suffix(*terminator).unwrap_or(line))
            }
        }
    }
}

pub(super) struct InputRecord<'a> {
    pub line: usize,
    pub raw: &'a [u8],
    pub decoded: Cow<'a, str>,
    pub byte_start: usize,
    pub byte_end: usize,
    pub decoded_range: std::ops::Range<usize>,
}

impl<'a> InputRecord<'a> {
    pub fn terminated(&self) -> bool {
        self.byte_end - self.byte_start > self.raw.len()
    }

    pub(super) fn search_view(&self, query: &super::Lines) -> super::SearchText<'_> {
        let (raw, decoded) = if query.crlf && self.raw.ends_with(b"\r") {
            (&self.raw[..self.raw.len() - 1], &self.decoded[..self.decoded.len() - 1])
        } else {
            (self.raw, self.decoded.as_ref())
        };
        query.search_text(raw, decoded)
    }
}

pub(super) fn input_records<'a>(
    bytes: &'a [u8],
    view: Option<&'a Cow<'_, str>>,
    terminator: u8,
    options: &'a super::Lines,
) -> impl Iterator<Item = InputRecord<'a>> {
    let mut records = Records::new(view, terminator);
    let mut byte_end = 0;
    let mut decoded_end = 0;
    bytes
        .split_inclusive(move |&b| b == terminator)
        .enumerate()
        .map(move |(i, record)| {
            let raw = record.strip_suffix(&[terminator]).unwrap_or(record);
            let byte_start = byte_end;
            byte_end += record.len();
            let decoded = records
                .decoded(byte_start, raw.len())
                .map_or_else(|| options.view(raw), Cow::Borrowed);
            let start = decoded_end;
            decoded_end += decoded.len() + record.len() - raw.len();
            InputRecord {
                line: i + 1,
                raw,
                decoded,
                byte_start,
                byte_end,
                decoded_range: start..decoded_end,
            }
        })
}

pub(super) fn cursor<'a>(
    raw: &'a [u8],
    view: &'a str,
    byte_input: bool,
) -> impl FnMut(usize) -> usize + 'a {
    let mut decoded = 0;
    let mut original = 0;
    move |at| {
        // Both decoders only expand bytes. Equal lengths imply that every
        // decoded boundary has the same byte offset in the source.
        if raw.len() == view.len() {
            return at;
        }
        let step = &view[decoded..at];
        original += if byte_input {
            step.chars().count()
        } else {
            lossy_width(step, &raw[original..])
        };
        decoded = at;
        original
    }
}

fn lossy_width(decoded: &str, raw: &[u8]) -> usize {
    let mut original = 0;
    let mut copied = 0;
    for (at, replacement) in decoded.match_indices(char::REPLACEMENT_CHARACTER) {
        original += at - copied;
        copied = at + replacement.len();
        original += replacement_width(&raw[original..], replacement);
    }
    original + decoded.len() - copied
}

fn replacement_width(bytes: &[u8], replacement: &str) -> usize {
    if bytes.starts_with(replacement.as_bytes()) {
        return replacement.len();
    }
    // The next raw scalar is malformed. At most four bytes determine
    // its error length, including a truncated sequence at end of input.
    let prefix = &bytes[..bytes.len().min(4)];
    let error = std::str::from_utf8(prefix).unwrap_err();
    error.error_len().unwrap_or(prefix.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_decoded_boundary_preserves_its_original_byte_offset() {
        let raw = b"a\xff\xef\xbf\xbd\xe2\x82z\xc2\xa2\xf0\x9f\x92";
        let view = String::from_utf8_lossy(raw);
        let mut offset = cursor(raw, &view, false);
        let boundaries = view.char_indices().map(|(i, _)| i).chain([view.len()]);
        assert_eq!(
            boundaries.map(&mut offset).collect::<Vec<_>>(),
            [0, 1, 2, 5, 7, 8, 10, 13]
        );
        let view: String = raw.iter().copied().map(char::from).collect();
        let mut offset = cursor(raw, &view, true);
        assert!(
            view.char_indices()
                .enumerate()
                .all(|(i, (at, _))| offset(at) == i)
        );
        assert_eq!(offset(view.len()), raw.len());
    }
}
