use super::{Language, Rule, Score, Token, at, branch};
use crate::domain::code::score::PairedSyntax;

mod rust;

#[derive(Default)]
pub(super) struct Condition<'a> {
    // Retain the header boundary after a finding is emitted: it also controls
    // whether a newline can separate the next condition.
    end: usize,
    header: Option<(usize, &'a str, usize)>,
}

impl<'a> Condition<'a> {
    pub fn finish(&mut self, score: &mut Score) {
        if let Some((line, unit, operators)) = self.header.take() {
            score.record(Rule::Conditionals, operators, line, unit);
        }
    }

    pub fn newline(&mut self, i: usize, continued: impl FnOnce() -> bool, score: &mut Score) {
        if self.at_statement_boundary(i) && !continued() {
            self.finish(score);
        }
    }

    fn at_statement_boundary(&self, i: usize) -> bool {
        self.header.is_some() && i >= self.end
    }

    fn header_ended(&self, word: &str, i: usize) -> bool {
        let closed = self.end != 0 && i == self.end;
        let delimiter = matches!(word, ";" | "{" | ":");
        if closed {
            return true;
        }
        i >= self.end && delimiter
    }

    pub fn begin(&mut self, line: usize, unit: &'a str, end: usize, score: &mut Score) {
        self.finish(score);
        self.header = Some((line, unit, 0));
        self.end = end;
    }

    pub fn token(&mut self, word: &str, i: usize, double_operator: bool, score: &mut Score) {
        let Some((_, _, count)) = &mut self.header else {
            return;
        };
        if double_operator || matches!(word, "and" | "or" | "AND" | "OR") {
            *count += 1;
        }
        if self.header_ended(word, i) {
            self.finish(score);
        }
    }
}

enum CasePhase {
    Selector,
    Pattern,
    Commands,
}

#[derive(Default)]
pub(super) struct ShellCases(Vec<CasePhase>);

impl ShellCases {
    pub fn branch(&mut self, tokens: &[Token], i: usize, first: bool, previous: &str) -> bool {
        let start = first
            || matches!(
                previous,
                ";" | "{" | "(" | ")" | "|" | "&" | "then" | "do" | "else"
            );
        let word = tokens[i].text;
        if self.open_or_close(word, start) {
            return false;
        }
        if let Some(arm) = self.0.last_mut().and_then(|phase| phase.observe(tokens, i)) {
            return arm;
        }
        start && branch(Language::Shell, word)
    }

    fn open_or_close(&mut self, word: &str, start: bool) -> bool {
        if !start {
            return false;
        }
        match word {
            "case" => self.0.push(CasePhase::Selector),
            "esac" => {
                self.0.pop();
            }
            _ => return false,
        }
        true
    }
}

impl CasePhase {
    fn observe(&mut self, tokens: &[Token], i: usize) -> Option<bool> {
        match (tokens[i].text, self) {
            ("in", phase @ Self::Selector) => {
                *phase = Self::Pattern;
                Some(false)
            }
            (")", phase @ Self::Pattern) => {
                *phase = Self::Commands;
                Some(true)
            }
            (";", phase) if arm_terminator(tokens, i) => {
                *phase = Self::Pattern;
                Some(false)
            }
            _ => None,
        }
    }
}

fn arm_terminator(tokens: &[Token], i: usize) -> bool {
    matches!(at(tokens, i + 1), ";" | "&") && tokens[i + 1].span.start == tokens[i].span.end
}

/// Header and expression decisions depend on the grammar. Keep shell's
/// mutable case stack only in the grammar that consumes it.
pub(super) enum LanguageBranches {
    Indented { python: bool },
    Shell(ShellCases),
    Cmake,
    Sql(Language),
    Rust(rust::MatchArms),
    Common(Language),
}

impl LanguageBranches {
    pub(super) fn new(lang: Language, syntax: PairedSyntax) -> Self {
        use Language::*;
        match lang {
            Python | Starlark => Self::Indented {
                python: lang == Python,
            },
            Shell => Self::Shell(ShellCases::default()),
            CMake => Self::Cmake,
            Sql | PlPgSql => Self::Sql(lang),
            Rust => Self::Rust(rust::MatchArms::read(syntax)),
            _ => Self::Common(lang),
        }
    }

    pub(super) fn observe(
        &mut self,
        t: &[Token],
        i: usize,
        first: bool,
        statement: bool,
    ) -> (bool, usize) {
        let prev = || at(t, i.wrapping_sub(1));
        match self {
            Self::Indented { python } => indented_decision(t, i, statement, *python),
            Self::Shell(cases) => (cases.branch(t, i, first, prev()), 0),
            Self::Cmake => (cmake_branch(t, i), 0),
            Self::Sql(lang) => (sql_branch(*lang, t[i].text, prev()), 0),
            Self::Rust(arms) => (rust_branch(t[i].text, arms, i), 0),
            Self::Common(lang) => common_decision(*lang, t, i),
        }
    }
}

fn rust_branch(word: &str, arms: &rust::MatchArms, i: usize) -> bool {
    if word == "=" {
        return arms.contains(i);
    }
    branch(Language::Rust, word)
}

fn cmake_branch(tokens: &[Token], i: usize) -> bool {
    branch(Language::CMake, tokens[i].text) && at(tokens, i + 1) == "("
}

fn sql_branch(lang: Language, word: &str, previous: &str) -> bool {
    branch(lang, word) && !previous.eq_ignore_ascii_case("end")
}

fn indented_decision(t: &[Token], i: usize, statement: bool, python: bool) -> (bool, usize) {
    let word = t[i].text;
    let header = matches!(word, "if" | "elif" | "for" | "while" | "except" | "raise")
        && (statement || matches!(at(t, i.wrapping_sub(1)), ":" | ";"));
    let expression = match word {
        "and" | "or" => true,
        "for" => !header,
        "else" => python && at(t, i + 1) == ":",
        _ => false,
    };
    (header, usize::from(expression))
}

fn common_decision(lang: Language, t: &[Token], i: usize) -> (bool, usize) {
    use Language::*;
    let word = t[i].text;
    let header = branch(lang, word)
        || match lang {
            C | Cpp | ObjectiveC => word == "?",
            Swift => word == "guard",
            _ => false,
        };
    let expression = matches!(lang, Lua | Ruby | Perl | Php) && matches!(word, "and" | "or");
    (header, usize::from(expression))
}
