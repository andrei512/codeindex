//! Pester and Perl subtests pass executable blocks to the runner. The generic
//! scorer sees their delimiters as nested braces. Each registration owns its
//! own decisions, including decisions written directly in the suite body.
use super::owners::{Opening, OwnerCursor, with_registrations};
use super::{Language, Rule, Score, Token, scan};
use crate::domain::code::roles;

pub(super) fn candidate(lang: Language, test_path: impl FnOnce() -> bool) -> bool {
    match lang {
        Language::Powershell
        | Language::Dart
        | Language::Ruby
        | Language::Php
        | Language::Kotlin => test_path(),
        Language::Perl | Language::Html | Language::Nix => true,
        _ => false,
    }
}

pub(crate) fn perl_source(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|l| l.starts_with("#!") && l.contains("perl"))
        || text.lines().any(|l| {
            l.trim_start().starts_with("use Test::") || l.trim_start().starts_with("use Test2::")
        })
}

// Inspect the header attached to a brace. A hash initializer, variable lookup,
// registration body or assertion callback has no control header.
fn control_body(t: &[Token], pairs: &[usize], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1) else {
        return false;
    };
    let header = if t[prev].text == ")" {
        pairs[prev].checked_sub(1).filter(|&k| k < prev)
    } else {
        Some(prev)
    };
    header.is_some_and(|at| control_keyword(t[at].text) || perl_iteration_variable(t, at))
}

fn control_keyword(word: &str) -> bool {
    [
        "if", "else", "elsif", "elseif", "unless", "for", "foreach", "while", "until", "do",
        "switch", "given", "when", "default", "try", "catch", "finally", "continue", "eval",
        "trap",
    ]
    .iter()
    .any(|x| word.eq_ignore_ascii_case(x))
}

// Perl permits an explicit iteration variable before the list.
fn perl_iteration_variable(t: &[Token], header: usize) -> bool {
    let mut k = header;
    while k > 0
        && header - k < 4
        && (t[k].kind == crate::domain::code::Kind::Word || t[k].text == "$")
    {
        k -= 1;
        if matches!(t[k].text, "foreach" | "for") {
            return true;
        }
    }
    false
}

fn has_registered_suites(lang: Language, role: &roles::RolePath, text: &str) -> bool {
    match lang {
        Language::Perl => perl_suites(role, text),
        Language::Kotlin => text.contains("kotest"),
        _ => true,
    }
}

fn perl_suites(role: &roles::RolePath, text: &str) -> bool {
    if !text.contains("subtest") {
        return false;
    }
    role.is_support() || perl_source(text)
}

fn retained_score(lang: Language, original: &Score) -> Score {
    Score {
        lines: original.lines,
        findings: original
            .findings
            .iter()
            .filter(|f| {
                !matches!(f.rule, Rule::Bumps | Rule::Nesting)
                    && !(lang == Language::Powershell && f.rule == Rule::Complexity)
            })
            .cloned()
            .collect(),
    }
}

pub(super) fn adjust(
    role: &roles::RolePath,
    original: &Score,
    syntax: &super::TestSyntax<'_, '_>,
) -> Option<Score> {
    let (lang, text) = (syntax.lang, syntax.text);
    if matches!(lang, Language::Html | Language::Nix) {
        return super::embedded_tests::adjust(lang, role.path, text, original);
    }
    if !has_registered_suites(lang, role, text) {
        return None;
    }
    let input = syntax.paired();
    let (t, pairs) = (input.tokens, input.pairs);
    let empty = roles::RubyBlocks::default();
    let ruby_blocks = if lang == Language::Ruby {
        syntax
            .ruby_blocks
            .get_or_init(|| roles::RubyBlocks::read(t))
    } else {
        &empty
    };
    let registrations = roles::suite_bodies(lang, input, ruby_blocks)?;
    let declarations = syntax.regions().iter().map(|r| Opening {
        start: r.start,
        end: r.end,
        owner: Depth::new(lang, r.name, t[r.start].line, r.end),
    });
    let registrations = registrations.into_iter().map(|body| Opening {
        start: body.open,
        end: body.end,
        owner: Depth::new(lang, body.name, body.line, body.end),
    });
    let mut openings = with_registrations(declarations, registrations).peekable();
    let syntax = ControlSyntax {
        tokens: t,
        pairs,
        ruby_blocks,
    };
    let mut owners = OwnerCursor::new(t.len(), Depth::new(lang, "<file>", 1, t.len()));
    let mut score = retained_score(lang, original);
    let mut throw_guards = Vec::new();
    while let Some((unit, run)) = owners.next_run(&mut openings, |unit| unit.record(&mut score)) {
        unit.observe_run(&syntax, run, &mut throw_guards);
    }
    owners
        .finish(|unit| unit.record(&mut score))
        .record(&mut score);
    if !throw_guards.is_empty() {
        score.findings.retain(|f| {
            f.rule != Rule::Conditionals || throw_guards.binary_search(&f.line).is_err()
        });
    }
    Some(score)
}

