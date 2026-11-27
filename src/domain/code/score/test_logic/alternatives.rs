//! Braced else arms continue the same decision. Reconcile the native
//! nested-region count only when replay reproduces its original value.
use super::{Cow, Finding, HashMap, Rule, Suites, scan};
use crate::domain::code::score::{Language, PairedSyntax, TestSyntax};

pub(super) fn correct<'a>(
    syntax: &TestSyntax<'_, '_>,
    original: &'a [Finding],
) -> Cow<'a, [Finding]> {
    // Same-line, same-name units can have more than one finding. A later
    // region sees the next original finding only if its predecessor was removed.
    let mut candidates = candidates(original);
    let paired = syntax.paired();
    let mut edits = HashMap::new();
    for region in syntax.regions() {
        let key = (paired.tokens[region.start].line, region.name);
        correct_candidate(candidates.get_mut(&key), original, &mut edits, |previous| {
            replay(paired, syntax.lang, region.start..region.end, previous)
        });
    }
    if edits.is_empty() {
        return Cow::Borrowed(original);
    }
    Cow::Owned(
        original
            .iter()
            .enumerate()
            .filter_map(|(i, f)| match edits.get(&i) {
                Some(&value) if value < Rule::Bumps.threshold() => None,
                Some(&value) => Some(Finding { value, ..f.clone() }),
                None => Some(f.clone()),
            })
            .collect(),
    )
}

fn correct_candidate(
    group: Option<&mut Vec<usize>>,
    original: &[Finding],
    edits: &mut HashMap<usize, usize>,
    replay: impl FnOnce(usize) -> Option<usize>,
) {
    let Some(group) = group else { return };
    let Some(&index) = group.last() else { return };
    let previous = *edits.get(&index).unwrap_or(&original[index].value);
    let Some(corrected) = replay(previous) else {
        return;
    };
    if corrected < Rule::Bumps.threshold() {
        group.pop();
    }
    edits.insert(index, corrected);
}

fn candidates(original: &[Finding]) -> HashMap<(usize, &str), Vec<usize>> {
    let mut candidates = HashMap::<_, Vec<_>>::new();
    for (i, finding) in original.iter().enumerate().rev() {
        if finding.rule == Rule::Bumps {
            candidates
                .entry((finding.line, finding.unit.as_str()))
                .or_default()
                .push(i);
        }
    }
    candidates
}

fn replay(
    paired: PairedSyntax<'_, '_>,
    lang: Language,
    span: std::ops::Range<usize>,
    previous: usize,
) -> Option<usize> {
    if !has_else_bridge(paired.tokens, span.clone()) {
        return None;
    }
    let (raw, corrected) = counts(paired, lang, span);
    (raw == previous && corrected < raw).then_some(corrected)
}

// Only these two token positions can keep a deep run alive across an else.
// The neighbors belong to the file even if a measured region ends mid-header.
fn has_else_bridge(tokens: &[super::Token], span: std::ops::Range<usize>) -> bool {
    let lookahead = (span.end + 2).min(tokens.len());
    tokens[span.start.saturating_sub(1)..lookahead]
        .windows(3)
        .any(|w| w[0].text == "}" && w[1].text == "else" && w[2].text == "{")
}

fn counts(
    PairedSyntax { tokens, pairs }: PairedSyntax<'_, '_>,
    lang: Language,
    region: std::ops::Range<usize>,
) -> (usize, usize) {
    let end = region.end;
    let mut suites = Suites::default();
    let (mut raw, mut corrected) = (0, 0);
    let (mut previous, mut deep) = (false, false);
    let braces = super::super::control::BraceGrammar::for_language(lang).with_tokens(tokens, pairs);
    for i in region {
        let depth = suites.depth(braces, i, end);
        let nested = depth >= 2;
        raw += usize::from(nested && !previous);
        previous = nested;
        if depth != 1 || !else_bridge(tokens, i) {
            corrected += usize::from(nested && !deep);
            deep = nested;
        }
    }
    (raw, corrected)
}

fn else_bridge(t: &[super::Token], i: usize) -> bool {
    let start = match t[i].text {
        "}" => i,
        "else" if i > 0 => i - 1,
        _ => return false,
    };
    t[start].text == "}" && scan::at(t, start + 1) == "else" && scan::at(t, start + 2) == "{"
}
