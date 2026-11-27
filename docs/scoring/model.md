---
page-type: reference
---
# File score calculation

The score is a deterministic function of the findings saved for one file. Each rule has an inclusive threshold. Values below threshold contribute zero. The line count used to weight the file in a directory summary is separate from the unit-length findings.

| Finding | Threshold | Input to the penalty |
| --- | ---: | --- |
| Branch complexity | 9 | Sum of reported complexities, C |
| Unit lines | 70 | Sum of reported lengths, L |
| Compound conditions | 2 | Sum of reported logical-operator counts, Q |
| Nested regions | 2 | For each reported count v, set r = 1 + 0.5 × max(0, v − 3). Sum min(r, 4 + 0.5 × r) across units, B |
| Nesting depth | 4 | Maximum reported depth, D, and number of depth findings, N |
| Parameters | 5 | A = 2 if present |
| Similar body (code lines) | 10 | U = 4 if present |

For Rust, each arm of a written `match` expression contributes to branch complexity. A `=>` used to declare a macro rule or a wire tag contributes no branch by itself. A `match` written inside a macro body still contributes its arms.

The Rust parameter count omits bare `_` parameters in functions declared with a foreign ABI, such as `extern "C" fn`. Such slots can be required by the caller even when the function ignores them. Named parameters, including names beginning with an underscore, still count. Functions using the Rust ABI count wildcard parameters as well. An implicit `extern fn` uses the C ABI. The scorer also retains wildcard parameters when the written ABI string contains escapes.

Empty sums, the maximum over no findings, and absent penalties are zero. Set F to 2 if C or L is positive and to zero otherwise.

```text
penalty = max(0.18 × C, max(F, 0.036 × L) + Q)
        + B + D + max(0, N − 1) + A + U
score   = 1 + 9 × 0.9825255625212619 ^ penalty
```

A single parameter finding gives a penalty of 2. An additional parameter finding leaves that term at 2. An additional unit with reportable depth increments N and can also raise D. The explanation retains each finding even when two findings contribute to the same term.

For reproducible evaluation in a checkout, build with `cargo build --release --locked`. Run `python3 benchmarks/score_eval.py prepare` followed by `python3 benchmarks/score_eval.py run --split dev` for numeric development errors. For finding-level comparisons use `uv run --script benchmarks/score_findings.py prepare` followed by `uv run --script benchmarks/score_findings.py run`. The latter compares independent reference findings by rule and starting line.
