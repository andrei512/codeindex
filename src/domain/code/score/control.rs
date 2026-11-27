//! Track control suites separately from braces belonging to data and declarations.
use super::scan::at;
use super::{Language, PairedSyntax, Token};
use Language::*;

/// Both ordinary declarations and framework-owned blocks count a new bump
/// only on entry into a distinct deeply nested region.
#[derive(Default)]
pub(super) struct DepthSummary {
    peak: usize,
    bumps: usize,
    deep: bool,
}

impl DepthSummary {
    pub(super) fn observe(&mut self, depth: usize) {
        self.peak = self.peak.max(depth);
        self.bumps += usize::from(depth >= 2 && !self.deep);
        self.deep = depth >= 2;
    }

    pub(super) fn record(&self, score: &mut super::Score, line: usize, name: &str) {
        score.record(super::Rule::Bumps, self.bumps, line, name);
        score.record(super::Rule::Nesting, self.peak, line, name);
    }
}

#[derive(Clone, Copy)]
pub(super) struct BraceGrammar {
    paren_condition: bool,
    newline_ends: bool,
    semicolon_ends: bool,
}
impl BraceGrammar {
    pub(super) fn for_language(lang: Language) -> Self {
        Self {
            paren_condition: !matches!(lang, Go | Rust | Swift),
            newline_ends: matches!(lang, Js | Kotlin),
            semicolon_ends: lang != Go,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ControlInput<'a, 'text> {
    grammar: BraceGrammar,
    syntax: PairedSyntax<'a, 'text>,
}

impl ControlInput<'_, '_> {
    pub(super) fn condition_end(self, open: usize) -> Option<usize> {
        if !self.grammar.paren_condition {
            return None;
        }
        self.syntax.close_paren(open)
    }
}

impl BraceGrammar {
    pub(super) fn with_tokens<'a, 'text>(
        self,
        tokens: &'a [Token<'text>],
        pairs: &'a [usize],
    ) -> ControlInput<'a, 'text> {
        ControlInput {
            grammar: self,
            syntax: PairedSyntax { tokens, pairs },
        }
    }
}

pub(super) fn expire(ends: &mut Vec<usize>, token: usize) {
    while ends.last().is_some_and(|&end| end <= token) {
        ends.pop();
    }
}

#[derive(Default)]
pub(super) struct Suites {
    ends: Vec<usize>,
    pending: bool,
    statement_end: usize,
}

impl Suites {
    pub(super) fn depth(&mut self, input: ControlInput, i: usize, unit_end: usize) -> usize {
        let PairedSyntax { tokens: t, pairs } = input.syntax;
        let w = t[i].text;
        // Only control suites add control depth. Object literals, classes,
        // composite literals and ordinary lexical scopes also use braces.
        expire(&mut self.ends, i);
        match w {
            "if" | "else" | "for" | "foreach" | "while" | "do" | "switch" | "match" | "loop"
            | "catch" | "try" | "finally" | "when" | "select" => {
                self.control_header(input, i, unit_end);
            }
            "{" => self.enter_pending_block(pairs[i], t.len()),
            "}" => self.pending = false,
            ";" => self.pending &= !input.grammar.semicolon_ends,
            _ => {}
        }
        self.ends.len()
    }

    fn enter_pending_block(&mut self, close: usize, len: usize) {
        if !self.pending {
            return;
        }
        self.ends.extend((close < len).then_some(close));
        self.pending = false;
    }

    fn control_header(&mut self, input: ControlInput, i: usize, unit_end: usize) {
        let PairedSyntax { tokens, pairs } = input.syntax;
        if tokens[i].text == "while" && closes_do_loop(tokens, pairs, i) {
            return;
        }
        if let Some(close) = input.condition_end(i + 1) {
            self.enter_statement(input, close + 1, unit_end);
            self.pending = false;
        } else {
            let t = input.syntax.tokens;
            self.pending = !(t[i].text == "else" && at(t, i + 1) == "if");
        }
    }

    fn enter_statement(&mut self, input: ControlInput, body: usize, unit_end: usize) {
        let (t, pairs) = (input.syntax.tokens, input.syntax.pairs);
        let mut end = if (body + 1..unit_end).contains(&self.statement_end) {
            self.statement_end
        } else {
            body
        };
        while end < unit_end && !matches!(t[end].text, ";" | "{" | "}") {
            if input.grammar.newline_ends && statement_newline(t, body, end) {
                break;
            }
            end = input.syntax.next_top_level(end, unit_end, &["(", "["]);
        }
        self.statement_end = end;
        if at(t, end) == "{" {
            end = pairs[end].min(unit_end);
        }
        self.ends.push(end);
    }
}

fn statement_newline(tokens: &[Token], body: usize, end: usize) -> bool {
    end > body && tokens[end].line > tokens[end - 1].line
}

/// A trailing while belongs to the do suite that just closed.
fn closes_do_loop(tokens: &[Token], pairs: &[usize], i: usize) -> bool {
    if i == 0 || tokens[i - 1].text != "}" {
        return false;
    }
    let open = pairs[i - 1];
    open > 0 && open < i && tokens[open - 1].text == "do"
}
