//! Prove a single file's graph inputs before scheduling unchanged files.
use super::{CapturePlan, Captured, Previous, analyze};
use crate::{
    application::index::layout::SOURCE,
    domain::code::{GraphComparison, Language, Unit},
};
use std::io;

pub(super) struct Prepared {
    pub checked: CheckedEdit,
    pub capture: Captured,
}

pub(super) struct CheckedEdit {
    pub id: usize,
    old_text: Option<String>,
    pub graph: GraphEdit,
}

impl CheckedEdit {
    pub fn plan_evidence(
        &self,
        rows: &mut std::borrow::Cow<'_, [Option<usize>]>,
    ) -> Option<(u64, u64)> {
        match self.graph {
            GraphEdit::Equal => {
                rows.to_mut()[self.id] = Some(self.id);
                None
            }
            GraphEdit::PatchEnd { offset, new_end } => Some((offset, new_end)),
            GraphEdit::Recompute => None,
        }
    }

    pub fn take_old_body(&mut self) -> Option<(usize, String)> {
        self.old_text.take().map(|text| (self.id, text))
    }
}

pub(super) enum GraphEdit {
    Recompute,
    Equal,
    PatchEnd { offset: u64, new_end: u64 },
}

impl GraphEdit {
    fn for_evidence(equal: bool) -> Self {
        if equal { Self::Equal } else { Self::Recompute }
    }

    pub fn preserves_graph(&self) -> bool {
        !matches!(self, Self::Recompute)
    }
}

pub(super) fn same_inputs(p: &dyn Previous, id: usize, unit: &Unit, old: &str) -> io::Result<bool> {
    let Some(lang) = Language::for_path(unit.path) else {
        return Ok(false);
    };
    if !lang.analyzer().edit_proof().is_some_and(|proof| {
        matches!(
            proof.compare(unit.analysis, old, unit.text),
            GraphComparison::CompareEvidence
        )
    }) {
        return Ok(false);
    }
    equal_evidence(p, id, unit.analysis)
}

fn equal_evidence(
    p: &dyn Previous,
    id: usize,
    analysis: &crate::domain::code::Analysis,
) -> io::Result<bool> {
    let range = &p.entries()[id].evidence;
    let mut encoded = Vec::new();
    analysis.encode(&mut encoded);
    if range.end - range.start != encoded.len() as u64 {
        return Ok(false);
    }
    p.same_bytes(range.clone(), &encoded)
}

fn single_changed_row(reuse: &[Option<usize>]) -> Option<usize> {
    let mut changed = reuse
        .iter()
        .enumerate()
        .filter(|(_, prior)| prior.is_none());
    let (id, _) = changed.next()?;
    changed.next().is_none().then_some(id)
}

pub(super) fn prepare(
    p: &dyn Previous,
    obs: &mut [super::ObservedPath],
    plan: &CapturePlan<'_>,
) -> io::Result<Option<Prepared>> {
    let Some(id) = single_changed_row(&plan.reuse) else {
        return Ok(None);
    };
    let file = &mut obs[id];
    let Some(text) = file.captured.as_ref().and_then(Option::as_ref) else {
        return Ok(None);
    };
    // Configured inputs must first run with the captured project view.
    if p.compilation_database().is_some() && crate::domain::code::CProject::recognizes(&file.path) {
        return Ok(None);
    }
    let Some(proof) = Language::for_path(&file.path).and_then(|lang| lang.analyzer().edit_proof())
    else {
        return Ok(None);
    };
    let Some(range) = &p.entries()[id].bytes else {
        return Ok(None);
    };
    let old_size = range.end - range.start;
    if old_size.saturating_add(text.len() as u64) > plan.budget.capture_credit as u64 {
        return Ok(None);
    }
    let base = p.extent(SOURCE).start;
    let mut bytes = Vec::with_capacity(old_size as usize);
    p.copy_bytes(base + range.start..base + range.end, &mut bytes)?;
    let old_text = String::from_utf8(bytes).map_err(io::Error::other)?;
    let mut local = analyze(&file.path, Some(text));
    let comparison = proof.compare(&local.code, &old_text, text);
    let graph = compare_graph(p, id, &mut local.code, comparison)?;
    let (prior, blob) = (plan.inputs.identify)(id, file, Some(text), super::PriorRow::Changed(id))?;
    let source = plan
        .budget
        .pool
        .retain(file.captured.take().flatten().expect("captured source"))?;
    Ok(Some(Prepared {
        checked: CheckedEdit {
            id,
            old_text: Some(old_text),
            graph,
        },
        capture: Captured {
            blob,
            source: Some(source),
            local: Some(local),
            prior,
        },
    }))
}

fn compare_graph(
    prior: &dyn Previous,
    id: usize,
    analysis: &mut crate::domain::code::Analysis,
    comparison: GraphComparison,
) -> io::Result<GraphEdit> {
    match comparison {
        GraphComparison::Recompute => Ok(GraphEdit::Recompute),
        GraphComparison::CompareEvidence => equal_evidence(prior, id, analysis).map(GraphEdit::for_evidence),
        GraphComparison::CompareWithOldEnd {
            declaration,
            old_end,
            new_end,
        } => compare_previous_span(prior, id, analysis, (declaration, old_end, new_end)),
    }
}

fn compare_previous_span(
    prior: &dyn Previous,
    id: usize,
    analysis: &mut crate::domain::code::Analysis,
    (declaration, old_end, new_end): (usize, usize, usize),
) -> io::Result<GraphEdit> {
    analysis.declarations[declaration].span.end = old_end;
    let equal = equal_evidence(prior, id, analysis);
    analysis.declarations[declaration].span.end = new_end;
    if equal? && let Some(offset) = prior.source_end_word(id, old_end)? {
        return Ok(GraphEdit::PatchEnd { offset, new_end: new_end as u64 });
    }
    Ok(GraphEdit::Recompute)
}
