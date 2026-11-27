//! Evidence from an executable test: a value assembled by independent branches
//! reaches an assertion, or two branches verify the same element inside a loop.
use super::{
    Finding, Language, PairedSyntax, Rule, Score, TestSyntax, Token, control::Suites, scan,
};
use crate::domain::code::{Kind, indentation, roles};
use std::{borrow::Cow, collections::HashMap};

mod alternatives;
mod assertions;
use assertions::Expressions;

pub(super) fn supports(lang: Language) -> bool {
    matches!(
        lang,
        Language::Python
            | Language::Js
            | Language::Go
            | Language::Cpp
            | Language::C
            | Language::Java
            | Language::Swift
            | Language::Rust
            | Language::Csharp
            | Language::Kotlin
            | Language::Dart
    )
}

struct Adjustment {
    alternatives: bool,
    oracle: bool,
}

impl Adjustment {
    fn plan(syntax: &TestSyntax, findings: &[Finding]) -> Option<Self> {
        let (lang, text) = (syntax.lang, syntax.text);
        if !text.contains("if") {
            return None;
        }
        let has_else = text.contains("else");
        let alternatives =
            has_else && lang != Language::Python && findings.iter().any(|f| f.rule == Rule::Bumps);
        let oracle = Self::has_oracle_syntax(text, has_else);
        (alternatives || oracle).then_some(Self {
            alternatives,
            oracle,
        })
    }

    fn has_oracle_syntax(text: &str, has_else: bool) -> bool {
        text.contains("+=")
            || text.contains("++")
            || (has_else && text.contains('[') && text.contains("for"))
    }

    fn findings<'a>(&self, syntax: &TestSyntax, original: &'a [Finding]) -> Cow<'a, [Finding]> {
        if self.alternatives {
            alternatives::correct(syntax, original)
        } else {
            Cow::Borrowed(original)
        }
    }
}

pub(super) fn adjust(
    role: &roles::RolePath,
    original: &Score,
    syntax: &TestSyntax,
) -> Option<Score> {
    let plan = Adjustment::plan(syntax, &original.findings)?;
    let mut findings = plan.findings(syntax, &original.findings);
    if plan.oracle {
        inspect_tests(role, syntax, &mut findings);
    }
    match findings {
        Cow::Owned(findings) => Some(Score {
            lines: original.lines,
            findings,
        }),
        Cow::Borrowed(_) => None,
    }
}

fn inspect_tests(role: &roles::RolePath, syntax: &TestSyntax, out: &mut Cow<'_, [Finding]>) {
    let input = syntax.paired();
    let t = input.tokens;
    let detected = role.local_regions(syntax.text, input);
    for (region, children) in detected.tests_with_children() {
        let nested = if syntax.lang == Language::Python {
            children
        } else {
            &[]
        };
        let end = t.partition_point(|token| token.span.start < region.end);
        let inspected = owned_tokens(&t[..end], region.start, nested);
        let mut inspection = Inspection::new(syntax, input, end);
        inspected.for_each(|i| inspection.observe(i, out));
    }
}

// Exclude nested Python declarations from the writes and assertions of their parent.
fn owned_tokens<'a>(
    tokens: &'a [Token<'_>],
    start: usize,
    children: &'a [roles::Region],
) -> impl Iterator<Item = usize> + 'a {
    let mut cursor = tokens.partition_point(|token| token.span.start < start);
    children
        .iter()
        .map(Some)
        .chain(std::iter::once(None))
        .flat_map(move |child| match child {
            Some(child) => {
                let next =
                    cursor + tokens[cursor..].partition_point(|t| t.span.start < child.start);
                let owned = cursor..next;
                cursor = next + tokens[next..].partition_point(|t| t.span.start < child.end);
                owned
            }
            None => cursor..tokens.len(),
        })
}

fn assertion(w: &str) -> bool {
    matches!(w, "assert" | "expect" | "assert_eq" | "assert_ne")
        || w.starts_with("ASSERT_")
        || w.starts_with("EXPECT_")
        || w.starts_with("XCTAssert")
        || w.strip_prefix("assert")
            .is_some_and(|suffix| suffix.starts_with(char::is_uppercase))
}

struct Writes {
    end: usize,
    first: Option<usize>,
    decisions: Vec<usize>,
}

fn body(t: &[Token], pairs: &[usize], i: usize, end: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < end {
        match t[j].text {
            "{" => return (pairs[j] < end).then_some(j),
            ";" | "}" => return None,
            "(" | "[" if pairs[j] < end && pairs[j] > j => j = pairs[j],
            _ => (),
        }
        j += 1;
    }
    None
}

/// Each active decision has one boundary, measured in indentation columns
/// for Python and in closing token positions for braced languages.
enum DecisionScopes {
    Indented(Vec<(usize, usize)>),
    Braced {
        decisions: Vec<(usize, usize)>,
        loops: Vec<usize>,
    },
}
impl DecisionScopes {
    fn new(lang: Language) -> Self {
        if lang == Language::Python {
            Self::Indented(Vec::new())
        } else {
            Self::Braced {
                decisions: Vec::new(),
                loops: Vec::new(),
            }
        }
    }

