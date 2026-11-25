# Agent benchmark

Run the same public Terminal-Bench 2.0 task with Codex in two fresh Docker environments. The baseline gets the task prompt. The treatment also gets the cx CLI and its checked-in introduction.

    python3 benchmarks/terminal_bench.py fix-git --model openai/MODEL --cx-binary /path/to/linux/cx --attempts 3

Use a model available to your Harbor Codex agent. The runner requires uvx, Docker, working Codex authentication, and an executable Linux build of cx matching the container architecture. Add `--dry-run` to inspect both commands without launching trials.

Harbor retains trajectories, commands, verifier results and logs under the ignored `target/terminal-bench/` directory. Each arm gets a JSON summary with reward, start and finish times, tool-call count, and Harbor's reported token and cost metrics. Missing metrics remain null. Compare usage alongside verifier reward. Raw runs stay local.

The other scripts here measure cx build performance and CLI parity.

## Maintainability score accuracy

Run `npx nx run codeindex-workspace:eval-score` to build the release CLI and evaluate it on all 5,000 public FORGE Python files. The harness downloads a commit-pinned JSONL and verifies its SHA-256. Only the source files enter the temporary repository. The expected scores and findings stay in the harness.

For an offline run or individual errors, use `python3 benchmarks/score.py --dataset /path/to/python_corp_bleud.jsonl --output results.json`. The report includes exact matches, absolute errors, and finding matches by rule and starting line. It counts unimplemented reference findings as misses. This is a calibration corpus for the scoring formula. Agreement on new projects requires a separate held-out evaluation.

Counting Python control suites separately from indentation inside expressions, and including short-circuit decisions and comprehension loops, reduced mean absolute error from 0.295 to 0.229 points on the ten-point scale. Of the 5,000 files, 4,169 scored within half a point (previously 4,044) and 1,834 matched within 1e-9 (previously 871). The largest error fell from 2.774 to 2.326 in that iteration. Accounting for Python method receivers and multiline conditions subsequently brought mean absolute error to 0.227 (1,854 exact matches and 4,173 within half a point). The similar-body detector matched 28 of 299 reference high-duplication findings at the expected line. These results measure Python only.

### Test-code identification

Run `python3 benchmarks/test_roles/eval.py` after a release build. This separate eval checks the role at labelled locations in public repositories against a filename-only baseline. [Dataset, splits and reproduction](test_roles/README.md).

### Multilingual score evaluation

Run `python3 benchmarks/score_eval.py prepare` to download pinned public inputs, verify their checksums and build the local corpus under `target/score-eval/`. Run `python3 benchmarks/score_eval.py coverage` for the frozen split counts and `python3 benchmarks/score_eval.py run --split dev` to evaluate the release binary. The same command accepts `train` and `test`. Reserve the test split for release evaluation. Raw source and generated reports stay local.

Each case records source bytes, language, upstream dataset or repository, family, source path, split, reference score and available findings. Real files use pinned source revisions or the corresponding Figshare archive. Competitive-programming examples and their refactorings stay in training because the original Python set informed scorer development. Real projects stay in a single split. The collector excludes identical source shared between splits after normalizing line endings and surrounding whitespace. This check cannot detect every near-clone. The lock records input hashes, membership, labels and counts.

Reports show each language separately and weight scored languages equally in the macro average. They also separate perfect scores and null labels. A null Repowise label means its collector returned no score and can include tool errors. Score error covers numeric pairs only. The path-role breakdown is a naming heuristic; a file can contain both tests and production code. Reference agreement by path does not measure whether a warning was useful. Assess that separately with human-reviewed warnings and matched unflagged examples across test cases, shared helpers and production functions.

The pinned sources are the [FORGE originals and refactorings](https://github.com/codescene-research/ai-friendliness-forge-2026), [SCAM Java and C++ examples](https://github.com/codescene-research/test-generation-scam-2026), [Repowise score caches](https://github.com/repowise-dev/repowise-bench), and [ICSME scores](https://github.com/codescene-research/maintainability-benchmarking-icsme-2024) joined to the [source archives](https://figshare.com/articles/dataset/A_Software_Maintainability_Dataset/12801215).

On the frozen development split, distinguishing executable control suites from data literals, separating callback bodies and correcting signature and guard handling reduced mean absolute error from 0.479 to 0.249 across 5,089 numeric pairs. The count within half a point rose from 3,347 to 4,128. Go accounts for 4,710 of these pairs. On training data, mean error averaged equally over nine languages fell from 0.471 to 0.258. These are tuning results. The blind split was reserved.

### Finding-level development panel

Build the release binary with `cargo build --release --locked`, then run:

    uv run --script benchmarks/score_findings.py prepare
    uv run --script benchmarks/score_findings.py run --output target/finding-eval/results.json
    uv run --script benchmarks/score_findings.py controls --output target/finding-eval/controls.json

The panel contains 12 files from each of two pinned repositories per language, across 23 languages. Selection uses a stable path hash and samples files with and without reference findings where available. Preparation checks the frozen panel digest and rejects repositories reserved for blind evaluation. Reports retain both sets of findings per file. Matches require the same rule and starting line. Lizard and independent syntax-tree measurements supply the reference counts. These measure agreement on complexity, parameter counts and unit length where supported. Thirteen additional profiles have separate constructed boundary tests. Those tests establish expected behavior on examples only.

For published reference findings, run:

    python3 benchmarks/score_published_findings.py prepare
    python3 benchmarks/score_published_findings.py run

The [3SX campaign](https://github.com/codescene-research/3sx/tree/fea568087f13ddb01ac6c53b5945265ed5ef7eeb/docs/refactoring/tasks) lists selected C targets. Unlisted findings cannot be classified from these reports. The [CodeScene editor fixture](https://github.com/codescene-oss/codescene-vscode/blob/281bfd88337a95857dce242726df4c835298db1f/src/test/fixtures/gc.cpp.expected.json) supplies expected findings for one large C++ file. Both collectors use public pinned inputs and run locally without credentials. Neither evaluates blind repositories.

On the next development pass, fixes for constructors, conditional compilation, shell case arms and nesting brought rule-and-line matches on the balanced panel from 428 to 446 out of 559, with 73 extra findings in both runs. Recalibrating the contribution from independent deeply nested functions reduced numeric development error from 0.249 to 0.226 points per file, and from 0.307 to 0.260 when weighting the nine scored languages equally. Only two development files have numeric C labels and three have numeric C++ labels. Use the separate finding panel for more source coverage. On the 5,000 FORGE training files, mean error stayed near 0.227. The blind split remains reserved.

### Ripgrep compatibility

Run `python3 benchmarks/rg_upstream.py target/release/cx /path/to/ripgrep /tmp/cx-rg-results` with ripgrep checked out at tag `14.1.1`. The harness builds upstream's default-feature integration suite, runs the same runner against both executables and saves both logs. A nonzero exit means at least one suite failed. Repeat with a separate results directory and append `--pcre2` to run the PCRE2-enabled suite. Use a system with the PCRE2-8 shared library installed for that profile. For Git worktrees, plain directories, and terminal output, run `benchmarks/cli_parity.py` with the same ripgrep version as the oracle.

### Test maintainability

The [test-quality development eval](test_quality/README.md) freezes ordinary scores and findings, measures research-labelled patterns and paired controls, and reserves separate repositories for blind evaluation.
