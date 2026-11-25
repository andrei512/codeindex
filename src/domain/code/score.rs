//! File-local maintainability measurements. Store only findings and the weight
//! of the file; the score and its explanation are derived from the same record.
use super::{Analysis, Language, Token};
use std::sync::Arc;
mod control;
mod duplicates;
mod embedded_tests;
mod registrations;
mod scan;
mod suite_length;
pub(crate) mod suite_nesting;
pub(crate) mod test_logic;
pub(crate) use suite_length::{adjust as suite_adjustment, candidate as suite_candidate};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Rule {
    Complexity,
    Length,
    Conditionals,
    Bumps,
    Nesting,
    Arguments,
    Duplication,
    Oracle,
}
impl Rule {
    pub(crate) const ALL: [Self; 8] = [
        Self::Complexity,
        Self::Length,
        Self::Conditionals,
        Self::Bumps,
        Self::Nesting,
        Self::Arguments,
        Self::Duplication,
        Self::Oracle,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Complexity => "branch complexity",
            Self::Length => "unit lines",
            Self::Conditionals => "compound conditions",
            Self::Bumps => "nested regions",
            Self::Nesting => "nesting depth",
            Self::Arguments => "parameters",
            Self::Duplication => "similar body (code lines)",
            Self::Oracle => "conditional test expectation",
        }
    }
    pub(crate) fn threshold(self) -> usize {
        match self {
            Self::Complexity => 9,
            Self::Length => 70,
            Self::Conditionals | Self::Bumps | Self::Oracle => 2,
            Self::Nesting => 4,
            Self::Arguments => 5,
            Self::Duplication => 10,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub rule: Rule,
    pub line: usize,
    pub unit: String,
    pub value: usize,
}
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Score {
    pub lines: usize,
    pub findings: Vec<Finding>,
}
pub(crate) type Saved = Option<Arc<Score>>;
/// Store an override only when the default policy changes the base score.
pub(super) fn for_tests(
    path: &str,
    text: &str,
    base: Option<&Score>,
    analysis: &Analysis,
    project: bool,
    syntax: Option<(&[Token], &[usize])>,
) -> Saved {
    let original = base.filter(|s| s.lines > 0)?;
    let mut adjusted = suite_candidate(path, original)
        .and_then(|lang| suite_adjustment(lang, path, text, original));
    if let Some(lang) = suite_nesting::candidate(path)
        && let Some(result) =
            suite_nesting::adjust(lang, path, text, adjusted.as_ref().unwrap_or(original))
    {
        adjusted = Some(result);
    }
    if let Some(lang) = test_logic::candidate(path) {
        // The test policy measures written C syntax independently of build flags.
        let facts = (!(project && matches!(lang, Language::C | Language::Cpp))).then_some(analysis);
        if let Some(result) = test_logic::adjust(
            lang,
            path,
            text,
            adjusted.as_ref().unwrap_or(original),
            facts,
            syntax,
        ) {
            adjusted = Some(result);
        }
    }
    adjusted.filter(|s| s != original).map(Arc::new)
}

impl Score {
    fn record(&mut self, rule: Rule, value: usize, line: usize, name: &str) {
        if value >= rule.threshold() {
            self.findings.push(Finding {
                rule,
                value,
                line,
                unit: name.into(),
            });
        }
    }
    pub(crate) fn value(&self) -> f64 {
        let mut sums = [0.; 8];
        let mut deeply_nested_units = 0usize;
        for f in &self.findings {
            let value = f.value as f64;
            let total: &mut f64 = &mut sums[f.rule as usize];
            if f.rule == Rule::Nesting {
                *total = total.max(value);
                deeply_nested_units += 1;
            } else if f.rule == Rule::Bumps {
                let regions = 1. + 0.5 * (value - 3.).max(0.);
                *total += regions.min(4. + 0.5 * regions);
            } else {
                *total += value;
            }
        }
        let [
            cc,
            lines,
            conditions,
            bumps,
            nesting,
            args,
            duplicates,
            oracle,
        ] = sums;
        let base = if cc > 0. || lines > 0. { 2_f64 } else { 0. };
        let penalty = (cc * 0.18).max(base.max(lines * 0.036) + conditions)
            // Taper repeated bumps within a unit. Count independent deep units
            // separately so one function cannot mask complexity in another.
            + bumps + oracle
            + nesting + deeply_nested_units.saturating_sub(1) as f64
            + if args > 0. { 2. } else { 0. }
            + if duplicates > 0. { 4. } else { 0. };
        1. + 9. * 0.9825255625212619_f64.powf(penalty)
    }
}
pub(super) fn with_tokens(lang: Language, text: &str, tokens: &[Token], facts: &Analysis) -> Saved {
    let mut expanded = Vec::new();
    let tokens = if matches!(lang, Language::Sql | Language::PlPgSql)
        && facts
            .declarations
            .iter()
            .any(|d| matches!(d.kind, "plpgsql_function" | "plpgsql_procedure"))
    {
        for token in tokens {
            let w = token.text;
            if let Some(rest) = w.strip_prefix('$')
                && let Some(pos) = rest.find('$')
                && let width = pos + 2
                && w.len() >= 2 * width
                && w.ends_with(&w[..width])
            {
                for mut inner in super::lexer::tokens(&w[width..w.len() - width], Language::PlPgSql)
                {
                    inner.line += token.line - 1;
                    let base = token.span.start + width;
                    inner.span = base + inner.span.start..base + inner.span.end;
                    expanded.push(inner);
                }
            } else {
                expanded.push(Token {
                    text: w,
                    line: token.line,
                    kind: token.kind,
                    span: token.span.clone(),
                });
            }
        }
        expanded.as_slice()
    } else {
        tokens
    };
    let pairs = super::script::paired_for_score(tokens);
    with_pairs(lang, text, tokens, &pairs, facts)
}
pub(super) fn with_pairs(
    lang: Language,
    text: &str,
    tokens: &[Token],
    pairs: &[usize],
    facts: &Analysis,
) -> Saved {
    Some(Arc::new(scan::measure(lang, text, tokens, pairs, facts)))
}
pub(crate) fn markdown(text: &str, parsed: &crate::domain::markdown::Parsed) -> Saved {
    let mut score = Score::default();
    let mut sections = parsed.sections.iter().skip(1).peekable();
    let mut unit = "<document>";
    let mut start = 1;
    let mut count = 0;
    let mut offset = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        while sections.peek().is_some_and(|s| s.start <= offset) {
            score.record(Rule::Length, count, start, unit);
            let s = sections.next().unwrap();
            unit = &s.title;
            start = i + 1;
            count = 0;
        }
        if !line.trim().is_empty() {
            count += 1;
            score.lines += 1;
        }
        offset += line.len();
    }
    score.record(Rule::Length, count, start, unit);
    Some(Arc::new(score))
}

#[cfg(test)]
mod tests;