    fn indentation(&self, token: &Token, previous_line: usize, source: &str) -> Option<usize> {
        let Self::Indented(stack) = self else {
            return None;
        };
        let follows_decision = !stack.is_empty() || matches!(token.text, "if" | "elif");
        if previous_line == token.line || !follows_decision {
            return None;
        }
        Some(indentation(source, token.span.start))
    }

    fn enclosing(&mut self, i: usize, indent: Option<usize>) -> Option<usize> {
        fn active(
            stack: &mut Vec<(usize, usize)>,
            closed: impl Fn(usize) -> bool,
        ) -> Option<usize> {
            while stack.last().is_some_and(|&(boundary, _)| closed(boundary)) {
                stack.pop();
            }
            stack.last().map(|&(_, decision)| decision)
        }
        match self {
            Self::Indented(stack) => {
                active(stack, |base| indent.is_some_and(|indent| indent <= base))
            }
            Self::Braced { decisions, .. } => active(decisions, |end| end <= i),
        }
    }

    /// Update the control stack once per header. Only braced conditionals have
    /// a token range to inspect for alternative loop arms and Go assertions.
    fn header(
        &mut self,
        syntax: PairedSyntax,
        i: usize,
        end: usize,
        indent: Option<usize>,
    ) -> Option<DecisionBody> {
        let PairedSyntax { tokens, pairs } = syntax;
        let conditional = match tokens[i].text {
            "if" | "elif" => true,
            "for" | "while" => false,
            _ => return None,
        };
        match self {
            Self::Indented(stack) => {
                if let Some(base) = indent.filter(|_| conditional) {
                    stack.push((base, i));
                }
                None
            }
            Self::Braced { decisions, loops } => {
                let open = body(tokens, pairs, i, end)?;
                super::control::expire(loops, i);
                let close = pairs[open];
                update_braced_control(decisions, loops, (i, open, close), conditional)
            }
        }
    }
}

fn update_braced_control(
    decisions: &mut Vec<(usize, usize)>,
    loops: &mut Vec<usize>,
    (i, open, close): (usize, usize, usize),
    conditional: bool,
) -> Option<DecisionBody> {
    if !conditional {
        loops.push(close);
        return None;
    }
    decisions.push((close, i));
    Some(DecisionBody {
        open,
        close,
        in_loop: !loops.is_empty(),
    })
}

// In Go a failed comparison followed by t.Error/Fatal is an assertion.
fn go_failure(t: &[Token], open: usize, close: usize) -> bool {
    open + 4 < close
        && t[open + 2].text == "."
        && matches!(t[open + 3].text, "Error" | "Errorf" | "Fatal" | "Fatalf")
        && t[open + 4].text == "("
}

struct DecisionBody {
    open: usize,
    close: usize,
    in_loop: bool,
}

struct Inspection<'a, 's> {
    input: PairedSyntax<'a, 's>,
    text: &'s str,
    lang: Language,
    end: usize,
    expressions: Expressions<'a, 's>,
    variables: Variables<'s>,
    decisions: DecisionScopes,
    scopes: Vec<usize>,
    previous_line: usize,
}

impl<'a, 's> Inspection<'a, 's> {
    fn new(syntax: &TestSyntax<'_, 's>, input: PairedSyntax<'a, 's>, end: usize) -> Self {
        Self {
            input,
            text: syntax.text,
            lang: syntax.lang,
            end,
            expressions: Expressions::new(input, end),
            variables: Variables::default(),
            decisions: DecisionScopes::new(syntax.lang),
            scopes: Vec::new(),
            previous_line: 0,
        }
    }

    fn observe(&mut self, i: usize, out: &mut Cow<'_, [Finding]>) {
        let token = &self.input.tokens[i];
        self.observe_scope(i, token.text);
        let indent = self
            .decisions
            .indentation(token, self.previous_line, self.text);
        let decision = self.decisions.enclosing(i, indent);
        if let Some(body) = self.decisions.header(self.input, i, self.end, indent) {
            self.observe_header(i, body, out);
        }
        self.previous_line = token.line;
        self.variables
            .observe_write(self.input.tokens, i, decision, || {
                self.scopes.last().copied().unwrap_or(self.end)
            });
        if assertion(token.text) {
            self.variables.assertion(
                self.input.tokens,
                || self.expressions.operands(self.lang, i),
                out,
            );
        }
    }

    fn observe_scope(&mut self, i: usize, text: &str) {
        super::control::expire(&mut self.scopes, i);
        if text == "{" && self.input.pairs[i] < self.end {
            self.scopes.push(self.input.pairs[i]);
        }
    }

