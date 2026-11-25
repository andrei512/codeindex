# Evaluate test-code identification

The unit of evaluation is one labelled source location at a fixed revision. The expected role is runnable test, test support, or production. Predictions can also abstain. One checkout belongs to one split. The benchmark compares the public `cx --score --roles --json` result with the earlier filename-only baseline on the same locations. The baseline calls a test-path hit a runnable test.

```sh
cargo build --release --locked
python3 benchmarks/test_roles/eval.py --coverage
python3 benchmarks/test_roles/eval.py --split dev
# Regression check on the published test split
python3 benchmarks/test_roles/eval.py --split test
```

The development run is also available as `npx nx run codeindex-workspace:eval-test-roles`. Python, Git and network access are needed for the first fetch. The harness verifies source digests, stages public source and required context in temporary Git worktrees, runs the release binary, and keeps labels outside those worktrees. Results and cached sources go under `target/test-role-eval/`. A result records the executable digest, manifest digests, each disagreement and a confusion matrix per profile. Report language-macro accuracy alongside pooled counts, since Python and C have large runner-generated panels. Also report production labelled as test, missed tests, confusion between tests and helpers, and abstentions. For labels that specify only a line, consult the separate metric excluding lines with multiple predicted roles.

## Frozen evaluation

Across 877 held-out locations in 68 repositories, Codeindex got 852 test-side versus production decisions right. The filename baseline got 824. When runnable tests and helpers are scored separately, Codeindex got 813 right and the baseline got 656. The most frequent error was labelling a runnable test as test support (36 locations). Equal-weight mean exact-role accuracy over 35 profiles was 84.9%. The smallest profiles have only ten anchors. All eight abstentions count as incorrect in these figures. The held-out observations are now published evaluation results. [Per-profile counts and frozen hashes](results.json).

## Regression results after reviewing the failures

The two original splits contained 71 exact-role disagreements. Fixes recovered 66 against the original labels with zero newly failing locations. The five remaining disagreements are Java labels. Three methods build fixtures or implement assertions in a test source set. Two methods named `testSerialization` are absent from the pinned JUnit runner’s discovery output. The harness retains upstream expectations in `overall` and reports the reviewed comparison under `reviewed`. All 2,467 reviewed locations agree. Both splits were used during repair and now measure regressions. The additional `*_context.jsonl` files supply pinned source used by the real projects, without adding votes to the score. [Regression results](regression_results.json) · [Label decisions and runner evidence](adjudications.json)

## Where the labels come from

- `public.jsonl` and `assets.jsonl` contain reviewed line anchors, a reason for each label, the public revision, path and SHA-256. They include shipped testing libraries, runnable tests, helper implementations and data files read by test runners. JavaScript and TypeScript share the JS profile.
- `runner.jsonl` records registrations observed in Click's pytest collection and libuv's compiled test runner. Repeated parameterizations of the same declaration count once. An unlabelled header supplies macro definitions. See `runner_labels.py --help` for regeneration requirements.
- `testroutes.py` joins the published TestRoutes Java method and route tables to their original project revisions. It uses annotated test, helper, focal and contextual methods, and records exclusions for unresolved coordinates. Upstream role labels are retained even when an assertion helper was annotated as focal. [TestRoutes dataset](https://doi.org/10.5281/zenodo.3741674).
- The optional `lexer_inputs.jsonl` panel covers source files that Pygments reads as **fixture data**. Its labels come from its collector and paired golden files. Run `python3 benchmarks/test_roles/lexer_inputs.py` to regenerate and `python3 benchmarks/test_roles/eval.py --manifest benchmarks/test_roles/lexer_inputs.jsonl --without-testroutes --output target/test-role-eval/results-lexer.json` to evaluate it separately.

When extending the eval, add a label because a runner collected a callable, a test consumes a fixture, or reviewed source establishes its role. Include the evidence. An undocumented method in a test dataset has no gold label. Count one vote per unique location. Add missing projects to a new holdout before tuning on published holdout errors. The panel measures these examples; sparse profiles and frameworks require additional independent projects to estimate population accuracy.
