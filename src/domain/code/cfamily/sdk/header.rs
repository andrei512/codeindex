//! Extract possible macro writes and literal includes across preprocessor branches.
use super::super::directives::{Directive, WrittenDirective};
use super::super::physical_lines::{logical_lines, starts_new_line};
use super::{Header, HeaderInclude, Include, Language, lexer, literal_name};
#[cfg(test)]
use std::borrow::Cow;

// Only the first arm of an outer #ifndef is treated as an include guard.
// Every nested condition makes its includes optional regardless of its spelling.
#[derive(Default)]
struct Conditions {
    depth: usize,
    outer_guard: bool,
}
impl Conditions {
    fn enter(&mut self, guard: super::super::directives::Guard) {
        if self.depth == 0 {
            self.outer_guard = guard == super::super::directives::Guard::Defined(false);
        }
        self.depth += 1;
    }
    fn alternate(&mut self) -> Option<()> {
        match self.depth {
            0 => None,
            1 => {
                self.outer_guard = false;
                Some(())
            }
            _ => Some(()),
        }
    }
    fn end(&mut self) -> Option<()> {
        self.depth = self.depth.checked_sub(1)?;
        Some(())
    }
    fn optional(&self) -> bool {
        self.depth > 1 || self.depth == 1 && !self.outer_guard
    }
}
fn has_trigraph(text: &str) -> bool {
    // Trigraphs can change the meaning of a directive before tokenization.
    text.contains("??")
        && text
            .as_bytes()
            .windows(3)
            .any(|w| &w[..2] == b"??" && b"=/()!<>-'".contains(&w[2]))
}

fn directive_lines(text: &str) -> Option<String> {
    // Preserve all context when literals or comments can span lines.
    if ["/*", "R\"", "_Pragma"]
        .iter()
        .any(|&marker| text.contains(marker))
    {
        return None;
    }
    Some(
        text.split_inclusive('\n')
            .filter(|line| {
                let line = line.trim_start();
                line.starts_with('#') || line.starts_with("%:")
            })
            .collect(),
    )
}

fn pragma_operator<'a>(token: &lexer::Token<'a>) -> Option<Option<&'a str>> {
    if token.text == "\\" {
        return None;
    }
    if token.kind != lexer::Kind::String || !token.text.contains("pop_macro") {
        return Some(None);
    }
    let body = token.text.trim_matches('"').trim();
    let name = body
        .strip_prefix("pop_macro")?
        .trim()
        .strip_prefix('(')?
        .trim()
        .strip_suffix(')')?
        .trim()
        .strip_prefix(r#"\""#)?
        .strip_suffix(r#"\""#)?;
    Some(Some(name))
}

impl Header {
    pub(super) fn parse(text: &str) -> Option<Self> {
        if has_trigraph(text) {
            return None;
        }
        let translated = logical_lines(text);
        let text = translated.as_ref();
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let filtered = directive_lines(text);
        let text = filtered.as_deref().unwrap_or(text);
        let tokens = lexer::tokens(text, Language::Cpp);
        let mut out = Self::default();
        if text.contains('\\') || text.contains("pop_macro") {
            out.pragma_operators(&tokens)?;
        }
        let mut conditions = Conditions::default();
        for row in tokens.chunk_by(|left, right| {
            left.line == right.line || !starts_new_line(&text[left.span.end..right.span.start])
        }) {
            out.directive(text, row, &mut conditions)?;
        }
        (conditions.depth == 0).then_some(out)
    }

    fn pragma_operators(&mut self, tokens: &[lexer::Token]) -> Option<()> {
        for token in tokens {
            let name = pragma_operator(token)?;
            self.writes.extend(name.map(str::to_owned));
        }
        Some(())
    }

    fn directive(
        &mut self,
        text: &str,
        row: &[lexer::Token],
        conditions: &mut Conditions,
    ) -> Option<()> {
        let Some(WrittenDirective { kind, args }) = WrittenDirective::read(row, true) else {
            return Some(());
        };
        match kind {
            Directive::If(guard) => conditions.enter(guard),
            Directive::Alternate(_) => conditions.alternate()?,
            Directive::End => conditions.end()?,
            Directive::Write(_) => self.written_macro(args)?,
            Directive::Include | Directive::Import | Directive::IncludeNext => {
                self.written_include(text, args, kind, conditions.optional())?;
            }
            Directive::Pragma => self.pragma_macro(args)?,
            _ => {}
        }
        Some(())
    }

    fn written_macro(&mut self, args: &[lexer::Token]) -> Option<()> {
        let name = args.first()?.text;
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return None;
        }
        self.writes.insert(name.to_owned());
        Some(())
    }

    fn written_include(
        &mut self,
        text: &str,
        args: &[lexer::Token],
        directive: Directive,
        conditional: bool,
    ) -> Option<()> {
        let (mut kind, name) = include_name(text, args)?;
        if directive == Directive::IncludeNext {
            kind = Include::Next;
        }
        self.includes.push(HeaderInclude {
            kind,
            name: name.to_owned(),
            conditional,
        });
        Some(())
    }

    fn pragma_macro(&mut self, args: &[lexer::Token]) -> Option<()> {
        let supported = args
            .first()
            .is_some_and(|t| matches!(t.text, "push_macro" | "pop_macro"));
        if supported {
            let name = args.get(2)?.text.strip_prefix('"')?.strip_suffix('"')?;
            self.writes.insert(name.to_owned());
        }
        Some(())
    }
}

fn include_name<'a>(text: &'a str, args: &[lexer::Token<'a>]) -> Option<(Include, &'a str)> {
    let header = super::super::directives::HeaderName::read(text, args)?;
    if header.quoted && args.len() != 1 {
        return None;
    }
    let name = header.name;
    let kind = if header.quoted {
        Include::Quote
    } else {
        Include::Angle
    };
    literal_name(name).then_some((kind, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_includes_resume_the_outer_guard_after_inner_branches() {
        let text = "#ifndef OUTER\n#if INNER\n#include <inside>\n#else\n#include <fallback>\n#endif\n#include <required>\n#else\n#include <other>\n#endif\n#include <outside>\n";
        let header = Header::parse(text).unwrap();
        let includes: Vec<_> = header
            .includes
            .iter()
            .map(|include| (include.name.as_str(), include.conditional))
            .collect();
        assert_eq!(
            includes,
            [
                ("inside", true),
                ("fallback", true),
                ("required", false),
                ("other", true),
                ("outside", false)
            ]
        );
        assert!(Header::parse("#else\n").is_none());
        assert!(Header::parse("#if OPEN\n").is_none());
    }

    #[test]
    fn physical_line_encodings_and_splices_agree_with_translation_order() {
        let pieces = ["", "λ", "\r", "\n", "\\", " \t", "\r\n", "\\ \n"];
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    let input = [a, b, c].concat();
                    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
                    let expected: String = normalized
                        .split_inclusive('\n')
                        .map(|line| {
                            line.strip_suffix('\n')
                                .and_then(|s| s.trim_end_matches([' ', '\t']).strip_suffix('\\'))
                                .unwrap_or(line)
                        })
                        .collect();
                    assert_eq!(logical_lines(&input), expected, "{input:?}");
                }
            }
        }
        assert!(matches!(logical_lines("literal \\n\n"), Cow::Borrowed(_)));
    }
}
