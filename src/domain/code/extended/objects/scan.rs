//! These languages differ in raw strings, multiline strings and embedded markup.
//! Keep literals opaque, including interpolation, to avoid synthesizing code.
use super::{Kind, Language, Token};
use crate::domain::code::lexer::{block_comment_end, quoted_len, word_char};

fn heredoc_end(text: &str) -> Option<usize> {
    let header = text.strip_prefix("<<<")?;
    let newline = header.find('\n')?;
    let label = header[..newline].trim().trim_matches(['\'', '"']);
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b == b'_' || b.is_ascii_alphanumeric())
    {
        return None;
    }
    let mut offset = 3 + newline + 1;
    for line in text[offset..].split_inclusive('\n') {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if let Some(rest) = trimmed.strip_prefix(label)
            && !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            return Some(offset + line.len() - trimmed.len() + label.len());
        }
        offset += line.len();
    }
    Some(text.len())
}

#[derive(Default)]
struct Dialect {
    embedded_php: bool,
    hack_header: bool,
    hash_comments: bool,
    nested_comments: bool,
    strings: Strings,
}

#[derive(Default)]
enum Strings {
    #[default]
    Escaped,
    Go,
    Java,
    Kotlin,
    Dart,
}

impl Dialect {
    fn for_language(lang: Language) -> Self {
        match lang {
            Language::Php => Self {
                embedded_php: true,
                hash_comments: true,
                ..Default::default()
            },
            Language::Hack => Self {
                hack_header: true,
                hash_comments: true,
                ..Default::default()
            },
            Language::Go => Self {
                strings: Strings::Go,
                ..Default::default()
            },
            Language::Java => Self {
                strings: Strings::Java,
                ..Default::default()
            },
            Language::Kotlin => Self {
                nested_comments: true,
                strings: Strings::Kotlin,
                ..Default::default()
            },
            Language::Dart => Self {
                nested_comments: true,
                strings: Strings::Dart,
                ..Default::default()
            },
            _ => Self::default(),
        }
    }
}

impl Strings {
    fn end(&self, bytes: &[u8], start: usize) -> usize {
        let quote = bytes[start];
        let triple = matches!(self, Self::Kotlin | Self::Dart | Self::Java)
            && bytes[start..].starts_with(&[quote; 3]);
        let raw = match self {
            Self::Go => quote == b'`',
            Self::Kotlin => triple,
            Self::Dart => bytes[..start].strip_suffix(b"r").is_some_and(|prefix| {
                prefix
                    .last()
                    .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_')
            }),
            _ => false,
        };
        start + quoted_len(&bytes[start..], triple, raw, |_| {})
    }
}

impl Dialect {
    fn starts_line_comment(&self, tail: &str) -> bool {
        tail.starts_with("//")
            || (self.hash_comments && tail.starts_with('#') && !tail.starts_with("#["))
    }

    fn comment_end(&self, source: &str, start: usize) -> Option<usize> {
        let tail = &source[start..];
        if self.starts_line_comment(tail) {
            return Some(start + self.line_comment_width(tail));
        }
        tail.starts_with("/*")
            .then(|| block_comment_end(source.as_bytes(), start, self.nested_comments, |_| {}))
    }

    fn line_comment_width(&self, tail: &str) -> usize {
        let line = tail.split_once('\n').map_or(tail, |(line, _)| line);
        if self.embedded_php {
            return line.find("?>").unwrap_or(line.len());
        }
        line.len()
    }

    fn code_token(
        &self,
        source: &str,
        start: usize,
        comment: &mut impl FnMut(usize),
    ) -> (usize, Option<Kind>) {
        if let Some(end) = self.comment_end(source, start) {
            comment(start);
            return (end, None);
        }
        let tail = &source[start..];
        let bytes = source.as_bytes();
        if let Some(width) = self.hash_comments.then(|| heredoc_end(tail)).flatten() {
            return (start + width, Some(Kind::String));
        }
        let c = tail.chars().next().unwrap();
        match c {
            '\'' | '"' | '`' => (self.strings.end(bytes, start), Some(Kind::String)),
            _ if word_char(c) => (
                start
                    + tail
                        .find(|next: char| !word_char(next))
                        .unwrap_or(tail.len()),
                Some(Kind::Word),
            ),
            _ => (
                start + c.len_utf8(),
                (!c.is_whitespace()).then_some(Kind::Punct),
            ),
        }
    }
}

fn code_entry(tail: &str) -> Option<usize> {
    let open = tail.find("<?")? + 2;
    let suffix = &tail[open..];
    let tag = if suffix.starts_with("php") {
        3
    } else if suffix.starts_with("hh") {
        2
    } else {
        0
    };
    Some(open + tag)
}

impl Dialect {
    fn region_token(
        &self,
        source: &str,
        start: usize,
        in_code: &mut bool,
        comment: &mut impl FnMut(usize),
    ) -> Option<(usize, Option<Kind>)> {
        let tail = &source[start..];
        if !*in_code {
            let width = code_entry(tail)?;
            *in_code = true;
            return Some((start + width, None));
        }
        if self.embedded_php && tail.starts_with("?>") {
            *in_code = false;
            return Some((start + 2, None));
        }
        Some(self.code_token(source, start, comment))
    }
}

pub(super) fn scan(source: &str, lang: Language) -> Vec<Token<'_>> {
    tokens_with_comments(source, lang, |_| {})
}

pub(in crate::domain::code) fn tokens_with_comments(
    source: &str,
    lang: Language,
    mut comment: impl FnMut(usize),
) -> Vec<Token<'_>> {
    let dialect = Dialect::for_language(lang);
    let mut in_code = !dialect.embedded_php;
    let mut tokens = Vec::new();
    let (mut i, mut line) = (
        if dialect.hack_header {
            super::hack_header_len(source).unwrap_or(0)
        } else {
            0
        },
        1,
    );
    let b = source.as_bytes();
    while i < b.len() {
        let start = i;
        let Some((end, kind)) = dialect.region_token(source, start, &mut in_code, &mut comment)
        else {
            break;
        };
        i = end;
        tokens.extend(kind.map(|kind| Token {
            kind,
            text: &source[start..i],
            span: start..i,
            line,
        }));
        line += b[start..i].iter().filter(|&&c| c == b'\n').count();
    }
    tokens
}
