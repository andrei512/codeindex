//! Framework base names and written type boundaries in a single source file.
use super::*;

fn framework_base(
    lang: Language,
    name: &str,
    tokens: &[crate::domain::code::Token],
    present: &std::cell::OnceCell<bool>,
) -> bool {
    type Pattern = &'static [&'static str];
    const FRAMEWORKS: &[(Language, &str, &[Pattern])] = &[
        (
            Language::Java,
            "TestCase",
            &[
                &["junit", ".", "framework", ".", "TestCase"],
                &["import", "junit", ".", "framework", ".", "*"],
            ],
        ),
        (
            Language::Swift,
            "XCTestCase",
            &[&["import", "XCTest"], &["XCTest", ".", "XCTestCase"]],
        ),
        (
            Language::Hack,
            "HackTest",
            &[&["Facebook", "\\", "HackTest"], &["MiniTest", "\\"]],
        ),
    ];
    let Some((_, _, alternatives)) = FRAMEWORKS
        .iter()
        .find(|(language, base, _)| *language == lang && *base == name)
    else {
        return false;
    };
    *present.get_or_init(|| {
        alternatives.iter().any(|pattern| {
            tokens.windows(pattern.len()).any(|words| {
                words
                    .iter()
                    .zip(*pattern)
                    .all(|(token, spelling)| token.text == *spelling)
            })
        })
    })
}

fn base(
    lang: Language,
    s: &Syntax,
    mut pos: usize,
    present: &std::cell::OnceCell<bool>,
) -> Option<Base> {
    if s.w(pos) == "\\" {
        pos += 1;
    }
    if !is_word(s, pos) {
        return None;
    }
    while matches!(s.w(pos + 1), "." | "\\") && is_word(s, pos + 2) {
        pos += 2;
    }
    let name = s.w(pos);
    Some(if framework_base(lang, name, s.tokens, present) {
        Base::Framework
    } else {
        Base::Named(name.into())
    })
}

fn anonymous_java_type(s: &Syntax<'_>, i: usize) -> Option<Type<'static>> {
    let mut open = i + 1;
    while open < s.tokens.len()
        && (is_word(s, open) || matches!(s.w(open), "." | "<" | ">" | "," | "?"))
    {
        open += 1;
    }
    if s.w(open) == "("
        && let Some(close) = s.close(open)
        && s.w(close + 1) == "{"
        && let Some(end) = s.close(close + 1)
    {
        return Some(Type {
            keyword: i,
            open: close + 1,
            close: end,
            name: "",
            base: None,
            extension: false,
        });
    }
    None
}

fn type_header(
    lang: Language,
    s: &Syntax,
    start: usize,
    framework: &std::cell::OnceCell<bool>,
) -> Option<(usize, usize, Option<Base>)> {
    let mut header = Header {
        parent: None,
        generics: 0,
    };
    let mut open = start;
    while let Some(word) = header_word(s, open) {
        header.observe(word, || base(lang, s, open + 1, framework));
        open = next_header_word(s, open, word);
    }
    if s.w(open) != "{" {
        return None;
    }
    Some((open, s.close(open)?, header.parent))
}

struct Header {
    parent: Option<Base>,
    generics: usize,
}

impl Header {
    fn observe(&mut self, word: &str, parent: impl FnOnce() -> Option<Base>) {
        match word {
            "<" => self.generics += 1,
            ">" => self.generics = self.generics.saturating_sub(1),
            "extends" | ":" if self.generics == 0 && self.parent.is_none() => {
                self.parent = parent()
            }
            _ => (),
        }
    }
}

fn header_word<'a>(s: &'a Syntax, at: usize) -> Option<&'a str> {
    let word = s.tokens.get(at)?.text;
    (!matches!(word, "{" | "}" | ";" | "class")).then_some(word)
}

fn next_header_word(s: &Syntax, i: usize, word: &str) -> usize {
    if word == "(" {
        s.close(i).unwrap_or(i) + 1
    } else {
        i + 1
    }
}

enum ClassForm {
    Type,
    Extension,
    Trait,
    Anonymous,
}

impl ClassForm {
    fn read(lang: Language, keyword: &str) -> Option<Self> {
        match (lang, keyword) {
            (Language::Java, "new") => Some(Self::Anonymous),
            (Language::Swift, "extension") => Some(Self::Extension),
            (Language::Hack, "trait") => Some(Self::Trait),
            (_, "class" | "interface" | "enum" | "struct" | "trait" | "actor" | "record") => {
                Some(Self::Type)
            }
            _ => None,
        }
    }

    fn at<'a>(
        self,
        lang: Language,
        s: &'a Syntax,
        i: usize,
        framework: &std::cell::OnceCell<bool>,
    ) -> Option<Type<'a>> {
        if matches!(self, Self::Anonymous) {
            return anonymous_java_type(s, i);
        }
        if !is_word(s, i + 1) || matches!(s.w(i + 1), "func" | "var" | "let" | "function") {
            return None;
        }
        let (open, close, mut parent) = type_header(lang, s, i + 2, framework)?;
        if matches!(self, Self::Trait) {
            let mut cursor = open + 1;
            while cursor < close {
                if s.seq(cursor, &["require", "extends"]) {
                    parent = base(lang, s, cursor + 2, framework);
                    break;
                }
                cursor = s.close(cursor).unwrap_or(cursor) + 1;
            }
        }
        Some(Type {
            keyword: i,
            open,
            close,
            name: s.w(i + 1),
            base: parent,
            extension: matches!(self, Self::Extension),
        })
    }
}

pub(super) fn types<'a>(lang: Language, s: &'a Syntax) -> Vec<Type<'a>> {
    let framework = std::cell::OnceCell::new();
    s.tokens
        .iter()
        .enumerate()
        .filter_map(|(i, token)| ClassForm::read(lang, token.text)?.at(lang, s, i, &framework))
        .collect()
}
