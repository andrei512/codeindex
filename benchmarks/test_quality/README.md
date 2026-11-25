# Evaluate test maintainability

The development baseline uses real public files in every Codeindex language profile, including Markdown. JavaScript and TypeScript share an implementation profile. It measures what the current scorer emits before any test-specific rules or weights change. The classification eval answers a different question about the role of a source location.

## Run

```sh
cargo build --release --locked
python3 benchmarks/test_quality/eval.py
python3 benchmarks/test_quality/published/panel.py
python3 benchmarks/test_quality/published/baseline.py
```

The main runner accepts a release binary via `--binary`. It runs `cx --score --ignore-test-rules --json` by default. Use `--candidate` to run ordinary scoring on the same inputs. With a copy of the pre-flag executable, add `--reference PATH` to compare every byte of the real-repository JSON results. Build commit `ca0df97` to reproduce that reference. The runner writes the file scores, findings, directory totals and a compact report under `target/test-quality/`. Run the main panel through Nx with `npx nx run codeindex-workspace:eval-test-quality`.

## Development inputs

- **Real source.** The role eval provides pinned test-side development files. Another 220 files were sampled before scoring by hashing eligible paths in pinned public development repository trees. `sample_dev.py` reproduces the latter manifest using GitHub CLI for tree metadata. The regular eval only needs Python, the release binary, and access to public source on the first run. The path sample includes helpers and fixture data. Its denominator describes the selected test-like paths.
- **Independent reviews.** Two coding agents independently inspected the same files without scorer output. The two review files retain each judgment, its source evidence, uncertainty and applicability. File-level comparisons count agreement on whether a runnable test in the file has avoidable control flow or excessive executable verbosity. Compare the former with ordinary complexity, compound-condition and nesting findings and the latter with unit-length findings. These are file-level proxies; a finding can belong to a different function. Disagreements and unscored files have their own counts. These labels are developer aids with no claim to human maintainer endorsement.
- **Located conditional cases.** `conditional.jsonl` contains seven deliberately selected test units. The harness counts ordinary control-flow findings whose reported starting lines fall inside each unit. A single independent inspection supplied these exploratory expectations. Keep these counts separate from the file-level panel.
- **Paired controls.** The same scenario appears in six languages. One pair changes fixture layout, the other reorganizes conditional expectations. Each is run through the scorer in isolation at the same target path with its companion source. Treat the second pair's preference as a hypothesis. The manifest records the relationship between translations, and the verifier runs both versions against working code and two faults. [Paired fixtures](pairs/README.md)
- **Published detection labels.** Imported research labels retain their source definitions and granularity. The public panel covers five languages and omits conflicting decisions. The project-wide panel supplies broader language coverage. [Inputs, exclusions and baseline](published/README.md)

## Sealed evaluation

`blind.lock.json` reserves one separate public repository revision per profile by remote HEAD metadata only. Its 35 project names are disjoint from the development set. The development runner checks its allowlist before reading source and provides no switch to open a blind repository. The reserve contains repository identities, not inspected or labelled cases. The previously published role-classification test split cannot serve as a fresh blind eval.

Before any later blind run, freeze one candidate, rule-to-finding mappings, comparison margins and a sampling protocol. Use the same path-hash sampling within the locked revisions, keep related translations and file variants together, and report an empty or unsupported profile as a coverage gap. Review cases without scorer output before running either policy. A single repository per profile supports an initial cross-language comparison; it cannot establish a reliable population rate per language. [Scoring evaluation protocol](../../docs/scoring/evaluate-test-rules.md)

## Frozen outputs

The frozen run included 479 files from 41 repositories. Of these, 469 returned a numeric score, eight were present without a score and two were absent from the CLI output. The two independent reviews cover 105 files, three per profile. In the 38 scorable files both reviewers assessed as free of excessive executable verbosity, eight triggered a unit-length warning somewhere in the file. Neither of the two agreed verbosity positives triggered that warning. A file-level match can be incidental.

The baseline summary gives profile counts, review agreement and paired results. The compressed snapshot contains the exact repository outputs with file scores and individual findings. Its uncompressed SHA-256 is stored in the summary. The compatibility report records the CodeScene development metrics and finding panels. [Baseline](results/baseline.json) · [Repository outputs](results/baseline.json.gz) · [Compatibility results](results/compatibility.json)


## Suite-length comparison

Run `python3 benchmarks/test_quality/eval.py --candidate` and compare the candidate against the frozen snapshot. Seventeen of 479 files changed. The eight file-level length flags on the 38 scorable, agreed healthy examples cleared. Nine additional files changed without an agreed healthy-file label. All five localized verbosity leads still lack length findings. The two Go reviewers identified different methods in the same file; only the Swift cache method has two reviews pointing at the same method. [Located labels](verbosity_units.jsonl) · [File-by-file changes and timings](results/suite-length.json)

To reproduce the exploratory repetition comparison after loading development inputs, run `uv run --script benchmarks/test_quality/repetition_probe.py`. The probe looks for three distinct consecutive call names in source order repeated three times without overlap. It reached two of four hypothesized units. Both controls stayed silent. The Swift test's pattern runs across separate lists of lookups and expectations. This probe is evaluated separately from the CLI score.

## Suite nesting

Run `npx nx run codeindex-workspace:eval-test-nesting` to compare flat and four-level suites. Add `--indexed` to the Python runner to exercise the persistent index in an isolated temporary Git repository. The default runner uses a plain folder. Both exercise the public CLI and fail on a missing score, a missing warning, an extra warning, or an incorrect depth or region count.

The controls cover quiet suites, sibling groups, four real nested branches repeated twice inside one test or hook, and separate test and hook bodies with one nested region apiece. Each fixture describes source syntax. The panel measures scorer output without executing the framework. Counts distinguish language dialects and framework styles. The real-file evaluation uses the pinned development sources and its existing independent reviews. [Fixture generator](nesting/eval.py) · [Before and after, including every changed development file](results/suite-nesting.json)

## Conditional expectations

Run `python3 benchmarks/test_quality/oracle_controls.py` for the focused development controls. They exercise assembled expectations, observed counts checked against literals, accumulator resets, diagnostic-only reads, indexed verification, nested alternatives and Pester failure guards. The development comparison includes the three located conditional positives and the two previously flagged healthy files. Those five cases guided development. The paired timing measures a complete warm score query over the 477 captured files of the development corpus. [File-by-file changes and regression checks](results/conditional-oracles.json)

## Warm score benchmark

Run `python3 benchmarks/test_quality/warm_score.py --before /path/to/previous/cx` after loading the development corpus. Each binary uses an independent checkout, index and watcher. The harness compares full CLI output at each pair, times repeated watcher queries and standalone reopens, and measures the cost of starting with an empty index using an ordinary-policy compatibility query. The source files remain in the OS cache. [Measured results and phase profile](results/warm-score.json)
