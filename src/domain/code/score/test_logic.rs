//! Evidence from an executable test: a value assembled by independent branches
//! reaches an assertion, or two branches verify the same element inside a loop.
use super::{Finding, Language, Rule, Score, Token, control::Suites, scan};
use crate::domain::code::{Kind, analyze, indentation, lexer, roles, script};
use std::collections::HashMap;

pub(crate) fn candidate(path: &str) -> Option<Language> {
    let lang = Language::for_path(path)?;
    (matches!(
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
    ) && roles::on_test_path(path, lang))
    .then_some(lang)
}

pub(super) fn adjust(
    lang: Language,
    path: &str,
    text: &str,
    original: &Score,
    facts: Option<&crate::domain::code::Analysis>,
    syntax: Option<(&[Token], &[usize])>,
) -> Option<Score> {
    if !text.contains("if") {
        return None;
    }
    let alternatives = text.contains("else")
        && lang != Language::Python
        && original.findings.iter().any(|f| f.rule == Rule::Bumps);
    let oracle = text.contains("+=")
        || text.contains("++")
        || (text.contains('[') && text.contains("else") && text.contains("for"));
    if !alternatives && !oracle {
        return None;
    }
    let local_tokens;
    let local_pairs;
    let (t, pairs) = match syntax {
        Some(shared) => shared,
        None => {
            local_tokens = lexer::tokens(text, lang);
            local_pairs = script::paired_for_score(&local_tokens);
            (local_tokens.as_slice(), local_pairs.as_slice())
        }
    };
    let mut findings = original.findings.clone();
    if alternatives {
        let native;
        let facts = match facts {
            Some(f) => f,
            None => {
                native = analyze(lang, text);
                &native
            }
        };
        for r in scan::regions(lang, text, t, pairs, facts) {
            let Some(pos) = findings.iter().position(|f| {
                f.rule == Rule::Bumps && f.unit == r.name && f.line == t[r.start].line
            }) else {
                continue;
            };
            let mut suites = Suites::default();
            let (mut raw, mut corrected, mut previous, mut deep) = (0, 0, 0, false);
            for i in r.start..r.end {
                let d = suites.depth(lang, t, pairs, i, r.end);
                raw += usize::from(d >= 2 && previous < 2);
                previous = d;
                // A braced else arm continues the same decision. Its punctuation
                // cannot separate two independent stretches of nested execution.
                let bridge = d == 1
                    && ((t[i].text == "}"
                        && scan::at(t, i + 1) == "else"
                        && scan::at(t, i + 2) == "{")
                        || (t[i].text == "else"
                            && i > 0
                            && t[i - 1].text == "}"
                            && scan::at(t, i + 1) == "{"));
                if !bridge {
                    corrected += usize::from(d >= 2 && !deep);
                    deep = d >= 2;
                }
            }
            if raw == findings[pos].value && corrected < raw {
                if corrected < Rule::Bumps.threshold() {
                    findings.remove(pos);
                } else {
                    findings[pos].value = corrected;
                }
            }
        }
    }
    if oracle {
        let detected = roles::local_regions(lang, path, text, t, pairs);
        for r in detected
            .regions
            .into_iter()
            .filter(|r| r.evidence.0 == roles::Role::Test)
        {
            let start = t.partition_point(|v| v.span.start < r.start);
            let end = t.partition_point(|v| v.span.start < r.end);
            inspect(lang, text, t, pairs, start, end, &mut findings);
        }
    }
    let changed = findings.len() != original.findings.len()
        || findings
            .iter()
            .zip(&original.findings)
            .any(|(a, b)| a.rule != b.rule || a.value != b.value || a.line != b.line);
    changed.then_some(Score {
        lines: original.lines,
        findings,
    })
}

