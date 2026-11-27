//! Here-document delimiters establish a physical input boundary before substitutions run.
use super::*;

pub(super) struct HereDelimiter<'a> {
    word: std::borrow::Cow<'a, str>,
    quoted: bool,
    strip_tabs: bool,
}

impl<'a> HereDelimiter<'a> {
    fn read(input: &'a str, strip_tabs: bool) -> (usize, Option<Self>) {
        let mut chars = WordChars::new(input);
        let word = chars.decoded();
        let quoted = chars.quoted();
        (
            chars.consumed(),
            word.map(|word| Self {
                word,
                quoted,
                strip_tabs,
            }),
        )
    }

    fn bounds(&self, tail: &str) -> (usize, usize) {
        let mut start = 0;
        let mut next = 0;
        let mut pending = self.matcher();
        for line in tail.split_inclusive('\n') {
            next += line.len();
            match self.observe_line(&mut pending, line) {
                HereLine::Continues => {}
                HereLine::Delimiter => return (start, next),
                HereLine::Complete => {
                    start = next;
                    pending = self.matcher();
                }
            }
        }
        (next, next)
    }

    fn observe_line(&self, pending: &mut DelimiterMatch, line: &str) -> HereLine {
        let (part, continued) = self.fragment(line);
        pending.consume(part);
        match (continued, pending.remaining) {
            (true, _) => HereLine::Continues,
            (false, Some("")) => HereLine::Delimiter,
            _ => HereLine::Complete,
        }
    }

    fn matcher(&self) -> DelimiterMatch<'_> {
        DelimiterMatch {
            remaining: Some(&self.word),
            indent: self.strip_tabs,
        }
    }

    fn fragment<'s>(&self, line: &'s str) -> (&'s str, bool) {
        if !self.quoted
            && let Some(part) = line.strip_suffix("\\\n")
        {
            let escapes = part.bytes().rev().take_while(|&c| c == b'\\').count();
            if escapes % 2 == 0 {
                return (part, true);
            }
        }
        (line.strip_suffix('\n').unwrap_or(line), false)
    }
}

enum HereLine {
    Continues,
    Delimiter,
    Complete,
}

struct DelimiterMatch<'a> {
    remaining: Option<&'a str>,
    indent: bool,
}

impl DelimiterMatch<'_> {
    fn consume(&mut self, part: &str) {
        let part = if self.indent {
            part.trim_start_matches('\t')
        } else {
            part
        };
        self.indent &= part.is_empty();
        self.remaining = self.remaining.and_then(|rest| rest.strip_prefix(part));
    }
}

impl<'a, F: FnMut(usize)> Parser<'a, F> {
    pub(super) fn here_delimiter(&self, at: usize) -> (usize, Option<HereDelimiter<'a>>) {
        let strip = self.b.get(at) == Some(&b'-');
        let start = self.blanks(at + usize::from(strip));
        let (width, delimiter) = HereDelimiter::read(&self.text[start..], strip);
        (start + width, delimiter)
    }

    pub(super) fn here(
        &mut self,
        mut i: usize,
        delimiters: &mut Vec<HereDelimiter<'a>>,
        owner: Owner,
    ) -> usize {
        for delimiter in delimiters.drain(..) {
            let (body, consumed) = delimiter.bounds(&self.text[i..]);
            if !delimiter.quoted {
                self.within(i + body, |parser| parser.expand_body(i, owner));
            }
            i += consumed;
        }
        i
    }
    fn expand_body(&mut self, start: usize, owner: Owner) {
        let mut cursor = start;
        while let Some(at) = self.expansion_start(cursor) {
            cursor = match self.b[at] {
                b'$' => self.expansion(at, owner),
                b'`' => self.backtick(at, owner),
                _ => at + 2,
            };
        }
    }

    fn expansion_start(&self, cursor: usize) -> Option<usize> {
        let relative = self
            .b
            .get(cursor..)?
            .iter()
            .position(|c| matches!(c, b'$' | b'\\' | b'`'))?;
        Some(cursor + relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_unquoted_here_words_execute_substitutions() {
        let source = "hit() { :; }\ncat <<OUTER <<'RAW'\n$(echo $(hit))\nOUTER\n$(hit)\nRAW\nhit\n";
        let sites: Vec<_> = analyze(source)
            .calls
            .into_iter()
            .filter(|call| call.is_call && &source[call.span.clone()] == "hit")
            .map(|call| call.span.start)
            .collect();
        let inner = source.find("$(hit)").unwrap() + 2;
        assert_eq!(sites, [inner, source.rfind("hit").unwrap()]);
    }

    #[test]
    fn malformed_substitutions_stop_at_the_delimiter() {
        let source = "hit() { :; }\ncat <<END\n$(never_closes\nEND\nhit\n";
        let last = source.rfind("hit").unwrap();
        assert_eq!(
            analyze(source)
                .calls
                .iter()
                .filter(|c| c.is_call && c.span.start == last)
                .count(),
            1
        );
    }

    #[test]
    fn here_delimiters_remove_only_shell_quotes() {
        for (raw, expected, quoted) in [
            ("END next", "END", false),
            ("'E N' next", "E N", true),
            ("'E\"N' next", "E\"N", true),
            (r#""E\q" next"#, r"E\q", true),
            ("E\"N\"D next", "END", true),
            ("'' next", "", true),
            ("E\\\nND next", "END", false),
        ] {
            let (end, marker) = HereDelimiter::read(raw, false);
            let marker = marker.unwrap();
            assert_eq!(&raw[end..], " next");
            assert_eq!(marker.word.as_ref(), expected, "{raw}");
            assert_eq!(marker.quoted, quoted);
        }
    }

    #[test]
    fn continued_lines_are_joined_before_testing_the_delimiter() {
        let (_, marker) = HereDelimiter::read("END", true);
        let marker = marker.unwrap();
        let body = "x\\\nEND\nhit\n";
        let close = "\tE\\\nND\n";
        assert_eq!(
            marker.bounds(&format!("{body}{close}after")),
            (body.len(), body.len() + close.len())
        );
    }
}
