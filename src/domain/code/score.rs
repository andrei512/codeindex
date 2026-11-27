//! File-local maintainability measurements. Store only findings and the weight
//! of the file; the score and its explanation are derived from the same record.
use super::{Analysis, Language, Token};
use std::sync::Arc;
mod control;
mod duplicates;
pub(super) mod embedded_tests;
mod owners;
pub(crate) mod policy;
mod scan;
mod suite_length;
pub(crate) mod suite_nesting;
pub(crate) mod test_logic;

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
#[derive(Clone, Copy)]
struct RuleInfo {
    id: &'static str,
    label: &'static str,
    threshold: usize,
}

impl RuleInfo {
    const fn new(id: &'static str, label: &'static str, threshold: usize) -> Self {
        Self {
            id,
            label,
            threshold,
        }
    }
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
    const fn info(self) -> RuleInfo {
        const INFO: [RuleInfo; Rule::ALL.len()] = [
            RuleInfo::new("complexity", "branch complexity", 9),
            RuleInfo::new("length", "unit lines", 70),
            RuleInfo::new("conditions", "compound conditions", 2),
            RuleInfo::new("regions", "nested regions", 2),
            RuleInfo::new("depth", "nesting depth", 4),
            RuleInfo::new("parameters", "parameters", 5),
            RuleInfo::new("duplication", "similar body (code lines)", 10),
            RuleInfo::new("oracle", "conditional test expectation", 2),
        ];
        INFO[self as usize]
    }

    pub(crate) fn label(self) -> &'static str {
        self.info().label
    }

    pub(crate) fn id(self) -> &'static str {
        self.info().id
    }

    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rule| rule.id() == name)
    }

    pub(crate) fn threshold(self) -> usize {
        policy::measurement_threshold(self)
    }

    pub(crate) const fn default_threshold(self) -> usize {
        self.info().threshold
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
mod test_syntax;
use test_syntax::TestSyntax;
pub(super) use test_syntax::{Input, PairedSyntax};
impl Input<'_, '_> {
    /// Store an override only when the framework policy changes the raw score.
    fn adjustment(
        self,
        doc: &super::Document,
        base: Option<&Score>,
        facts: Option<&Analysis>,
        written: Option<Language>,
    ) -> Saved {
        let original = base.filter(|s| s.lines > 0)?;
        adjust_tests(
            doc.path,
            original,
            &TestSyntax::new(written?, doc.text, facts, self),
        )
    }

    pub(super) fn policies(
        self,
        doc: &super::Document,
        raw: Saved,
        facts: Option<&Analysis>,
        tests: bool,
    ) -> (Saved, Saved) {
        let adjusted = if tests {
            self.adjustment(doc, raw.as_deref(), facts, test_language(doc.path))
        } else {
            None
        };
        (raw, adjusted)
    }
}

fn test_language(path: &str) -> Option<Language> {
    if path.ends_with(".t") {
        Some(Language::Perl)
    } else {
        Language::for_path(path)
    }
}

fn adjust_tests(path: &str, original: &Score, syntax: &TestSyntax<'_, '_>) -> Saved {
    let lang = syntax.lang;
    let role = super::roles::RolePath::new(path, lang);
    let test_path = || role.is_support();
    let mut adjusted = if suite_length::supported(lang, test_path) {
        suite_length::adjust(&role, original, syntax)
    } else {
        None
    };
    if suite_nesting::candidate(lang, test_path)
        && let Some(result) =
            suite_nesting::adjust(&role, adjusted.as_ref().unwrap_or(original), syntax)
    {
        adjusted = Some(result);
    }
    if test_logic::supports(lang)
        && test_path()
        && let Some(result) =
            test_logic::adjust(&role, adjusted.as_ref().unwrap_or(original), syntax)
    {
        adjusted = Some(result);
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
        Self::value_of(&self.findings)
    }

    pub(crate) fn value_of<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> f64 {
        let mut penalty = Penalties::default();
        for finding in findings {
            penalty.add(finding);
        }
        penalty.score()
    }
}

#[derive(Default)]
struct Penalties {
    sums: [f64; Rule::ALL.len()],
    deeply_nested_units: usize,
}

impl Penalties {
    fn add(&mut self, finding: &Finding) {
        let value = finding.value as f64;
        let total = &mut self.sums[finding.rule as usize];
        match finding.rule {
            Rule::Nesting => {
                *total = total.max(value);
                self.deeply_nested_units += 1;
            }
            Rule::Bumps => {
                let regions = 1. + 0.5 * (value - 3.).max(0.);
                *total += regions.min(4. + 0.5 * regions);
            }
            _ => *total += value,
        }
    }

    fn score(self) -> f64 {
        let [
            cc,
            lines,
            conditions,
            bumps,
            nesting,
            args,
            duplicates,
            oracle,
        ] = self.sums;
        let base = if cc > 0. || lines > 0. { 2_f64 } else { 0. };
        let penalty = (cc * 0.18).max(base.max(lines * 0.036) + conditions)
            // Taper repeated bumps within a unit. Count independent deep units
            // separately so one function cannot mask complexity in another.
            + bumps + oracle
            + nesting + self.deeply_nested_units.saturating_sub(1) as f64
            + if args > 0. { 2. } else { 0. }
            + if duplicates > 0. { 4. } else { 0. };
        1. + 9. * 0.9825255625212619_f64.powf(penalty)
    }
}
/// Expand the token stream the SQL scorer uses for mixed procedural files.
pub(super) fn expanded_sql<'s, 'text>(
    tokens: &'s [Token<'text>],
) -> std::borrow::Cow<'s, [Token<'text>]> {
    let mut output = Vec::new();
    let mut copied = 0;
    for (i, token) in tokens.iter().enumerate() {
        let Some(body) = super::lexer::dollar_content(token.text) else {
            continue;
        };
        output.extend_from_slice(&tokens[copied..i]);
        let base = token.span.start + body.start;
        output.extend(
            super::lexer::tokens(&token.text[body], Language::PlPgSql)
                .into_iter()
                .map(|mut inner| {
                    inner.line += token.line - 1;
                    inner.span = base + inner.span.start..base + inner.span.end;
                    inner
                }),
        );
        copied = i + 1;
    }
    if copied == 0 {
        return std::borrow::Cow::Borrowed(tokens);
    }
    output.extend_from_slice(&tokens[copied..]);
    std::borrow::Cow::Owned(output)
}