fn assertion(w: &str) -> bool {
    w == "assert"
        || w == "expect"
        || w == "assert_eq"
        || w == "assert_ne"
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

fn asserted_element<'a>(
    t: &[Token<'a>],
    p: &[usize],
    start: usize,
    end: usize,
    index: &str,
) -> Vec<&'a str> {
    let mut names = Vec::new();
    for i in start..end {
        if !assertion(t[i].text) || scan::at(t, i + 1) != "(" {
            continue;
        }
        for w in t[i + 2..p[i + 1].min(end).max(i + 2)].windows(4) {
            if w[0].kind == Kind::Word && w[1].text == "[" && w[2].text == index && w[3].text == "]"
            {
                names.push(w[0].text);
            }
        }
    }
    names
}

fn inspect(
    lang: Language,
    text: &str,
    t: &[Token],
    p: &[usize],
    start: usize,
    end: usize,
    out: &mut Vec<Finding>,
) {
    let mut vars = HashMap::<&str, Writes>::new();
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut loops = Vec::new();
    let mut scopes = Vec::new();
    let mut conditions = Vec::<(usize, usize)>::new();
    let mut previous_line = 0;
    for i in start..end {
        let w = t[i].text;
        let next = scan::at(t, i + 1);
        scopes.retain(|&stop| stop > i);
        if w == "{" && p[i] < end {
            scopes.push(p[i]);
        }
        blocks.retain(|&(_, stop)| i < stop);
        loops.retain(|&stop| i < stop);
        if lang == Language::Python && previous_line != t[i].line {
            let ind = indentation(text, t[i].span.start);
            while conditions.last().is_some_and(|&(base, _)| ind <= base) {
                conditions.pop();
            }
        }
        let decision = if lang == Language::Python {
            conditions.last().map(|&(_, n)| n)
        } else {
            blocks.last().map(|&(n, _)| n)
        };
        if w == "if" || w == "elif" {
            if lang == Language::Python {
                if previous_line != t[i].line {
                    conditions.push((indentation(text, t[i].span.start), i));
                }
            } else if let Some(open) = body(t, p, i, end) {
                blocks.push((i, p[open]));
                let close = p[open];
                if !loops.is_empty()
                    && scan::at(t, close + 1) == "else"
                    && scan::at(t, close + 2) == "{"
                    && p[close + 2] < end
                {
                    for sub in t[i + 1..open]
                        .windows(4)
                        .filter(|s| s[1].text == "[" && s[3].text == "]")
                    {
                        let left = asserted_element(t, p, open + 1, close, sub[2].text);
                        let right = asserted_element(t, p, close + 3, p[close + 2], sub[2].text);
                        if left.iter().any(|n| *n != sub[0].text && right.contains(n)) {
                            record(
                                out,
                                t[i].line,
                                "indexed assertions in alternative loop arms",
                                2,
                            );
                            break;
                        }
                    }
                }
                // In Go a failed comparison is often the assertion itself.
                if lang == Language::Go
                    && open + 4 < p[open]
                    && t[open + 2].text == "."
                    && matches!(t[open + 3].text, "Error" | "Errorf" | "Fatal" | "Fatalf")
                    && t[open + 4].text == "("
                {
                    comparison(&mut vars, t, p, i + 1, open, out);
                }
            }
        }
        if matches!(w, "for" | "while")
            && let Some(open) = body(t, p, i, end)
        {
            loops.push(p[open]);
        }
        previous_line = t[i].line;
        if t[i].kind == Kind::Word
            && !(i > 0 && t[i - 1].line == t[i].line && matches!(t[i - 1].text, "." | ">" | ":"))
        {
            let after = scan::at(t, i + 2);
            if (next == "=" && !matches!(after, "=" | ">")) || (next == ":" && after == "=") {
                vars.insert(
                    w,
                    Writes {
                        end: scopes.last().copied().unwrap_or(end),
                        first: None,
                        decisions: Vec::new(),
                    },
                );
            } else if next == "+"
                && matches!(after, "+" | "=")
                && let Some(branch) = decision
                && let Some(v) = vars.get_mut(w)
                && i < v.end
                && !v.decisions.contains(&branch)
            {
                v.decisions.push(branch);
                v.first.get_or_insert(i);
            }
        }
        if assertion(w) {
            let open = i + 1 + usize::from(next == "!");
            if lang == Language::Python && w == "assert" {
                let stop = i + t[i..end].partition_point(|tok| tok.line == t[i].line);
                let comma = delimiter(t, p, i + 1, stop, &[","]).unwrap_or(stop);
                comparison(&mut vars, t, p, i + 1, comma, out);
            } else if scan::at(t, open) == "(" && p[open] < end {
                let close = p[open];
                if w == "expect"
                    && scan::at(t, close + 1) == "."
                    && matches!(scan::at(t, close + 2), "toBe" | "toEqual" | "toStrictEqual")
                    && scan::at(t, close + 3) == "("
                    && p[close + 3] < end
                {
                    observe(&mut vars, t, open + 1..close, close + 4..p[close + 3], out);
                } else if matches!(
                    w,
                    "assert_eq"
                        | "assert_ne"
                        | "ASSERT_EQ"
                        | "ASSERT_NE"
                        | "EXPECT_EQ"
                        | "EXPECT_NE"
                        | "assertEquals"
                        | "assertNotEquals"
                        | "XCTAssertEqual"
                        | "XCTAssertNotEqual"
                ) && let Some(comma) = delimiter(t, p, open + 1, close, &[","])
                {
                    let stop = delimiter(t, p, comma + 1, close, &[","]).unwrap_or(close);
                    observe(&mut vars, t, open + 1..comma, comma + 1..stop, out);
                } else {
                    let stop = delimiter(t, p, open + 1, close, &[","]).unwrap_or(close);
                    comparison(&mut vars, t, p, open + 1, stop, out);
                }
            }
        }
    }
}

