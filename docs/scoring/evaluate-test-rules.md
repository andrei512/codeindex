---
page-type: task
---
# Evaluate a proposed test scoring rule

Use this protocol to decide whether a proposed rule improves on the current scorer. The development panel and baseline are available now. Run the frozen policy with `cx --score --ignore-test-rules`. The default policy measures recognized suite children separately when assigning unit length.

## Select the evidence

Preserve each dataset’s original unit. A suite-level label can validate suite-level detection; it cannot identify a particular offending line. Separate generated suites from developer-written suites. Reuse a published maintainer rating only when its original example can be identified. The Spadini archive contains tool counts and calibration materials. Its published survey aggregates cannot supply individual maintainer verdicts. [Manual labels](https://figshare.com/s/7b8bf9a7580001929f63)

## Label the examples

For each new example, pin the repository revision, file, byte range, framework and label source. Two reviewers independently record the pattern, its impact and their reasoning before seeing either scorer's output. Keep disagreements and abstentions visible. For pair judgments, randomize left and right and check that both tests exercise the same behavior and detect the same representative failure. Include random tests drawn without reference to a detector so the false-alarm denominator reflects normal usage. Report deliberately enriched challenge cases separately. [Severity study replication package](https://doi.org/10.5281/zenodo.3611111)

## Compare the policies

Compare both policies on the same examples. Include healthy counterexamples and actionable cases for each rule, framework, and supported language. Keep repository families, translations, and before/after pairs in one split. Freeze rule definitions, weights and acceptance margins on development data before opening blind repositories. Report warning precision, false alarms per 100 reviewed healthy tests, actionable-finding recall, and agreement on easier-to-maintain pairs including ties. For a pair labelled equally maintainable, count an unchanged displayed score as agreement. Estimate paired uncertainty by resampling repository families and retaining their cases together. Report coverage and reviewer disagreements. An unsupported framework contributes to the coverage gap.

## Apply the decision

Adopt a rule only where the blind comparison shows fewer false alarms at acceptable recall, or higher recall at acceptable false-alarm cost, under the declared margins. The public score belongs to a file. Compare the same surrounding file with one test replaced in place, so unrelated code cannot decide the ranking. Pairwise judgments can validate ordering; an absolute point deduction requires a separate severity anchor or an explicit product convention. Select the adjustment on development data; the blind run accepts or rejects that frozen candidate. Any adjustment following inspection of the blind results needs a new blind panel. Where evidence is insufficient, retain the existing score. Always rerun the frozen CodeScene comparison in compatibility mode. Its purpose is to catch drift in the original model.
