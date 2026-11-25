//! Find the executable body of each suite, case, and lifecycle hook.
use super::{Language, Token};

pub(super) fn bodies<'a>(
    lang: Language,
    t: &[Token<'a>],
    pairs: &[usize],
    ruby: &[usize],
) -> Vec<(usize, usize, usize, &'a str)> {
    let mut out = Vec::new();
    for (i, token) in t.iter().enumerate() {
        let name = token.text;
        let prev = i.checked_sub(1).map_or("", |j| t[j].text);
        if matches!(prev, "." | "def" | "function" | "fun" | ">") {
            continue;
        }
        let next = i + 1;
        let at = |j: usize| t.get(j).map_or("", |v| v.text);
        let body = match lang {
            Language::Ruby
                if matches!(
                    name,
                    "it" | "test"
                        | "specify"
                        | "example"
                        | "describe"
                        | "context"
                        | "feature"
                        | "scenario"
                        | "before"
                        | "after"
                        | "around"
                        | "let"
                        | "let!"
                        | "subject"
                ) =>
            {
                let mut j = next;
                while j < t.len()
                    && t[j].line == token.line
                    && !matches!(at(j), "do" | "{" | ";" | "end")
                {
                    if matches!(at(j), "(" | "[") && pairs[j] < t.len() {
                        j = pairs[j];
                    }
                    j += 1;
                }
                if at(j) == "do" && ruby[j] < t.len() {
                    Some((j, ruby[j]))
                } else if at(j) == "{" && pairs[j] < t.len() {
                    Some((j, pairs[j]))
                } else {
                    None
                }
            }
            Language::Dart | Language::Php
                if matches!(
                    name,
                    "group"
                        | "describe"
                        | "context"
                        | "test"
                        | "testWidgets"
                        | "it"
                        | "beforeEach"
                        | "afterEach"
                        | "beforeAll"
                        | "afterAll"
                        | "setUp"
                        | "tearDown"
                        | "setUpAll"
                        | "tearDownAll"
                ) && at(next) == "("
                    && pairs[next] < t.len() =>
            {
                let mut j = next + 1;
                let mut body = None;
                while j < pairs[next] {
                    if at(j) == "(" && pairs[j] < pairs[next] {
                        let mut open = pairs[j] + 1;
                        if matches!(at(open), "async" | "sync") {
                            open += 1;
                        }
                        if at(open) == "{" && pairs[open] < pairs[next] {
                            body = Some((open, pairs[open]));
                            break;
                        }
                    }
                    if matches!(at(j), "(" | "[" | "{") && pairs[j] < pairs[next] {
                        j = pairs[j];
                    }
                    j += 1;
                }
                body
            }
            Language::Kotlin => {
                let string_case = name.starts_with('"');
                let j = if string_case && at(next) == "-" {
                    next + 1
                } else if at(next) == "(" && pairs[next] < t.len() {
                    pairs[next] + 1
                } else {
                    next
                };
                if (string_case
                    || matches!(
                        name,
                        "describe"
                            | "context"
                            | "it"
                            | "test"
                            | "given"
                            | "Given"
                            | "When"
                            | "Then"
                            | "And"
                            | "then"
                            | "should"
                            | "feature"
                            | "scenario"
                            | "beforeTest"
                            | "afterTest"
                            | "beforeEach"
                            | "afterEach"
                            | "beforeContainer"
                            | "afterContainer"
                            | "beforeSpec"
                            | "afterSpec"
                            | "beforeAny"
                            | "afterAny"
                    ))
                    && at(j) == "{"
                    && pairs[j] < t.len()
                {
                    Some((j, pairs[j]))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((open, close)) = body {
            out.push((open, close + 1, token.line, name));
        }
    }
    out
}

// Pair Ruby block keywords in token order so close delimiters work on a
// single line and inside an enclosing branch regardless of indentation.
pub(super) fn ruby_ends(t: &[Token]) -> Vec<usize> {
    let mut ends = vec![usize::MAX; t.len()];
    let mut stack = Vec::new();
    for (i, token) in t.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &t[j]);
        let statement = prev.is_none_or(|p| {
            p.line < token.line || matches!(p.text, ";" | "=" | "(" | "then" | "else" | "do")
        });
        match token.text {
            "if" | "unless" | "while" | "until" if statement => stack.push(i),
            "do" if stack.last().is_some_and(|&j| {
                t[j].line == token.line && matches!(t[j].text, "while" | "until" | "for")
            }) =>
            {
                ()
            }
            "do" | "class" | "module" | "def" | "case" | "begin" | "for" => stack.push(i),
            "end" => {
                if let Some(j) = stack.pop() {
                    ends[j] = i;
                }
            }
            _ => (),
        }
    }
    ends
}