    fn observe_header(&mut self, i: usize, body: DecisionBody, out: &mut Cow<'_, [Finding]>) {
        let DecisionBody {
            open,
            close,
            in_loop,
        } = body;
        let tokens = self.input.tokens;
        if in_loop && self.expressions.alternative_loop_assertions(i..open) {
            record(
                out,
                tokens[i].line,
                "indexed assertions in alternative loop arms",
                2,
            );
        }
        if self.lang == Language::Go && go_failure(tokens, open, close) {
            self.variables
                .assertion(tokens, || self.expressions.comparison(i + 1..open), out);
        }
    }
}

#[derive(Default)]
struct Variables<'a>(HashMap<&'a str, Writes>);

impl Writes {
    fn increment(&mut self, i: usize, branch: usize) {
        if i < self.end && !self.decisions.contains(&branch) {
            self.decisions.push(branch);
            self.first.get_or_insert(i);
        }
    }

    fn observed(
        &mut self,
        at: usize,
        other_value: impl FnOnce() -> bool,
    ) -> Option<(usize, usize)> {
        let count = self.decisions.len();
        if at >= self.end || count < Rule::Oracle.threshold() || !other_value() {
            return None;
        }
        let first = self.first.take()?;
        self.decisions.clear();
        Some((first, count))
    }
}

enum Write {
    Reset,
    Increment,
}

impl Write {
    fn at(t: &[Token], i: usize) -> Option<Self> {
        if t[i].kind != Kind::Word {
            return None;
        }
        let op = match (scan::at(t, i + 1), scan::at(t, i + 2)) {
            ("=", "=" | ">") => return None,
            ("=", _) | (":", "=") => Self::Reset,
            ("+", "+" | "=") => Self::Increment,
            _ => return None,
        };
        let token = &t[i];
        let member =
            i > 0 && t[i - 1].line == token.line && matches!(t[i - 1].text, "." | ">" | ":");
        (!member).then_some(op)
    }
}

impl<'a> Variables<'a> {
    fn observe_write(
        &mut self,
        tokens: &[Token<'a>],
        i: usize,
        branch: Option<usize>,
        scope_end: impl FnOnce() -> usize,
    ) {
        let name = tokens[i].text;
        match Write::at(tokens, i) {
            Some(Write::Reset) => {
                self.0.insert(
                    name,
                    Writes {
                        end: scope_end(),
                        first: None,
                        decisions: Vec::new(),
                    },
                );
            }
            Some(Write::Increment) => {
                if let (Some(branch), Some(writes)) = (branch, self.0.get_mut(name)) {
                    writes.increment(i, branch);
                }
            }
            None => {}
        }
    }

    fn assertion(
        &mut self,
        t: &[Token],
        operands: impl FnOnce() -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)>,
        out: &mut Cow<'_, [Finding]>,
    ) {
        if self.0.is_empty() {
            return;
        }
        let Some((a, b)) = operands() else { return };
        for (reads, other) in [(&a, &b), (&b, &a)] {
            for tok in &t[reads.clone()] {
                let Some(v) = self.0.get_mut(tok.text) else {
                    continue;
                };
                if let Some((first, count)) =
                    v.observed(reads.start, || has_other_value(&t[other.clone()], tok.text))
                {
                    record(out, t[first].line, tok.text, count);
                }
            }
        }
    }
}
fn has_other_value(tokens: &[Token], name: &str) -> bool {
    tokens.iter().any(|r| {
        r.text != name
            && r.kind == Kind::Word
            && r.text.starts_with(|c: char| c.is_alphabetic() || c == '_')
            && !matches!(
                r.text,
                "true" | "false" | "True" | "False" | "nil" | "None" | "null"
            )
    })
}

fn record(out: &mut Cow<'_, [Finding]>, line: usize, unit: &str, value: usize) {
    if value >= Rule::Oracle.threshold()
        && !out
            .iter()
            .any(|f| f.rule == Rule::Oracle && f.line == line && f.unit == unit)
    {
        out.to_mut().push(Finding {
            rule: Rule::Oracle,
            line,
            unit: unit.to_string(),
            value,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_nested_python_helper_owns_its_local_writes() {
        let cases = [
            (
                "    value = 2\n    def dormant():\n        value = 0\n        if a:\n            value += 1\n        if b:\n            value += 1\n",
                false,
            ),
            (
                "    value = 0\n    if a:\n        value += 1\n    def dormant():\n        value = 4\n    if b:\n        value += 1\n",
                true,
            ),
        ];
        for (body, expected) in cases {
            let text = format!("def test_value():\n{body}    assert actual == value\n");
            let measured = crate::domain::code::score_file(
                Language::Python,
                &crate::domain::code::Document {
                    path: "test_logic.py",
                    text: &text,
                },
                None,
                true,
            );
            let reported = measured
                .selected()
                .unwrap()
                .findings
                .iter()
                .any(|f| f.rule == Rule::Oracle);
            assert_eq!(reported, expected);
        }
    }
}