struct Depth<'a> {
    name: &'a str,
    line: usize,
    end: usize,
    branches: usize,
    pester_case: Option<bool>,
    depth: super::control::DepthSummary,
    controls: ControlStack,
}

struct ControlSyntax<'a, 'text> {
    tokens: &'a [Token<'text>],
    pairs: &'a [usize],
    ruby_blocks: &'a roles::RubyBlocks,
}

enum ControlStack {
    Statements(super::control::BraceGrammar, super::control::Suites),
    Ruby(Vec<usize>),
    Blocks(Vec<usize>),
}

impl ControlStack {
    fn new(lang: Language) -> Self {
        match lang {
            Language::Dart | Language::Php | Language::Kotlin => Self::Statements(
                super::control::BraceGrammar::for_language(lang),
                Default::default(),
            ),
            Language::Ruby => Self::Ruby(Vec::new()),
            _ => Self::Blocks(Vec::new()),
        }
    }

    fn depth(&mut self, syntax: &ControlSyntax, i: usize, end: usize) -> usize {
        let t = syntax.tokens;
        let (ends, opens) = match self {
            Self::Statements(grammar, suites) => {
                return suites.depth(grammar.with_tokens(t, syntax.pairs), i, end);
            }
            Self::Ruby(ends) => (ends, syntax.ruby_blocks.control_end(t, i)),
            Self::Blocks(ends) => {
                let opens = t[i].text == "{"
                    && syntax.pairs[i] < t.len()
                    && control_body(t, syntax.pairs, i);
                (ends, opens.then(|| syntax.pairs[i]))
            }
        };
        super::control::expire(ends, i);
        ends.extend(opens);
        ends.len()
    }
}

impl<'a> Depth<'a> {
    fn new(lang: Language, name: &'a str, line: usize, end: usize) -> Self {
        Self {
            name,
            line,
            end,
            branches: 0,
            pester_case: (lang == Language::Powershell).then(|| name.eq_ignore_ascii_case("It")),
            depth: Default::default(),
            controls: ControlStack::new(lang),
        }
    }

    fn observe_run(
        &mut self,
        syntax: &ControlSyntax,
        run: std::ops::Range<usize>,
        throw_guards: &mut Vec<usize>,
    ) {
        for i in run {
            self.observe(syntax, i, throw_guards);
        }
    }

    fn observe(&mut self, syntax: &ControlSyntax, i: usize, throw_guards: &mut Vec<usize>) {
        if let Some(case) = self.pester_case {
            let token = &syntax.tokens[i];
            self.branches += usize::from(scan::branch(Language::Powershell, token.text));
            if case && standalone_throw_guard(syntax.tokens, syntax.pairs, i) {
                throw_guards.push(token.line);
            }
        }
        self.depth.observe(self.controls.depth(syntax, i, self.end));
    }

    fn record(&self, score: &mut Score) {
        score.record(Rule::Complexity, self.branches + 1, self.line, self.name);
        self.depth.record(score, self.line, self.name);
    }
}

/// A single-statement throw guard is a Pester assertion. Keep a conditional
/// finding when another decision shares the line (findings have line precision).
fn standalone_throw_guard(t: &[Token], pairs: &[usize], i: usize) -> bool {
    if !pester_throw_body(t, pairs, i) {
        return false;
    }
    let line = t[i].line;
    let first = t[..i].partition_point(|v| v.line < line);
    let after = i + t[i..].partition_point(|v| v.line == line);
    t[first..after]
        .iter()
        .filter(|v| scan::branch(Language::Powershell, v.text))
        .take(2)
        .count()
        == 1
}

fn pester_throw_body(t: &[Token], pairs: &[usize], i: usize) -> bool {
    let syntax = super::PairedSyntax { tokens: t, pairs };
    if !t[i].text.eq_ignore_ascii_case("if") {
        return false;
    }
    let Some(open) = syntax.close_paren(i + 1).map(|end| end + 1) else {
        return false;
    };
    if scan::at(t, open) != "{" || pairs[open] >= t.len() {
        return false;
    }
    let close = pairs[open];
    throw_statement(t, open, close) && !pester_alternative(t, close + 1)
}

fn pester_alternative(tokens: &[Token], next: usize) -> bool {
    ["else", "elseif"]
        .iter()
        .any(|word| scan::at(tokens, next).eq_ignore_ascii_case(word))
}

fn throw_statement(tokens: &[Token], open: usize, close: usize) -> bool {
    if !scan::at(tokens, open + 1).eq_ignore_ascii_case("throw") {
        return false;
    }
    let literal = tokens
        .get(open + 2)
        .is_some_and(|arg| arg.kind == crate::domain::code::Kind::String);
    literal && throw_terminator(tokens, open, close)
}

fn throw_terminator(tokens: &[Token], open: usize, close: usize) -> bool {
    close == open + 3 || (close == open + 4 && scan::at(tokens, open + 3) == ";")
}
