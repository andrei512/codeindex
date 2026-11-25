//! Divide an existing length finding between the registered cases and their owning unit.
use super::{Finding, Language, Rule, Score, scan};
use crate::domain::code::{analyze, lexer, roles, script};

pub(crate) fn candidate(path: &str, score: &Score) -> Option<Language> {
    let lang = Language::for_path(path)?;
    if !matches!(
        lang,
        Language::Dart | Language::Powershell | Language::Ruby | Language::Sql | Language::PlPgSql
    ) {
        return None;
    }
    if matches!(lang, Language::Ruby | Language::Powershell) && !roles::on_test_path(path, lang) {
        return None;
    }
    score
        .findings
        .iter()
        .any(|f| {
            f.rule == Rule::Length
                && (f.unit == "<file>" || lang == Language::Dart && f.unit == "main")
        })
        .then_some(lang)
}

#[derive(Default)]
struct Lines {
    last: usize,
    count: usize,
}
impl Lines {
    fn add(&mut self, line: usize) {
        if self.last != line {
            self.count += 1;
            self.last = line;
        }
    }
}

pub(crate) fn adjust(lang: Language, path: &str, text: &str, score: &Score) -> Option<Score> {
    let tokens = lexer::tokens(text, lang);
    let pairs = script::paired_for_score(&tokens);
    let cases = roles::suite_regions(lang, path, text, &tokens, &pairs);
    if cases.is_empty() || cases.windows(2).any(|w| w[0].end > w[1].start) {
        return None;
    }
    let facts = analyze(lang, text);
    let units = scan::regions(lang, text, &tokens, &pairs, &facts);
    let units = units.iter().peekable();
    let mut stack = Vec::new();
    let mut output = Vec::new();
    let mut changed = false;
    // Only the synthetic file unit or Dart's top-level suite entry can own this warning.
    for finding in &score.findings {
        if finding.rule != Rule::Length
            || !(finding.unit == "<file>" || lang == Language::Dart && finding.unit == "main")
        {
            output.push(finding.clone());
            continue;
        }
        let mut case_lines: Vec<Lines> = cases.iter().map(|_| Lines::default()).collect();
        let mut total = Lines::default();
        let mut residual = Lines::default();
        let mut cursor = 0;
        let mut iter = units.clone();
        stack.clear();
        for (i, t) in tokens.iter().enumerate() {
            while stack.last().is_some_and(|u: &&scan::Region| u.end <= i) {
                stack.pop();
            }
            while iter.peek().is_some_and(|u| u.start == i) {
                stack.push(iter.next().unwrap());
            }
            let (name, start) = stack
                .last()
                .map_or(("<file>", 1), |u| (u.name, tokens[u.start].line));
            if name != finding.unit || start != finding.line {
                continue;
            }
            // Preserve the baseline line budget if two regions share a line.
            if total.last == t.line {
                continue;
            }
            total.add(t.line);
            while cursor < cases.len() && cases[cursor].end <= t.span.start {
                // A registration's trailing semicolon belongs to the call.
                if t.text == ";" && t.span.start == cases[cursor].end {
                    break;
                }
                cursor += 1;
            }
            if cases.get(cursor).is_some_and(|r| r.start <= t.span.start) {
                case_lines[cursor].add(t.line);
            } else {
                residual.add(t.line);
            }
        }
        // If the snapshot used a different language-specific tokenization, preserve its measurement.
        if total.count != finding.value || case_lines.iter().all(|u| u.count == 0) {
            output.push(finding.clone());
            continue;
        }
        changed = true;
        if residual.count >= Rule::Length.threshold() {
            output.push(Finding {
                value: residual.count,
                ..finding.clone()
            });
        }
        for (region, part) in cases.iter().zip(case_lines) {
            if part.count >= Rule::Length.threshold() {
                let line = text[region.start..region.end]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim();
                output.push(Finding {
                    rule: Rule::Length,
                    line: region.first,
                    unit: line.chars().take(100).collect(),
                    value: part.count,
                });
            }
        }
    }
    changed.then_some(Score {
        lines: score.lines,
        findings: output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_lengths(path: &str, source: &str) -> (Vec<Finding>, Vec<Finding>) {
        let lang = Language::for_path(path).unwrap();
        let original = crate::domain::code::scored(lang, source, path, None)
            .1
            .unwrap();
        let adjusted = adjust(lang, path, source, &original);
        let lengths = |s: &Score| {
            s.findings
                .iter()
                .filter(|f| f.rule == Rule::Length)
                .cloned()
                .collect()
        };
        (
            lengths(&original),
            lengths(adjusted.as_ref().unwrap_or(&original)),
        )
    }

    #[test]
    fn short_cases_split_but_long_cases_hooks_and_unowned_code_survive() {
        let small = "It 'short' {\n  Assert-True\n}\n".repeat(25);
        let work = "  Invoke-Operation\n".repeat(72);
        for (prefix, owner) in [("It 'long' {", "It 'long'"), ("BeforeAll {", "BeforeAll")] {
            let text = format!("{small}{prefix}\n{work}}}\n");
            let (before, after) = find_lengths("tests/Suite.Tests.ps1", &text);
            assert_eq!(before.len(), 1);
            assert_eq!(after.len(), 1);
            assert!(after[0].unit.starts_with(owner));
            assert_eq!(after[0].value, 74);
            assert_eq!(after[0].line, 76);
        }
        let (_, after) = find_lengths("tests/Suite.Tests.ps1", &format!("{small}{work}"));
        assert_eq!((after[0].unit.as_str(), after[0].value), ("<file>", 72));
    }

    #[test]
    fn local_helper_retains_its_own_length() {
        let text = format!(
            "import 'package:test/test.dart';\nvoid helper() {{\n{}}}\nvoid main() {{\n{}}}\n",
            "  step();\n".repeat(75),
            "  test('short', () {\n    verify();\n  });\n".repeat(30)
        );
        let (before, after) = find_lengths("test/sample_test.dart", &text);
        let helper = before.iter().find(|f| f.unit == "helper").unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(
            (&after[0].unit, after[0].value),
            (&helper.unit, helper.value)
        );
    }

    #[test]
    fn pgtap_wrapper_needs_plan_and_finish_and_preserves_long_cases() {
        let checks =
            "SELECT * FROM check_test(\n  ok(true), true, 'case', 'expected', ''\n);\n".repeat(30);
        let (_, ordinary) = find_lengths("src/queries.sql", &checks);
        assert_eq!(ordinary[0].unit, "<file>");
        let long = format!("SELECT * FROM check_test(\n{}true);\n", "  1,\n".repeat(75));
        let (_, suite) = find_lengths(
            "test/suite.sql",
            &format!("SELECT plan(31);\n{checks}{long}SELECT * FROM finish();"),
        );
        assert_eq!(suite.len(), 1);
        assert_eq!(suite[0].line, 92);
        assert_eq!(suite[0].value, 77);
    }
}
