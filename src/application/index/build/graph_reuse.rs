use super::*;
use std::collections::BTreeMap;

/// Capturing with a retained graph can skip parser evidence. A prediction from
/// observed paths must therefore survive the final source-identity comparison.
pub(super) struct GraphReuse<'a> {
    previous: Option<&'a dyn Previous>,
    eligible: bool,
    predicted: bool,
    edit: Option<script_edit::CheckedEdit>,
}
impl<'a> GraphReuse<'a> {
    pub fn predict(
        previous: Option<&'a dyn Previous>,
        eligible: bool,
        obs: &[ObservedPath],
        rows: &[Option<usize>],
    ) -> Self {
        let previous = previous.filter(|p| p.compatible(SYMBOLS) && p.compatible(REL));
        Self {
            previous,
            eligible,
            predicted: eligible && previous.is_some() && graph_unchanged(obs, rows),
            edit: None,
        }
    }
    pub fn prepare(
        &mut self,
        plan: &CapturePlan<'_>,
        obs: &mut [ObservedPath],
        table: &mut CaptureTable,
    ) -> io::Result<()> {
        let Some(prior) = self
            .previous
            .filter(|_| self.eligible && plan.inputs.predecessor.forward_known && !self.predicted)
        else {
            return Ok(());
        };
        let Some(edit) = script_edit::prepare(prior, obs, plan)? else {
            return Ok(());
        };
        table.row(edit.checked.id).store(edit.capture);
        self.edit = Some(edit.checked);
        Ok(())
    }
    pub fn prepared_row(&self) -> Option<usize> {
        self.edit.as_ref().map(|edit| edit.id)
    }
    pub fn take_old_body(&mut self) -> Option<(usize, String)> {
        self.edit
            .as_mut()
            .and_then(script_edit::CheckedEdit::take_old_body)
    }
    fn proved(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|edit| edit.graph.preserves_graph())
    }
    pub fn retain_during_capture(&self) -> bool {
        self.predicted || self.proved()
    }
    pub fn verify(&self, obs: &[ObservedPath], rows: &[Option<usize>]) -> io::Result<bool> {
        let retained =
            self.proved() || self.eligible && self.previous.is_some() && graph_unchanged(obs, rows);
        if self.retain_during_capture() && !retained {
            return Err(io::Error::other(
                "a verified binding input changed during capture",
            ));
        }
        Ok(retained)
    }
    pub fn plan_products(
        &self,
        retained: bool,
        config: &c_project::Refresh,
        inputs: GraphInputs<'_>,
        reused: &mut std::borrow::Cow<'_, [Option<usize>]>,
    ) -> io::Result<GraphPlan<'a>> {
        let rebuild = GraphPlan::Build {
            configured: config.catalog.is_some(),
        };
        let Some(previous) = self.previous else {
            return Ok(rebuild);
        };
        let end_patch = self
            .edit
            .as_ref()
            .and_then(|edit| edit.plan_evidence(reused));
        if retained {
            return Ok(GraphPlan::Retain {
                previous,
                end_patch,
            });
        }
        if self.edit.is_some() || config.invalidates_graph() {
            return Ok(rebuild);
        }
        if same_binding_inputs(previous, inputs.units, inputs.before, reused)? {
            Ok(GraphPlan::Retain {
                previous,
                end_patch,
            })
        } else {
            Ok(rebuild)
        }
    }
}

fn graph_unchanged(observed: &[crate::domain::ObservedPath], reuse: &[Option<usize>]) -> bool {
    observed
        .iter()
        .zip(reuse)
        .enumerate()
        .all(|(id, (file, reused))| *reused == Some(id) || !code::Names::consumes_text(&file.path))
}

fn same_binding_inputs(
    prior: &dyn Previous,
    units: &[Unit],
    edited: Option<&BTreeMap<usize, String>>,
    evidence_reuse: &mut std::borrow::Cow<'_, [Option<usize>]>,
) -> io::Result<bool> {
    let Some(edited) = edited else {
        return Ok(false);
    };
    // edited_sources supplies a complete delta only when all paths and ordinals agree.
    let binding_edits = edited
        .iter()
        .filter(|&(&id, _)| code::Names::consumes_text(units[id].path));
    for (&id, old_text) in binding_edits {
        if !super::script_edit::same_inputs(prior, id, &units[id], old_text)? {
            return Ok(false);
        }
        evidence_reuse.to_mut()[id] = Some(id);
    }
    Ok(true)
}
