use super::duplicates::{self, Body};
use super::owners::{Opening, OwnerCursor};
use super::{Analysis, Language, Rule, Score, Token};
use Language::*;

#[derive(Default)]
struct Unit<'a> {
    name: &'a str,
    start: usize,
    end: usize,
    body: usize,
    args: usize,
    lines: usize,
    last: usize,
    branches: usize,
    depth: super::control::DepthSummary,
    nesting: nesting::State,
}
impl Unit<'_> {
    fn finish(&self, score: &mut Score) {
        score.record(Rule::Complexity, self.branches + 1, self.start, self.name);
        self.depth.record(score, self.start, self.name);
        for (rule, value) in [(Rule::Length, self.lines), (Rule::Arguments, self.args)] {
            score.record(rule, value, self.start, self.name);
        }
    }
}
fn command(lang: Language) -> bool {
    matches!(
        lang,
        Shell
            | Powershell
            | CMake
            | Makefile
            | Batchfile
            | Dockerfile
            | Assembly
            | Sql
            | PlPgSql
            | Procfile
            | Mako
    )
}
pub(super) fn branch(lang: Language, w: &str) -> bool {
    match lang {
        Html | Css | Proto => false,
        Lua => matches!(w, "if" | "elseif" | "for" | "while" | "until"),
        Sql | PlPgSql => ["when", "if", "elsif", "while", "for"]
            .iter()
            .any(|s| w.eq_ignore_ascii_case(s)),
        Assembly => matches!(
            w,
            "je" | "jne" | "jz" | "jnz" | "jg" | "jl" | "beq" | "bne" | "cbz" | "cbnz"
        ),
        CMake | Batchfile | Powershell => {
            ["if", "elseif", "foreach", "for", "while", "catch", "switch"]
                .iter()
                .any(|s| w.eq_ignore_ascii_case(s))
        }
        _ => matches!(
            w,
            "if" | "elif"
                | "elsif"
                | "elseif"
                | "unless"
                | "for"
                | "foreach"
                | "while"
                | "until"
                | "except"
                | "catch"
                | "case"
                | "when"
        ),
    }
}
fn lower_keyword(w: &str, options: &[&str]) -> bool {
    options.iter().any(|s| w.eq_ignore_ascii_case(s))
}
pub(super) fn at<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |x| x.text)
}
fn ind(text: &str, start: usize) -> usize {
    let pos = text[..start].rfind('\n').map_or(0, |n| n + 1);
    text.as_bytes()[pos..start]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .map(|b| if *b == b'\t' { 8 } else { 1 })
        .sum()
}
mod nesting;
mod regions;
pub(super) use regions::{Region, RegionSource};
mod decisions;
use decisions::{Condition, LanguageBranches};

impl RegionSource<'_, '_> {
    pub(super) fn measure(&self, regions: &[Region]) -> Score {
        let RegionSource {
            lang,
            text,
            syntax,
            facts,
        } = *self;
        let has_declarations = !facts.declarations.is_empty();
        let super::PairedSyntax { tokens: t, pairs } = syntax;
        let style = nesting::Style::for_language(lang);
        let input = nesting::Input {
            html_open: false,
            braces: super::control::BraceGrammar::for_language(lang),
            lang,
            source: text,
            tokens: t,
            pairs,
        };
        let mut stream = TokenStream::new(input, style);
        let file = Unit {
            name: "<file>",
            start: 1,
            end: t.len(),
            nesting: style.start(None),
            ..Unit::default()
        };
        let mut owners = OwnerCursor::new(t.len(), file);
        let mut openings = regions
            .iter()
            .map(|r| Opening {
                start: r.start,
                end: r.end,
                owner: Unit {
                    name: r.name,
                    start: t[r.start].line,
                    body: r.body,
                    end: r.end,
                    args: r.args,
                    nesting: style.start(Some((text, t[r.start].span.start))),
                    ..Unit::default()
                },
            })
            .peekable();
        let mut bodies = Vec::new();
        while let Some((unit, run)) =
            owners.next_run(&mut openings, |u| finish(u, &mut stream.score, &mut bodies))
        {
            stream.observe(unit, run);
        }
        stream.condition.finish(&mut stream.score);
        let mut file = owners.finish(|u| finish(u, &mut stream.score, &mut bodies));
        if omit_module_length(lang, stream.brace_controls, has_declarations, file.branches) {
            file.lines = 0;
        }
        file.finish(&mut stream.score);
        duplicates::record(&mut stream.score, t, &bodies);
        stream.score
    }
}

struct TokenStream<'tokens, 'text, 'names> {
    input: nesting::Input<'tokens, 'text>,
    condition: Condition<'names>,
    branches: LanguageBranches,
    continued_until: usize,
    brace_controls: bool,
    indentation_suites: bool,
    directive: Directive,
    score: Score,
}

