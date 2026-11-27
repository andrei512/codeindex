//! Each measured unit owns only the nesting state required by its grammar.
use super::{Language, Token, Unit, at, command, ind, lower_keyword};
use crate::domain::code::score::control::Suites;
use Language::*;

#[derive(Clone, Copy)]
pub(super) enum Style {
    IndentedControl,
    IndentedBlock,
    BracedControl,
    Delimited { discount_body: bool },
}

impl Style {
    pub fn for_language(lang: Language) -> Self {
        match lang {
            Python | Starlark => Self::IndentedControl,
            Makefile | Mako => Self::IndentedBlock,
            Go | Rust | Js | Csharp | C | Cpp | Java | Kotlin | Swift | Dart | ObjectiveC | Php
            | Hack => Self::BracedControl,
            _ => Self::Delimited {
                discount_body: !command(lang) && !matches!(lang, Ruby | Lua),
            },
        }
    }

    pub fn start(self, header: Option<(&str, usize)>) -> State {
        match self {
            Self::IndentedControl => State::IndentedControl(Vec::new()),
            Self::IndentedBlock => {
                State::IndentedBlock(header.into_iter().map(|(s, at)| ind(s, at)).collect())
            }
            Self::BracedControl => State::BracedControl(Suites::default()),
            Self::Delimited { discount_body } => State::Delimited {
                depth: 0,
                discount: usize::from(discount_body && header.is_some()),
            },
        }
    }
}

pub(super) enum State {
    IndentedControl(Vec<usize>),
    IndentedBlock(Vec<usize>),
    BracedControl(Suites),
    Delimited { depth: usize, discount: usize },
}

impl Default for State {
    fn default() -> Self {
        Self::Delimited {
            depth: 0,
            discount: 0,
        }
    }
}

pub(super) struct Input<'s, 'text> {
    pub lang: Language,
    pub braces: super::super::control::BraceGrammar,
    pub source: &'text str,
    pub tokens: &'s [Token<'text>],
    pub pairs: &'s [usize],
    // The last angle delimiter belongs to the token stream, even across unit boundaries.
    pub html_open: bool,
}

impl Unit<'_> {
    pub(super) fn update_nesting(
        &mut self,
        input: &mut Input,
        i: usize,
        first: bool,
        statement: bool,
    ) {
        let context = NestingStep {
            i,
            first,
            statement,
            unit_end: self.end,
            before_body: i < self.body,
            whole_file: self.name == "<file>",
        };
        if let Some(depth) = self.nesting.depth_at(input, context) {
            self.depth.observe(depth);
        }
    }
}

struct NestingStep {
    i: usize,
    first: bool,
    statement: bool,
    unit_end: usize,
    before_body: bool,
    whole_file: bool,
}

impl State {
    fn depth_at(&mut self, input: &mut Input, step: NestingStep) -> Option<usize> {
        let NestingStep {
            i,
            first,
            statement,
            unit_end,
            before_body,
            whole_file,
        } = step;
        let t = input.tokens;
        let token = &t[i];
        Some(match self {
            State::IndentedControl(stack) => statement.then(|| {
                let opens = indented_suite(token.text, at(t, i + 1));
                indentation(stack, ind(input.source, token.span.start), opens)
            })?,
            State::IndentedBlock(stack) => first.then(|| {
                indentation(stack, ind(input.source, token.span.start), true)
                    .saturating_sub(1 + usize::from(!whole_file))
            })?,
            State::BracedControl(suites) => {
                suites.depth(input.braces.with_tokens(t, input.pairs), i, unit_end)
            }
            State::Delimited { depth, discount } => {
                *depth = delimiter_depth(*depth, delimiters(input, i, first, before_body));
                depth.saturating_sub(*discount)
            }
        })
    }
}

fn delimiter_depth(depth: usize, (open, close): (bool, bool)) -> usize {
    (depth + usize::from(open)).saturating_sub(usize::from(close))
}

fn indented_suite(word: &str, next: &str) -> bool {
    match word {
        "if" | "elif" | "else" | "for" | "while" => true,
        "async" => next == "for",
        _ => false,
    }
}

fn indentation(stack: &mut Vec<usize>, indent: usize, opens: bool) -> usize {
    while stack.last().is_some_and(|&parent| parent >= indent) {
        stack.pop();
    }
    if opens {
        stack.push(indent);
    }
    stack.len()
}

fn delimiters(input: &mut Input, i: usize, first: bool, before_body: bool) -> (bool, bool) {
    let (lang, t) = (input.lang, input.tokens);
    let w = t[i].text;
    let prev = i.checked_sub(1).map_or("", |j| t[j].text);
    match lang {
        Html => html_delimiters(&mut input.html_open, t, i),
        Shell => (
            matches!(w, "then" | "do" | "case" | "{"),
            matches!(w, "fi" | "done" | "esac" | "}" | "elif"),
        ),
        Ruby | Lua => keyword_delimiters(lang, prev, w, (first, before_body)),
        CMake => (
            lower_keyword(w, &["if", "while", "foreach", "function", "macro"]),
            lower_keyword(
                w,
                &["endif", "endwhile", "endforeach", "endfunction", "endmacro"],
            ),
        ),
        Sql | PlPgSql => sql_delimiters(lang, prev, w),
        _ => (w == "{", w == "}"),
    }
}
fn keyword_delimiters(
    lang: Language,
    previous: &str,
    word: &str,
    (first, before_body): (bool, bool),
) -> (bool, bool) {
    match (lang, word) {
        (Ruby, "if" | "unless" | "while" | "until") => (
            first || matches!(previous, ";" | "=" | "then" | "else" | "("),
            false,
        ),
        (Lua, "function") if before_body => (false, false),
        _ => (
            matches!(
                word,
                "if" | "for"
                    | "while"
                    | "unless"
                    | "case"
                    | "begin"
                    | "function"
                    | "def"
                    | "repeat"
            ),
            matches!(word, "end" | "until"),
        ),
    }
}

fn sql_delimiters(lang: Language, previous: &str, word: &str) -> (bool, bool) {
    let opening_words: &[_] = match lang {
        PlPgSql => &["begin", "case", "if", "loop"],
        _ => &["begin", "case"],
    };
    let opening = !previous.eq_ignore_ascii_case("end") && lower_keyword(word, opening_words);
    (opening, word.eq_ignore_ascii_case("end"))
}

fn html_delimiters(open: &mut bool, tokens: &[Token], i: usize) -> (bool, bool) {
    match tokens[i].text {
        "<" => html_tag_open(open, tokens, i + 1),
        ">" => {
            *open = false;
            (false, false)
        }
        "/" => (false, *open && at(tokens, i + 1) == ">"),
        _ => (false, false),
    }
}

fn html_tag_open(open: &mut bool, tokens: &[Token], i: usize) -> (bool, bool) {
    let next = at(tokens, i);
    *open = !void_html(next);
    if next == "/" {
        return (false, !void_html(at(tokens, i + 1)));
    }
    let named = tokens
        .get(i)
        .is_some_and(|t| t.kind == crate::domain::code::Kind::Word);
    (*open && named, false)
}

fn void_html(s: &str) -> bool {
    lower_keyword(
        s,
        &[
            "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
            "source", "track", "wbr",
        ],
    )
}
