//! Track control suites separately from braces belonging to data and declarations.
use super::scan::at;
use super::{Language, Token};
use Language::*;

#[derive(Default)]
pub(super) struct Suites {
    ends: Vec<usize>,
    pending: bool,
    statement_end: usize,
}

impl Suites {
    pub(super) fn depth(
        &mut self,
        lang: Language,
        t: &[Token],
        pairs: &[usize],
        i: usize,
        unit_end: usize,
    ) -> usize {
        let w = t[i].text;
        // Only control suites add control depth. Object literals, classes,
        // composite literals and ordinary lexical scopes also use braces.
        while self.ends.last().is_some_and(|&end| end <= i) {
            self.ends.pop();
        }
        // The trailing condition in a braced do/while closes the same loop.
        // Opening a second suite here invents a second nested region.
        if w == "while" && i > 0 && t[i - 1].text == "}" {
            let open = pairs[i - 1];
            if open > 0 && open < i && t[open - 1].text == "do" {
                return self.ends.len();
            }
        }
        if matches!(
            w,
            "if" | "else"
                | "for"
                | "foreach"
                | "while"
                | "do"
                | "switch"
                | "match"
                | "loop"
                | "catch"
                | "try"
                | "finally"
                | "when"
                | "select"
        ) {
            let next = i + 1;
            if w == "else" && at(t, next) == "if" {
                self.pending = false;
            } else if at(t, next) == "("
                && pairs[next] < t.len()
                && !matches!(lang, Go | Rust | Swift)
            {
                let body = pairs[next] + 1;
                let mut end = if body < self.statement_end && self.statement_end < unit_end {
                    self.statement_end
                } else {
                    body
                };
                while end < unit_end && !matches!(t[end].text, ";" | "{" | "}") {
                    if end > body && matches!(lang, Js | Kotlin) && t[end].line > t[end - 1].line {
                        break;
                    }
                    if matches!(t[end].text, "(" | "[") && pairs[end] > end && pairs[end] < unit_end
                    {
                        end = pairs[end];
                    }
                    end += 1;
                }
                self.statement_end = end;
                if at(t, end) == "{" {
                    end = pairs[end].min(unit_end);
                }
                if end > i {
                    self.ends.push(end);
                }
                self.pending = false;
            } else {
                self.pending = true;
            }
        } else if w == "{" && self.pending {
            if pairs[i] < t.len() {
                self.ends.push(pairs[i]);
            }
            self.pending = false;
        } else if w == "}" || (w == ";" && lang != Go) {
            self.pending = false;
        }
        self.ends.len()
    }
}
