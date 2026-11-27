//! Fenced code and raw HTML each own the lines up to their closing boundary.
use super::{Container, fence};

#[derive(Clone, Copy)]
pub(super) enum HtmlEnd {
    BlankLine,
    Literal(&'static str),
    ClosingTag(&'static str),
}

impl HtmlEnd {
    fn reached(self, line: &str) -> bool {
        match self {
            Self::BlankLine => line.is_empty(),
            Self::Literal(end) => line.contains(end),
            Self::ClosingTag(end) => line.match_indices("</").any(|(start, _)| {
                line.as_bytes()
                    .get(start..start + end.len())
                    .is_some_and(|tag| tag.eq_ignore_ascii_case(end.as_bytes()))
            }),
        }
    }
}

fn block_tag_name<'a>(body: &str, folded: &'a mut [u8; 10]) -> Option<&'a [u8]> {
    let name = body.trim_start_matches('/').as_bytes();
    // The longest recognized names are "blockquote" and "figcaption".
    // Reject longer names as soon as the fixed buffer fills.
    let mut len = 0;
    for &byte in name {
        if matches!(byte, b' ' | b'\t' | b'/' | b'>') {
            break;
        }
        *folded.get_mut(len)? = byte.to_ascii_lowercase();
        len += 1;
    }
    Some(&folded[..len])
}

fn html_block(s: &str) -> Option<HtmlEnd> {
    let body = s.strip_prefix('<')?;
    for (start, end) in [("!--", "-->"), ("?", "?>"), ("![CDATA[", "]]>")] {
        if body.starts_with(start) {
            return Some(HtmlEnd::Literal(end));
        }
    }
    let mut folded = [0; 10];
    let name = block_tag_name(body, &mut folded)?;
    match name {
        b"script" => Some(HtmlEnd::ClosingTag("</script>")),
        b"pre" => Some(HtmlEnd::ClosingTag("</pre>")),
        b"style" => Some(HtmlEnd::ClosingTag("</style>")),
        b"textarea" => Some(HtmlEnd::ClosingTag("</textarea>")),
        tag => BLANK_LINE_TAGS.binary_search(&tag).ok().map(|_| HtmlEnd::BlankLine),
    }
}

// These HTML block elements end at a blank line instead of at a specific tag.
const BLANK_LINE_TAGS: &[&[u8]] = &[
    b"address",
    b"article",
    b"aside",
    b"base",
    b"basefont",
    b"blockquote",
    b"body",
    b"caption",
    b"center",
    b"col",
    b"colgroup",
    b"dd",
    b"details",
    b"dialog",
    b"dir",
    b"div",
    b"dl",
    b"dt",
    b"fieldset",
    b"figcaption",
    b"figure",
    b"footer",
    b"form",
    b"frame",
    b"frameset",
    b"h1",
    b"h2",
    b"h3",
    b"h4",
    b"h5",
    b"h6",
    b"head",
    b"header",
    b"hr",
    b"html",
    b"iframe",
    b"legend",
    b"li",
    b"link",
    b"main",
    b"menu",
    b"menuitem",
    b"nav",
    b"noframes",
    b"ol",
    b"p",
    b"param",
    b"search",
    b"section",
    b"summary",
    b"table",
    b"tbody",
    b"td",
    b"tfoot",
    b"th",
    b"thead",
    b"title",
    b"tr",
    b"track",
    b"ul",
];

#[derive(Default)]
pub(super) enum OpaqueBlocks {
    #[default]
    Text,
    Fence(FencedBlock),
    Html(HtmlEnd),
}

pub(super) struct FencedBlock {
    marker: u8,
    width: usize,
    quotes: usize,
    list_indent: usize,
}

impl FencedBlock {
    fn contains(&self, line: &str, container: &Container) -> bool {
        if self.quotes != container.quotes {
            return false;
        }
        if self.list_indent == 0 || line.is_empty() {
            return true;
        }
        container.list != 0 && !(container.item_start && container.list <= self.list_indent)
    }

    /// None keeps the fence open. A container exit returns the line to the block parser.
    fn end(&self, line: &str, container: &Container) -> Option<bool> {
        if self.contains(line, container) {
            self.closes(line, container).then_some(true)
        } else {
            Some(false)
        }
    }

    fn closes(&self, line: &str, container: &Container) -> bool {
        container.indent <= 3 && line.as_bytes().first() == Some(&self.marker)
            && fence(line).is_some_and(|(_, n, rest)| n >= self.width && rest.trim().is_empty())
    }
}

impl OpaqueBlocks {
    pub(super) fn continues(&mut self, line: &str, container: &Container) -> bool {
        let consume_last = match self {
            Self::Text => return false,
            Self::Html(end) if !end.reached(line) => return true,
            Self::Html(end) => !matches!(end, HtmlEnd::BlankLine),
            Self::Fence(block) => match block.end(line, container) {
                Some(consume) => consume,
                None => return true,
            },
        };
        *self = Self::Text;
        consume_last
    }

    pub(super) fn open(
        &mut self,
        line: &str,
        container: &Container,
        include_html_containers: bool,
    ) -> bool {
        if let Some((marker, width, rest)) = fence(line)
            && (marker != b'`' || !rest.contains('`'))
        {
            *self = Self::Fence(FencedBlock {
                marker,
                width,
                quotes: container.quotes,
                list_indent: container.list,
            });
            return true;
        }
        if let Some(end) = html_block(line)
            && (include_html_containers || !matches!(end, HtmlEnd::BlankLine))
        {
            if !end.reached(line) {
                *self = Self::Html(end);
            }
            return true;
        }
        false
    }
}
