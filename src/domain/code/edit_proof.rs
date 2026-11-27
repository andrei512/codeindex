//! A proof concerns graph reuse only. The caller must also compare serialized
//! evidence and verify the generation's path mapping and product versions.
use super::Analysis;

pub(crate) enum GraphComparison {
    Recompute,
    CompareEvidence,
    CompareWithOldEnd {
        declaration: usize,
        old_end: usize,
        new_end: usize,
    },
}

pub(crate) trait EditProof: Sync {
    fn compare(&self, analysis: &Analysis, before: &str, after: &str) -> GraphComparison;
}

/// Local bindings depend on saved declarations, calls, direct edges, and the
/// source spelling at those sites. The storage layer proves the rows identical.
pub(super) struct LocalNames;
impl EditProof for LocalNames {
    fn compare(&self, facts: &Analysis, before: &str, after: &str) -> GraphComparison {
        if has_language_evidence(facts) {
            return GraphComparison::Recompute;
        }
        let mut spans = facts
            .declarations
            .iter()
            .map(|d| &d.span)
            .chain(facts.calls.iter().map(|c| &c.span));
        if spans.all(|r| {
            after
                .get(r.clone())
                .is_some_and(|s| before.get(r.clone()) == Some(s))
        }) {
            GraphComparison::CompareEvidence
        } else {
            GraphComparison::Recompute
        }
    }
}

fn has_language_evidence(facts: &Analysis) -> bool {
    facts.direct_evidence.is_some() || facts.script.is_some() || facts.rust.is_some()
}
