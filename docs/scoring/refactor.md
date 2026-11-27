---
page-type: task
---
# Refactor a repository using score findings

Use the directory score to choose where to investigate. Use the individual findings to decide what to change. Keep the starting executable and report so you can measure the effect of the code changes under the same rules.

## Find a change worth making

Save the output of `cx --score . --json` outside the repository. Record the revision, local edits, scoring configuration and exclusions. Estimate each file’s share of the gap by multiplying its scorable line count by the number of points it falls short of ten. Group findings that arise from the same decision. A duplicated policy across twenty callers can deserve attention before the lowest-scoring file.

Follow one operation through its callers. Decide which component owns its rules and what complexity can be removed locally. Pick a case that would break if the change were wrong. Try the change on one representative path and compare its behavior and findings with the baseline.

## Expand a verified change

Once the pilot works, migrate the other affected callers. Give each worker a separate ownership group and a check it can run independently. Score the merged result, including newly created files. Compare it using the original line counts as well as the new counts so moving code between files cannot masquerade as the entire gain.

In Codeindex, one such pass took 289 edited or added scored files to ten. The repository moved from 9.71176 to 9.99207 and findings fell from 547 to 11 under the same scorer and exclusions. The remaining findings needed individual decisions. For example, regrouping a public function’s inputs required migrating its callers. Keeping a branch in an evaluation fixture preserved the behavior the fixture was built to test.

## Explain the last fraction of a point

Run the saved scorer on the finished code, then run the current scorer with the same configuration. On the final Codeindex tree the earlier scorer gave 9.99907. It counted declarative Rust wire tags as branches and an unnamed foreign callback argument as an ordinary parameter. With those two rules corrected, all 712 measured files scored ten. The repository policy excluded 14 generated tables and two intentional fixtures.

Report behavior checks, measured performance and coverage alongside the score. A ten means every measured file passed the configured rules. The packaged `refactor-with-cx` skill gives agents the procedure.
