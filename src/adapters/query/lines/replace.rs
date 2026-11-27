//! Compile replacement syntax once, then append into the caller's output buffer.
use super::{Lines, Pattern};
use std::ops::Range;

#[derive(Default)]
pub(super) struct Buffer {
    pub bytes: Vec<u8>,
    pub columns: Vec<usize>,
    captures: Option<Box<crate::domain::pattern::CaptureScratch>>,
}

pub(super) struct Template<'a> {
    source: &'a str,
    parts: Vec<Part<'a>>,
}

enum Part<'a> {
    Text(&'a str),
    Whole,
    Group(&'a str),
}

impl<'a> Part<'a> {
    fn after_dollar(tail: &'a str) -> (Self, &'a str) {
        if let Some(after) = tail.strip_prefix('$') {
            return (Self::Text("$"), after);
        }
        let (key, after) = tail.strip_prefix('{').and_then(|s| s.split_once('}'))
            .unwrap_or_else(|| {
                let end = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(tail.len());
                tail.split_at(end)
            });
        match key {
            "" => (Self::Text("$"), tail),
            "0" => (Self::Whole, after),
            name => (Self::Group(name), after),
        }
    }
}

pub(super) struct Match<'a> {
    pub text: &'a str,
    pub span: Range<usize>,
    pub raw: &'a [u8],
    pub bytes: Range<usize>,
}

impl Match<'_> {
    fn capture(&self) -> crate::domain::pattern::CaptureSpan<'_> {
        use crate::domain::pattern::{CaptureSpan, ChunkEdges};
        CaptureSpan {
            text: self.text,
            span: self.span.clone(),
            edges: ChunkEdges {
                left_invalid: self.text[..self.span.start].ends_with("�") && !self.raw[..self.bytes.start].ends_with("�".as_bytes()),
                right_invalid: self.text[self.span.end..].starts_with("�") && !self.raw[self.bytes.end..].starts_with("�".as_bytes()),
            },
        }
    }
}

impl<'a> Template<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut parts = Vec::new();
        let mut rest = source;
        while let Some((text, tail)) = rest.split_once('$') {
            if !text.is_empty() {
                parts.push(Part::Text(text));
            }
            let (part, after) = Part::after_dollar(tail);
            parts.push(part);
            rest = after;
        }
        if !parts.is_empty() && !rest.is_empty() {
            parts.push(Part::Text(rest));
        }
        Self { source, parts }
    }

    pub fn needs_groups(&self) -> bool {
        self.parts.iter().any(|part| matches!(part, Part::Group(_)))
    }

    pub fn append(
        &self,
        buffer: &mut Buffer,
        query: &Lines<'_>,
        matched: Match,
    ) {
        let hay = matched.text;
        let span = &matched.span;
        let out = &mut buffer.bytes;
        if self.parts.is_empty() {
            out.extend_from_slice(self.source.as_bytes());
            return;
        }
        let mut groups = None;
        for part in &self.parts {
            match part {
                Part::Text(text) => out.extend_from_slice(text.as_bytes()),
                Part::Whole => query.write_capture(out, &hay[span.clone()]),
                Part::Group(name) => {
                    let groups = groups.get_or_insert_with(|| {
                        Groups::find(&query.patterns, &mut buffer.captures, &matched)
                    });
                    if let Some(range) = groups.as_ref().and_then(|groups| groups.range(name)) {
                        query.write_capture(out, &hay[range.clone()]);
                    }
                }
            }
        }
    }
}

impl Lines<'_> {
    fn write_capture(&self, out: &mut Vec<u8>, matched: &str) {
        if self.byte_input {
            // The byte-mode search view maps every source octet to one scalar.
            out.extend(matched.chars().map(|c| c as u8));
        } else {
            out.extend_from_slice(matched.as_bytes());
        }
    }
}

struct Groups<'a, 'arg> {
    pattern: &'a Pattern<'arg>,
    slots: Vec<Option<Range<usize>>>,
}

impl<'a, 'arg> Groups<'a, 'arg> {
    fn find(
        patterns: &'a [Pattern<'arg>],
        scratch: &mut Option<Box<crate::domain::pattern::CaptureScratch>>,
        matched: &Match<'_>,
    ) -> Option<Self> {
        patterns.iter().find_map(|pattern| {
            let slots = match pattern {
                Pattern::Regex(regex) => scratch.get_or_insert_with(Default::default)
                    .groups(regex, matched.capture())?,
                Pattern::Pcre(regex) => regex.captures(matched.text, matched.span.start)
                    .next().filter(|slots| slots.first() == Some(&Some(matched.span.clone())))?,
                Pattern::Literal(_) => return None,
            };
            Some(Self { pattern, slots })
        })
    }

    fn range(&self, name: &str) -> Option<&Range<usize>> {
        let id = match self.pattern {
            Pattern::Regex(regex) => regex.capture_id(name),
            Pattern::Pcre(regex) => regex.id(name),
            Pattern::Literal(_) => None,
        }?;
        self.slots.get(id)?.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::{Template, Match};

    #[test]
    fn dollar_escapes_and_empty_group_names_preserve_literal_text() {
        let template = Template::new("$$/$0/${}/${0}/${missing}/$");
        let mut out = super::Buffer::default();
        template.append(&mut out, &super::Lines::default(), Match { text: "needle", span: 0..6, raw: b"needle", bytes: 0..6 });
        assert_eq!(out.bytes, b"$/needle/${}/needle//$");
    }
}