fn delimiter(
    t: &[Token],
    pairs: &[usize],
    mut i: usize,
    end: usize,
    words: &[&str],
) -> Option<usize> {
    while i < end {
        if words.contains(&t[i].text) {
            return Some(i);
        }
        if matches!(t[i].text, "(" | "[" | "{") && pairs[i] > i && pairs[i] < end {
            i = pairs[i];
        }
        i += 1;
    }
    None
}

fn comparison(
    vars: &mut HashMap<&str, Writes>,
    t: &[Token],
    pairs: &[usize],
    start: usize,
    end: usize,
    out: &mut Vec<Finding>,
) {
    if let Some(op) = delimiter(t, pairs, start, end, &["=", "!"])
        && op + 1 < end
        && t[op + 1].text == "="
    {
        observe(vars, t, start..op, op + 2..end, out);
    }
}

fn observe(
    vars: &mut HashMap<&str, Writes>,
    t: &[Token],
    a: std::ops::Range<usize>,
    b: std::ops::Range<usize>,
    out: &mut Vec<Finding>,
) {
    for (reads, other) in [(&a, &b), (&b, &a)] {
        for tok in &t[reads.clone()] {
            if let Some(v) = vars.get_mut(tok.text)
                && reads.start < v.end
                && v.decisions.len() >= 2
                && t[other.clone()].iter().any(|r| {
                    r.text != tok.text
                        && r.kind == Kind::Word
                        && r.text.starts_with(|c: char| c.is_alphabetic() || c == '_')
                        && !matches!(
                            r.text,
                            "true" | "false" | "True" | "False" | "nil" | "None" | "null"
                        )
                })
                && let Some(first) = v.first.take()
            {
                record(out, t[first].line, tok.text, v.decisions.len());
                v.decisions.clear();
            }
        }
    }
}
fn record(out: &mut Vec<Finding>, line: usize, unit: &str, value: usize) {
    if !out
        .iter()
        .any(|f| f.rule == Rule::Oracle && f.line == line && f.unit == unit)
    {
        out.push(Finding {
            rule: Rule::Oracle,
            line,
            unit: unit.to_string(),
            value,
        });
    }
}
