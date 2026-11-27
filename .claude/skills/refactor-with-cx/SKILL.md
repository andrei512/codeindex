---
name: refactor-with-cx
description: Use cx to plan and verify maintainability improvements across a repository. Diagnose architectural and local problems, choose changes by their likely impact, and replan when results plateau.
---

## 1. Map the whole repository

Freeze the checkout including local edits, the scorer executable and configuration, and the full output of `cx --score . --json` in scratch. Record scored and excluded files and lines. Inspect the distribution and findings by subsystem. Look both at the lowest scores and at the largest contributions to the gap. For a line-weighted score, a file contributes `starting lines × (10 − starting score) / total starting lines`. A cluster of medium-scoring files can outweigh the worst files.

For a numeric target, compute the best possible result from the proposed scope with all other files held fixed. Apply this ceiling to the overall plan. An individual pilot can earn its place by testing one shared cause. Forecast achievable improvement separately using the particular findings a change could remove. Read as many files as the diagnosis needs.

When many files have findings, plan across the whole gap. Pick a meaningful share of it (half is a useful first estimate) and identify enough causes to cover it. A top-50 list is a reading aid. Repeatedly exhausting that list while the same rules recur is a signal to change the diagnosis. Keep batches small enough to verify independently.

## 2. Trace a real operation and choose work

Start from an operation a user or maintainer performs. Follow one representative case and one case with a different outcome through producers, consumers and tests. Use `cx --reference NAME` for named relationships and `cx -n -F TEXT` for runtime registrations. Follow the trail into high-scoring files if they own the contract. Inspect a second operation when the first explains little of the gap.

Write one short proposal per cause. For example, two consumers calculate refund eligibility independently. The purchase policy could supply one decision to both. A partially refunded order distinguishes it from an untouched order. Changing eligibility should then require editing one owner. Before merging similar code, check whether the two consumers are supposed to disagree in any supported case.

For each proposal record:

- **Evidence and owner.** Where is the same rule decided twice, an invariant reconstructed, or one routine carrying unrelated responsibilities? For a local mechanical change, identify the specific avoidable complexity.
- **Change and prediction.** Name what will disappear, the affected callers and findings, and the expected score effect where estimable. Name the smallest observation that would disprove the diagnosis.
- **Choice.** Compare credible proposals on attainable gain, confidence, cost and risk. Give precedence to prerequisites that unlock other changes. Assign one owner per decision and parallelize independent work. Record justified complexity with the reason to retain it.

## 3. Prove a change before expanding it

Check pending patches and previous experiments for the same cause before editing. Reuse a verified result when its inputs and contract still match. Freeze a discriminating behavior check before editing. Exercise the normal entry point and relevant edge case. A weak check should fail on a deliberately wrong answer before it is used to approve the change. If the baseline itself is wrong, use an independent reference and record the correction separately. Keep held-out data sealed for its intended check.

Implement the smallest end-to-end change that tests the proposal. Preserve the contract across its callers. Check the predicted reduction directly, such as sites deciding a rule, tokenizations per input, or affected findings. Rescore the changed ownership group with the frozen scorer, including new and extracted files. Expand once the observation supports the hypothesis. Low-scoring or weakly tested code warrants smaller steps and stronger behavior evidence.

When a pilot succeeds, assign disjoint groups of files to workers. Give each worker the decision it owns, its callers, a behavior check and a score target. Coordinate changes to shared interfaces before parallelizing their callers. For a target of ten, rescore *every* edited or newly created scorable file in the group. Carry any remaining finding forward by name. Verify the merged tree before attributing the gain to a worker.

## 4. Verify the result and its cost

Use focused checks while developing. Run required integration checks on the combined result. For performance-sensitive stages, compare adjacent executables on identical inputs and compare the original and final builds end to end. Record executable identity, sample counts, cache conditions, output parity and variation. Repeat suspected regressions. Changes to the evaluator require comparing case identities and answers on the same binary before evaluating the candidate.

Report the whole-repository score and findings gained and lost. Also compare original ownership groups at starting line weights. Trace moved code back to its original owner, pool its findings when the formula permits, and label approximate attribution. Report changes to coverage, exclusions or scoring policy separately. Give distinct credit for simpler code, corrected behavior and measured performance. A file split or an increase in lines can raise the raw average by itself.

## 5. Decide what to do next

Keep a short record per hypothesis: affected operation, predicted change, measured result, and next decision. Use it when refreshing the whole-repository map.

- **Prediction confirmed.** Extend to verified callers, then pick the next cause with remaining evidence.
- **Design improved and score stayed flat.** Record the demonstrated benefit and revise the score forecast. Look elsewhere for the remaining numeric gain.
- **Prediction failed or the code merely moved.** Identify which assumption failed. Narrow, replace or discard the change. Retry only with a different mechanism or evidence that distinguishes the next attempt.
- **The same files recur.** Compare their remaining findings to the decision record. Follow an upstream owner, investigate another subsystem, or retain the design for a stated reason. A ranking change alone does not justify repeating a review.

Near the target, read every remaining finding. Classify it by the decision it requires:

- **Code under our control.** Remove the underlying complexity and exercise the affected callers. For an exported API, check external usage and document any approved migration.
- **Required behavior or data.** Preserve the example or external contract. A score fixture may need the very branch it demonstrates. Record any agreed exclusion by path and reason, together with the coverage change.
- **Measurement error.** Produce one example that should trigger the rule and one that should stay clear. Verify both, check existing evaluations and compare the old and new scorers on the same final tree with the same policy.

For an exact target, inspect individual unrounded results and coverage. A displayed average of 10.00 can still hide a finding.

Finish against the user's actual target and checks. If the target remains open and evidence-backed proposals run out, report the measured gap, the justified remaining complexity and what evidence would unlock another attempt.

## Research basis

Borg et al. studied single-pass refactoring of short Python competition programs. They evaluated test outcomes and score changes separately. Starting CodeHealth carried some risk information across groups, with weak prediction for an individual edit. The repository planning and plateau rules above come from engineering practice and measured refactoring rounds. [Study, methods and results](https://arxiv.org/pdf/2601.02200)
