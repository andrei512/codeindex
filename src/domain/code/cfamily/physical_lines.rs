//! C translation phase two. Keep source coordinates for facts published on the original file.
use std::{borrow::Cow, ops::Range};

pub(super) fn logical_lines(text: &str) -> Cow<'_, str> {
    translate(text, Vec::new, |_, _| {})
}

pub(super) fn translate<'a>(
    text: &'a str,
    raw_strings: impl FnOnce() -> Vec<Range<usize>>,
    mut change: impl FnMut(usize, usize),
) -> Cow<'a, str> {
    if !text.contains(['\r', '\\']) {
        return Cow::Borrowed(text);
    }
    let raw_strings = std::cell::LazyCell::new(raw_strings);
    let mut raw = RawRanges::default();
    let mut out = None;
    let mut copied = 0;
    let edits = PhysicalLines::new(text).filter_map(|line| {
        let edit = line.edit(text)?;
        (!raw.contains(std::cell::LazyCell::force(&raw_strings), line.end)).then_some((line, edit))
    });
    for (line, edit) in edits {
        let cooked = out.get_or_insert_with(|| String::with_capacity(text.len()));
        edit.append(cooked, &text[copied..edit.content_end]);
        change(cooked.len(), line.next - cooked.len());
        copied = line.next;
    }
    match out {
        Some(mut cooked) => {
            cooked.push_str(&text[copied..]);
            Cow::Owned(cooked)
        }
        None => Cow::Borrowed(text),
    }
}
#[derive(Default)]
struct RawRanges {
    next: usize,
}

impl RawRanges {
    fn contains(&mut self, ranges: &[Range<usize>], at: usize) -> bool {
        while ranges.get(self.next).is_some_and(|span| span.end <= at) {
            self.next += 1;
        }
        ranges.get(self.next).is_some_and(|span| span.contains(&at))
    }
}

struct PhysicalLines<'a> {
    text: &'a str,
    next: usize,
}

impl<'a> PhysicalLines<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, next: 0 }
    }
}

impl Iterator for PhysicalLines<'_> {
    type Item = PhysicalLine;

    fn next(&mut self) -> Option<PhysicalLine> {
        let start = self.next;
        let end = start + self.text[start..].find(['\r', '\n'])?;
        let cr = self.text.as_bytes()[end] == b'\r';
        let next = end + 1 + usize::from(cr && self.text.as_bytes().get(end + 1) == Some(&b'\n'));
        self.next = next;
        Some(PhysicalLine {
            start,
            end,
            next,
            cr,
        })
    }
}

struct PhysicalLine {
    start: usize,
    end: usize,
    next: usize,
    cr: bool,
}

struct LineEdit {
    content_end: usize,
    restore_newline: bool,
}

impl LineEdit {
    fn append(&self, cooked: &mut String, unchanged: &str) {
        cooked.push_str(unchanged);
        if self.restore_newline {
            cooked.push('\n');
        }
    }
}

impl PhysicalLine {
    fn edit(&self, text: &str) -> Option<LineEdit> {
        let line = &text[self.start..self.end];
        if let Some(prefix) = line.trim_end_matches([' ', '\t']).strip_suffix('\\') {
            return Some(LineEdit {
                content_end: self.start + prefix.len(),
                restore_newline: false,
            });
        }
        self.cr.then_some(LineEdit {
            content_end: self.end,
            restore_newline: true,
        })
    }
}
// A newline inside a block comment disappears when the preprocessor
// replaces the comment by one space. The lexer preserves physical positions.
pub(super) fn starts_new_line(mut gap: &str) -> bool {
    while let Some((before, comment)) = gap.split_once("/*") {
        let Some(after) = after_block_comment(gap, before, comment) else {
            return true;
        };
        gap = after;
    }
    gap.contains('\n')
}

fn after_block_comment<'a>(gap: &str, before: &str, comment: &'a str) -> Option<&'a str> {
    if before.contains('\n') {
        return None;
    }
    if before.contains("//") && gap.contains('\n') {
        return None;
    }
    comment.split_once("*/").map(|(_, after)| after)
}

pub(super) fn raw_strings(
    text: &str,
    t: &[super::Token],
    language: super::Language,
) -> Vec<Range<usize>> {
    if language != super::Language::Cpp || !text.contains("R\"") {
        return Vec::new();
    }
    t.iter()
        .filter(|token| {
            token.kind == super::Kind::String
                && super::super::lexer::cpp_raw_body(token.text).is_some()
        })
        .map(|token| token.span.clone())
        .collect()
}
