//! Divide an existing length finding between the registered cases and their owning unit.
use super::owners::{Opening, OwnerCursor};
use super::{Finding, Language, Rule, Score};
use crate::domain::code::roles;

pub(super) fn supported(lang: Language, test_path: impl FnOnce() -> bool) -> bool {
    match lang {
        Language::Ruby | Language::Powershell => test_path(),
        Language::Dart | Language::Sql | Language::PlPgSql => true,
        _ => false,
    }
}

pub(super) fn adjust(
    role: &roles::RolePath,
    score: &Score,
    syntax: &super::TestSyntax<'_, '_>,
) -> Option<Score> {
    let (lang, text) = (syntax.lang, syntax.text);
    let mut lengths = score
        .findings
        .iter()
        .filter(|finding| eligible(lang, finding));
    let first = lengths.next()?;
    let input = syntax.paired();
    let tokens = input.tokens;
    let cases = role.suite_regions(text, input, &syntax.ruby_blocks);
    if cases.is_empty() || cases.windows(2).any(|w| w[0].end > w[1].start) {
        return None;
    }
    let mut measured = vec![Budget::new(first, cases.len())];
    for finding in lengths {
        let owner = (finding.unit.as_str(), finding.line);
        if measured.iter().all(|b| b.owner != owner) {
            measured.push(Budget::new(finding, cases.len()));
        }
    }
    count_lines(&mut measured, syntax, tokens, &cases);
    emit_findings(syntax, score, &cases, &measured)
}

fn count_lines(
    measured: &mut [Budget<'_>],
    syntax: &super::TestSyntax<'_, '_>,
    tokens: &[super::Token<'_>],
    cases: &[roles::Region],
) {
    let keys: Vec<_> = measured.iter().map(|budget| budget.owner).collect();
    let owner_position = |owner| keys.iter().position(|&key| key == owner);
    let file_owner = owner_position(("<file>", 1));
    let mut openings = syntax
        .regions()
        .iter()
        .map(|r| Opening {
            start: r.start,
            end: r.end,
            owner: owner_position((r.name, tokens[r.start].line)),
        })
        .peekable();
    let mut owners = OwnerCursor::new(tokens.len(), file_owner);
    let mut cursor = 0;
    while let Some((owner, run)) = owners.next_run(&mut openings, |_| {}) {
        let Some(owner) = *owner else { continue };
        for token in &tokens[run] {
            measured[owner].count(token, cases, &mut cursor);
        }
    }
}

fn emit_findings(
    syntax: &super::TestSyntax,
    score: &Score,
    cases: &[roles::Region],
    measured: &[Budget<'_>],
) -> Option<Score> {
    // Copy unchanged findings only after confirming the native line count.
    let mut findings = Vec::new();
    let mut copied = 0;
    let apportioned = score
        .findings
        .iter()
        .enumerate()
        .filter(|(_, finding)| eligible(syntax.lang, finding))
        .filter_map(|(i, finding)| {
            let budget = measured.iter().find(|budget| budget.apportions(finding))?;
            Some((i, finding, budget))
        });
    for (i, f, budget) in apportioned {
        findings.extend_from_slice(&score.findings[copied..i]);
        budget.emit(f, cases, syntax.text, &mut findings);
        copied = i + 1;
    }
    if copied == 0 {
        return None;
    }
    findings.extend_from_slice(&score.findings[copied..]);
    Some(Score {
        lines: score.lines,
        findings,
    })
}

fn eligible(lang: Language, finding: &Finding) -> bool {
    finding.rule == Rule::Length
        && (finding.unit == "<file>" || lang == Language::Dart && finding.unit == "main")
}

struct Budget<'a> {
    owner: (&'a str, usize),
    last: usize,
    total: usize,
    residual: usize,
    cases: Vec<usize>,
}

impl<'a> Budget<'a> {
    fn new(finding: &'a Finding, cases: usize) -> Self {
        Self {
            owner: (&finding.unit, finding.line),
            cases: vec![0; cases],
            last: 0,
            total: 0,
            residual: 0,
        }
    }

    fn apportions(&self, finding: &Finding) -> bool {
        self.owner == (finding.unit.as_str(), finding.line)
            && self.total == finding.value
            && self.residual < self.total
    }

    fn count(&mut self, token: &super::Token, cases: &[roles::Region], cursor: &mut usize) {
        // A line shared by adjacent registrations counts once for this owner.
        if self.last == token.line {
            return;
        }
        self.last = token.line;
        self.total += 1;
        while *cursor < cases.len() && cases[*cursor].end <= token.span.start {
            if token.text == ";" && token.span.start == cases[*cursor].end {
                break;
            }
            *cursor += 1;
        }
        if cases
            .get(*cursor)
            .is_some_and(|r| r.start <= token.span.start)
        {
            self.cases[*cursor] += 1;
        } else {
            self.residual += 1;
        }
    }

    fn emit(
        &self,
        original: &Finding,
        regions: &[roles::Region],
        text: &str,
        out: &mut Vec<Finding>,
    ) {
        let threshold = Rule::Length.threshold();
        if self.residual >= threshold {
            out.push(Finding {
                value: self.residual,
                ..original.clone()
            });
        }
        for (region, &value) in regions.iter().zip(&self.cases) {
            if value >= threshold {
                let label = text[region.start..region.end]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim();
                out.push(Finding {
                    rule: Rule::Length,
                    line: region.first,
                    unit: label.chars().take(100).collect(),
                    value,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_lengths(path: &str, source: &str) -> (Vec<Finding>, Vec<Finding>) {
        let lang = Language::for_path(path).unwrap();
        let (facts, original) = crate::domain::code::scored(lang, source, path, None);
        let original = original.unwrap();
        let adjusted = adjust(
            &roles::RolePath::new(path, lang),
            &original,
            &super::super::TestSyntax::new(lang, source, Some(&facts), super::super::Input::Source),
        );
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