pub(super) fn with_tokens(lang: Language, text: &str, tokens: &[Token], facts: &Analysis) -> Saved {
    let input = scoring_tokens(lang, tokens, facts);
    let pairs = super::script::paired_for_score(&input);
    with_pairs(
        lang,
        text,
        PairedSyntax {
            tokens: &input,
            pairs: &pairs,
        },
        facts,
    )
}

fn scoring_tokens<'s, 'text>(
    lang: Language,
    tokens: &'s [Token<'text>],
    facts: &Analysis,
) -> std::borrow::Cow<'s, [Token<'text>]> {
    if matches!(lang, Language::Sql | Language::PlPgSql)
        && facts
            .declarations
            .iter()
            .any(|d| super::navigation::procedural_declaration(d.kind))
    {
        expanded_sql(tokens)
    } else {
        std::borrow::Cow::Borrowed(tokens)
    }
}

pub(super) fn sql_scores(
    doc: &super::Document,
    tokens: &[Token],
    facts: &Analysis,
    tests: bool,
) -> (Saved, Saved) {
    let lang = Language::Sql;
    let input = scoring_tokens(lang, tokens, facts);
    let pairs = super::script::paired_for_score(&input);
    if matches!(input, std::borrow::Cow::Borrowed(_)) {
        return paired_scores(
            super::scoring::ScoreSource { lang, doc },
            PairedSyntax {
                tokens,
                pairs: &pairs,
            },
            facts,
            tests,
        );
    }
    // Procedural bodies expand for the raw score. Framework rules use the written tokens.
    let base = with_pairs(
        lang,
        doc.text,
        PairedSyntax {
            tokens: &input,
            pairs: &pairs,
        },
        facts,
    );
    Input::Lexed(lang, tokens).policies(doc, base, Some(facts), tests)
}
pub(super) fn with_pairs(
    lang: Language,
    text: &str,
    syntax: PairedSyntax,
    facts: &Analysis,
) -> Saved {
    let source = scan::RegionSource {
        lang,
        text,
        syntax,
        facts,
    };
    let regions = source.regions();
    Some(Arc::new(source.measure(&regions)))
}
/// The raw and framework-adjusted scores share the same declaration ranges
/// when both policies consume the same written syntax and language.
pub(super) fn paired_scores(
    super::scoring::ScoreSource { lang, doc }: super::scoring::ScoreSource,
    input: PairedSyntax,
    facts: &Analysis,
    test_rules: bool,
) -> (Saved, Saved) {
    let (path, text) = (doc.path, doc.text);
    if !test_rules {
        return (with_pairs(lang, text, input, facts), None);
    }
    let written = test_language(path);
    if written != Some(lang) {
        let base = with_pairs(lang, text, input, facts);
        let adjusted = Input::Source.adjustment(doc, base.as_deref(), Some(facts), written);
        return (base, adjusted);
    }
    let syntax = TestSyntax::new(lang, text, Some(facts), Input::Paired(input));
    let raw = scan::RegionSource {
        lang,
        text,
        syntax: input,
        facts,
    };
    let base = Arc::new(raw.measure(syntax.regions()));
    let adjusted = if base.lines != 0 {
        adjust_tests(path, &base, &syntax)
    } else {
        None
    };
    (Some(base), adjusted)
}

