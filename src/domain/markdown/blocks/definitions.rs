//! Resolve Markdown reference definitions before rendering links and headings.
use super::super::reference;
use super::{Blocks, HashMap, Line, ReferenceDefinition, inline};

pub(super) fn valid_title(s: &str) -> bool {
    inline::after_link_title(s) == Some("")
}

enum LabelPiece<'a> {
    Closed { label: &'a str, tail: &'a str },
    Continued,
    Invalid,
}

fn label_piece(row: &str, footnote: bool) -> LabelPiece<'_> {
    match reference::next_label_bracket(row, footnote) {
        Some((end, b']')) if row.as_bytes().get(end + 1) == Some(&b':') => LabelPiece::Closed {
            label: &row[..end],
            tail: &row[end + 2..],
        },
        None => LabelPiece::Continued,
        _ => LabelPiece::Invalid,
    }
}

pub(super) fn definition(s: &str) -> Option<(&str, &str, bool)> {
    let body = s.strip_prefix('[')?;
    let LabelPiece::Closed { label, tail } = label_piece(body, body.starts_with('^')) else {
        return None;
    };
    if !reference::complete(label) {
        return None;
    }
    if label.starts_with('^') {
        return Some((label, "", false));
    }
    let (url, title) = inline::LinkTarget::definition(tail)?;
    (title.is_empty() || valid_title(title)).then_some((label, url, title.is_empty()))
}

fn insert_definition(defs: &mut HashMap<String, String>, label: &str, url: &str) {
    defs.entry(reference::key(label).into_owned())
        .or_insert_with(|| inline::unescape(url).into_owned());
}

/// Bind both forms of reference definition in document order. A paragraph
/// can start with multiline definitions and still have links in its remaining rows.
pub(super) fn bind(blocks: &mut Blocks<'_>, text: &str) -> HashMap<String, String> {
    let mut defs = HashMap::new();
    for candidate in std::mem::take(&mut blocks.definitions) {
        match candidate {
            ReferenceDefinition::Single { label, url } => insert_definition(&mut defs, label, url),
            ReferenceDefinition::Paragraph { block: id, bracket_end } => {
                let block = &mut blocks.entries[id];
                let rows = &blocks.lines[block.lines.clone()];
                let mut skip = 0;
                while let Some(count) = consume(&rows[skip..], text, &mut defs) {
                    skip += count;
                }
                block.lines.start += skip;
                if skip > 0 && block.lines.start >= bracket_end {
                    block.lines.start = block.lines.end;
                }
            }
        }
    }
    defs
}

struct DefinitionRows<'a> {
    remaining: &'a [Line],
    text: &'a str,
}

impl<'a> DefinitionRows<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let (line, rest) = self.remaining.split_first()?;
        self.remaining = rest;
        Some(&self.text[line.body..line.end])
    }

    /// Join only the reference label. Destinations and title continuations
    /// stay borrowed from their original rows.
    fn label(&mut self) -> Option<(std::borrow::Cow<'a, str>, &'a str)> {
        let mut line = self.next()?.strip_prefix('[')?;
        let footnote = line.starts_with('^');
        let mut prefix = String::new();
        loop {
            match label_piece(line, footnote) {
                LabelPiece::Closed { label: part, tail } => {
                    let key = if prefix.is_empty() {
                        std::borrow::Cow::Borrowed(part)
                    } else {
                        prefix.push_str(part);
                        std::borrow::Cow::Owned(prefix)
                    };
                    return reference::complete(&key).then_some((key, tail));
                }
                LabelPiece::Invalid => return None,
                LabelPiece::Continued => {
                    if !footnote && prefix.len() + line.len() > reference::MAX_LABEL_BYTES {
                        return None;
                    }
                    prefix.push_str(line);
                    prefix.push('\n');
                    line = self.next()?;
                }
            }
        }
    }

    fn finish_title(&mut self, start: &str) -> Option<()> {
        let (title, mut content) = inline::LinkTitle::open(start.trim_start_matches([' ', '\t']))?;
        loop {
            if let Some(rest) = title.end(content).ok()? {
                return rest.trim_matches([' ', '\t']).is_empty().then_some(());
            }
            content = self.next()?;
        }
    }

    fn destination(&mut self, same_line: &'a str) -> Option<&'a str> {
        let mut text = same_line.trim_start_matches([' ', '\t']);
        if text.is_empty() {
            text = self.next()?;
        }
        let (url, suffix) = inline::LinkTarget::definition(text)?;
        if suffix.is_empty() {
            // A title on the next line is optional: restore the rows if its
            // delimiter never closes so that their ordinary links are rendered.
            let remaining = self.remaining;
            if self
                .next()
                .and_then(|line| self.finish_title(line))
                .is_none()
            {
                self.remaining = remaining;
            }
        } else {
            self.finish_title(suffix)?;
        }
        Some(url)
    }
}

fn consume(lines: &[Line], text: &str, defs: &mut HashMap<String, String>) -> Option<usize> {
    let mut rows = DefinitionRows {
        remaining: lines,
        text,
    };
    let (label, tail) = rows.label()?;
    let url = if label.starts_with('^') {
        ""
    } else {
        rows.destination(tail)?
    };
    insert_definition(defs, &label, url);
    Some(lines.len() - rows.remaining.len())
}