impl<'tokens, 'text, 'names> TokenStream<'tokens, 'text, 'names> {
    fn new(input: nesting::Input<'tokens, 'text>, style: nesting::Style) -> Self {
        let lang = input.lang;
        Self {
            directive: Directive::new(lang),
            score: Score::default(),
            condition: Condition::default(),
            branches: LanguageBranches::new(
                lang,
                super::PairedSyntax {
                    tokens: input.tokens,
                    pairs: input.pairs,
                },
            ),
            input,
            continued_until: 0,
            brace_controls: matches!(style, nesting::Style::BracedControl),
            indentation_suites: matches!(style, nesting::Style::IndentedControl),
        }
    }

    fn observe(&mut self, unit: &mut Unit<'names>, range: std::ops::Range<usize>) {
        let t = self.input.tokens;
        for i in range {
            let token = &t[i];
            let first = i == 0 || t[i - 1].line != token.line;
            self.score.lines += usize::from(first);
            if self.directive.contains(self.input.source, token, first) {
                continue;
            }
            self.observe_token(i, first, unit);
        }
    }
}
fn finish<'a>(u: Unit<'a>, score: &mut Score, bodies: &mut Vec<Body<'a>>) {
    u.finish(score);
    if u.name != "<file>" && u.lines >= Rule::Duplication.threshold() && u.body < u.end {
        bodies.push(Body {
            name: u.name,
            line: u.start,
            lines: u.lines,
            tokens: u.body..u.end,
        });
    }
}

fn omit_module_length(
    lang: Language,
    brace_controls: bool,
    has_declarations: bool,
    branches: usize,
) -> bool {
    if !brace_controls {
        return false;
    }
    let declaration_only = matches!(lang, Go | Java | Rust | C | Cpp | ObjectiveC);
    let script_module = matches!(lang, Js | Php | Swift | Kotlin | Csharp);
    (branches == 0 || declaration_only) && (!script_module || has_declarations)
}

struct Directive {
    enabled: bool,
    end: usize,
}

impl Directive {
    fn new(lang: Language) -> Self {
        Self {
            enabled: matches!(lang, C | Cpp | ObjectiveC),
            end: 0,
        }
    }

    fn contains(&mut self, text: &str, token: &Token, first: bool) -> bool {
        if !self.enabled {
            return false;
        }
        if token.span.start < self.end {
            return true;
        }
        if !first || token.text != "#" {
            return false;
        }
        self.end = token.span.start;
        for line in text[self.end..].split_inclusive('\n') {
            self.end += line.len();
            if !line.trim_end_matches(['\r', '\n']).ends_with('\\') {
                break;
            }
        }
        true
    }
}

impl nesting::Input<'_, '_> {
    fn go_continuation(&self, i: usize) -> bool {
        self.lang == Go && i >= 2 && self.double_operator(i - 2)
    }

    fn condition_end(&self, braced: bool, i: usize, end: usize) -> usize {
        if !braced {
            return 0;
        }
        self.braces
            .with_tokens(self.tokens, self.pairs)
            .condition_end(i + 1)
            .filter(|&close| close < end)
            .unwrap_or(0)
    }
}

impl<'names> TokenStream<'_, '_, 'names> {
    fn statement_start(&mut self, i: usize, first: bool) -> bool {
        let statement = first && i >= self.continued_until;
        if self.indentation_suites {
            self.observe_continuation(i);
        }
        if first && i >= self.continued_until {
            self.condition
                .newline(i, || self.input.go_continuation(i), &mut self.score);
        }
        statement
    }

    fn observe_continuation(&mut self, i: usize) {
        let t = self.input.tokens;
        let pair = self.input.pairs[i];
        if matches!(t[i].text, "(" | "[" | "{") && pair < t.len() {
            self.continued_until = self.continued_until.max(pair + 1);
        }
    }

    fn observe_token(&mut self, i: usize, first: bool, u: &mut Unit<'names>) {
        let statement = self.statement_start(i, first);
        let input = &self.input;
        let t = input.tokens;
        let token = &t[i];
        if u.last != token.line {
            u.lines += 1;
            u.last = token.line;
        }
        let w = token.text;
        let (br, expression_paths) = self.branches.observe(t, i, first, statement);
        let double_operator = input.double_operator(i);
        u.branches += expression_paths;
        u.branches += usize::from(
            double_operator
                && i >= u.body
                && (self.brace_controls || matches!(input.lang, Ruby | Shell)),
        );
        if br {
            u.branches += 1;
            let end = input.condition_end(self.brace_controls, i, u.end);
            self.condition
                .begin(token.line, u.name, end, &mut self.score);
        }
        self.condition.token(w, i, double_operator, &mut self.score);
        u.update_nesting(&mut self.input, i, first, statement);
    }
}

impl nesting::Input<'_, '_> {
    fn double_operator(&self, i: usize) -> bool {
        let t = self.tokens;
        let w = t[i].text;
        matches!(w, "&" | "|") && at(t, i + 1) == w && t[i + 1].span.start == t[i].span.end
    }
}