/// Measure one captured source when the caller only needs the requested score.
pub(crate) fn source(
    path: &str,
    text: &str,
    project: Option<&super::CProject>,
    test_rules: bool,
) -> Saved {
    use crate::domain::markdown::{is_markdown, score_sections};
    if is_markdown(path) {
        return markdown_sections(text, &score_sections(path, text), |_| false);
    }
    let lang = Language::for_path(path).or_else(|| {
        (test_rules && path.ends_with(".t") && suite_nesting::perl_source(text))
            .then_some(Language::Perl)
    })?;
    lang.with_analyzer(path, project, |parser| {
        parser.score(&super::Document { path, text }, test_rules)
    })
}

pub(crate) fn markdown(text: &str, parsed: &crate::domain::markdown::Parsed) -> Saved {
    markdown_sections(text, &parsed.sections, |_| false)
}

pub(crate) fn markdown_sections(
    text: &str,
    sections: &[crate::domain::markdown::Section],
    mut is_marker: impl FnMut(usize) -> bool,
) -> Saved {
    let mut score = Score::default();
    let mut budgets = SectionBudget {
        sections: sections.iter().skip(1).peekable(),
        unit: "<document>",
        start: 1,
        count: 0,
    };
    let mut offset = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        budgets.advance(i + 1, offset, &mut score);
        if !line.trim().is_empty() {
            budgets.count += usize::from(!is_marker(i + 1));
            // Reporting deducts all excluded lines, including marker lines, once.
            score.lines += 1;
        }
        offset += line.len();
    }
    budgets.record(&mut score);
    Some(Arc::new(score))
}

struct SectionBudget<'a, I: Iterator<Item = &'a crate::domain::markdown::Section>> {
    sections: std::iter::Peekable<I>,
    unit: &'a str,
    start: usize,
    count: usize,
}

impl<'a, I: Iterator<Item = &'a crate::domain::markdown::Section>> SectionBudget<'a, I> {
    fn advance(&mut self, line: usize, offset: usize, score: &mut Score) {
        while self
            .sections
            .peek()
            .is_some_and(|section| section.start <= offset)
        {
            self.record(score);
            self.unit = &self.sections.next().unwrap().title;
            self.start = line;
            self.count = 0;
        }
    }

    fn record(&self, score: &mut Score) {
        score.record(Rule::Length, self.count, self.start, self.unit);
    }
}

#[cfg(test)]
mod tests;
