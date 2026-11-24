---
page-type: reference
---
# Comparing implementations

Two implementations can assign different private ordinals and still answer identically. This contract identifies independent witnesses for the answers, the work required to obtain them, and the physical choices that need measurement.

## Query witnesses

| Request | Independent witness | Disagreement to catch |
| --- | --- | --- |
| Exact path, substring, glob | Apply the request predicate exhaustively to eligible paths in final public order. Exercise binary files, ignored tracked paths and worktrees. | A membership filter drops a real hit or leaks a retired file. |
| Literal, regex | Exhaustively match visible pinned source in path order. Compare full locations and truncation, including Unicode, cross-line patterns and patterns with no mandatory grams. | A filter treats an optional branch as mandatory or changes the match unit. |
| Ranked search | Query the archived release with the same visible chunks and fields. Compare ordered hits, floating scores where exposed, excerpts and section ancestry. Include repeated terms, punctuation, field boundaries, overlap and ties crossing the limit. | Matching the hit set hides changed ordering or an incorrectly highlighted excerpt. |
| Named symbols, definition, references, callers | Look up every named declaration and every distinct declaration and use position in the fixture. Compare site ranges, operation, role, target identity, context, certainty, capability and truncation. Include queries whose correct answer is empty. | Resolving two different declarations to the same spelling fabricates a caller. An exact answer downgraded to partial loses coverage. |
| Markdown section, parent, backlinks | Compare byte slices of the input and explicit lists of inbound link locations. Include root, skipped levels, duplicate headings, dangling fragments, rename, and missing versus explicitly empty fragment. | A backlink vanishes when its target is removed, or a parent query cuts off its last descendant. |
| Status and failures | Observe the pin used for a subsequent query over unchanged inputs. Inject invalid coordinates, permission errors, short writes and an incompatible manifest. | Success after a partial write, or an unsupported operation presented as exhaustive. |

Compare the uncapped logical relations in both directions as sets or multisets according to their keys. Querying only known sites misses fabricated sites, and CLI truncation can hide a missing 52nd caller. Probe each boundary, an interior offset, a no-site position, a surrogate interior, and each unsupported operation. Compare public hit order and the truncation bit separately.

Run `cargo test --locked` for self-contained fixtures. For performance comparisons use identical checkouts, separate empty stores and alternating run order. Verify results after durable reopen.

## A worked cross-index witness

Use an otherwise empty working tree with two eligible files. In the following JSON strings, decode each escape once to obtain the exact UTF-8 file contents.

```json
{
  "guide.md": "# Guide\n## Setup\n🙂 needle\n### Child\nagain\n",
  "refs.md": "[Setup](guide.md#setup)\n"
}
```

The archived Markdown release gives the following results on these bytes.

| Command | Expected projection |
| --- | --- |
| `content needle` | One hit in `guide.md`, line 3, byte column 6. The titles are `guide.md`, `Guide`, `Setup`. |
| `search needle` | One chunk, lines 2–3, score −0.8104588229790644. The excerpt is `"## Setup\n🙂 [needle]\n"` in JSON notation, with the same three breadcrumbs. |
| `section guide.md#child --parent` | Handle `guide.md#setup`, bytes [8,45), parent `guide.md#guide`. The body includes the entire Child section through its final LF. |
| `backlinks guide.md#setup` | One hit from `refs.md#`, line 1, byte offset 0, with the root breadcrumb of the referring document. |

## How to test one language

Apply these five steps to each language in the live registry.

Keep each synthetic case in this repository under `tests/fixtures/languages/<language>/<case>/`, for example `tests/fixtures/languages/python/super_call/`. Put the project files in a `repo/` subdirectory and the expected answers beside it. The harness copies `repo/` into a temporary directory, runs `git init` there, and runs queries and edits in that copy. For public cases, commit the repository URL, pinned revision, configuration, queries and expected answers. Fetch the checkout separately.

1. **Choose one query and one language feature.** Write the question in plain English. For example, “Which method does this inherited call refer to?” Repeat for symbol lookup, definitions, references and callers. Record the expected status for partial or unsupported operations. Give a dialect its own case when it changes the answer.
2. **Make the smallest repo that distinguishes right from wrong.** Include the intended declaration, an unrelated declaration with the same name, and a use that can refer to only one of them. Adapt the names to the language, such as methods, shell functions or build stages. For a declaration-only feature, include a same-spelling occurrence in ordinary text.
3. **Write the answer before running cx.** Save the exact query, expected paths and source ranges, expected coverage status, and the evidence. Use a pinned language tool that answers that question or two independent reviewers who agree on the source locations. For “find all references,” account for every use in the test scope. Keep unresolved disagreements visible as unverified.
4. **Run the query, then change one fact.** Compare a fresh build and a reopened index with the answer key. Write down how the answer should change if the use moves to the other declaration. Apply that edit and compare both the updated index and a clean build with the new answer key. For declaration-only features, rename the declaration and check the old and new names.
5. **Check the feature on real code.** Pick a public repo and pin its commit, project root, configuration and reference tool version. Select the use or declaration independently of cx. Apply the same answer-first comparison. A verified call site proves one reference. Only a complete independent list can verify an “all references” answer.

Run all language checks from the repository root with Python 3.9+, Git, Node/npm, uv and a Clang installation with system headers:

```sh
cargo build --release --locked
python3 tests/discover_languages.py --public
```

Omit `--public` for the offline synthetic suites. Read `target/discovery/suites.json` for suite outcomes and logs. Detailed reports live in `target/language-*.json`. To isolate one language, run `python3 tests/language_conformance.py --language python --public`. The full run continues through failing suites and exits nonzero if any suite fails or lacks a required tool.

A case's `checks.tsv` contains one operation, subject and expected location list per line, separated by tabs. Positions use one-based UTF-16 columns. Use `-` for an empty list. Append `|name` to an expected caller site to check its owner, or `|kind` to a symbol site to check its declaration kind. Relationship rows can add an expected operation status such as `resolved` or `resolved/partial` to check context coverage as well. A fifth field of `contains` checks only independently witnessed members of a reverse set. Prefix an operation with `?` for a probe of behavior outside the documented capability. Its differences appear in the report separately from required failures.

Keep replacement files in `edit/`, deleted paths in `delete.txt`, and post-edit answers in `edited.tsv`. The runner compares the edited checkout against both the existing store and an empty store. Public cases use `public.json` to pin the URL and commit.

For another discovery pass, select one unanswered question for each registered profile. Write a case containing a real use and the nearest misleading use. Change only the condition that should affect the answer, such as ending a scope, entering a comment, escaping an interpolation or selecting a parent method. Run the pinned public cases alongside the new examples. Record the smallest reproducer and confirm the expectation independently before listing a defect. Count wrong answers separately from questions whose semantics still need agreement.

The runner also deletes and restores the synthetic sources for names that passed their initial lookup. It checks that those names disappear and return at their original locations. For relationship hits it checks that every hit has evidence for the selected binding, operation and context. The per-profile verdict in the JSON applies to the sampled queries.

Run `python3 tests/language_paths.py` to check filename and dialect routing. Run `python3 tests/language_named_references.py --public` to check the user-facing name lookup against labelled fixture declarations and reference sites.

**When to stop expanding a test family.** Before running a batch, record the language feature, the wrong answer it could produce, the independent witness and the applicable profiles. Count distinct confirmed causes per batch. Keep repeated manifestations, unsupported features and errors in the answer key separate. Two batches without a new cause trigger a challenge on a different mechanism or on independently labelled public code. If that challenge finds a cause, follow it through adjacent constructs and edits. Stop that line of discovery once those checks also stop adding causes. Record exactly which inputs and operations the conclusion covers.

The held-out repository-transition tests use the same written declarations across all profiles and vary the result cap, tracked membership, file identity and line endings. The Python public-code oracle compares symbol locations and kinds with CPython's AST and token stream. For statically named calls it checks bytecode global reads, straight imports and unambiguous written definitions in the pinned package. Function parameters, rebinding and ambiguous same-line loads can invalidate that inference. The oracle has an independent negative control for them.

**Diagnosing C/C++ differences.** The public Clang suite records the exact compiler arguments, written source anchors, the sample selected before querying cx, and censored results. For direct calls it also checks the reverse edge, excludes callees independently bound to a different target and leaves unrelated references ungraded. After the public run, use `uv run tests/language_c_residual_controls.py` for isolated annotation and return-type edits, or `uv run tests/language_compiled_residuals.py` for ranked samples of unresolved sites. Credit a cause only when the source intervention preserves Clang's locations and restores the Codeindex answer. An unchanged compiler target and a still-wrong Codeindex answer leave that case open.

**Verdict.** A wrong answer fails. Missing evidence or a missing run leaves the case unverified. A language passes when every required case passes. Removing a required case or narrowing its scope needs independent review.

**Rollout.** List all live languages and their promised operations first. Keep existing frozen provider answers. Exercise the runner on three different behaviors: Python inheritance, an import redirected in another file, and a Procfile process declaration with unsupported callers. Deliberately drop a hit, return the wrong target, and claim a partial answer is complete. Each must fail. Then apply the procedure to every remaining language.

**Execution.** Run the full pinned suite before release. The synthetic suite can also run offline. Shared tests also cover Unicode positions, two hits on one line, malformed source, unrelated edits and targets introduced, renamed or deleted. Use the Markdown witnesses above for sections, breadcrumbs and backlinks.

## Selecting physical designs

Keep semantics and the input fact stream fixed while comparing candidates. Use an empty index, an identical checkout, the same cache preparation and the same durable reopen boundary for each paired run. Alternate order. Report per-repository paired times and spread for each input. Measure a rare miss, common hit, ranked multi-term query, first query after reopen and one-file edit. Record peak resident and reserved bytes, temporary bytes and final disk bytes. Reject candidates that lose query parity, durability or bounded progress.

| Material choice | Competing designs | Measurement that chooses |
| --- | --- | --- |
| Source access | Buffered read into owned capture, or direct capture into private snapshot backing with bounded mapped windows | End-to-reopen time, copied bytes, faults, peak memory and post-build read latency across small and oversized files. Both must survive checkout replacement. |
| Parsing and lexical work | Fused cursor, or independent consumers over the same immutable backing. Compare scalar and SIMD kernels on the same source and adversarial boundaries. | Count decoded bytes, repeated normalization, worker CPU and the serial tail. Keep the simpler baseline when timings overlap and the alternative buys no material reduction in bytes or peak use. |
| Binding | Recompute dependents of any changed input, or retain exact dependency projections and compare revisions | Measure evidence size and comparison cost against eliminated rebinding on real edits. Require identical target and reverse-edge changes for both. |
| Postings | File-major tiles, bounded key partitions, or sorted runs | Charge construction, fanout on common and rare keys, update cost, and transient bytes. For equivalent timing prefer materially less traffic or peak memory. |
| Scheduling | Local FIFO with stealing, or priority for ready work that unblocks dependents | Compare idle time with admissible work, queue cost, critical path and durable completion at multiple worker and memory limits. Check one-worker progress in both. |
| Source coordinates | Dense line starts, newline bitmap with rank, or sparse checkpoints | Compare bytes per text file and lookup cost for random hits and repeated hits on a long Unicode line. Require identical public locations. |

For the first comparison use buffered capture with private backing for oversized files, shared scalar extraction, dependency projections defined by the language profile, bounded count-and-place for exact postings, sorted runs for ranked positions, local queues with reserved drain credit, and dense line starts. These establish reproducible controls. A candidate that exceeds the budget must use its spill path in the comparison.

A faster fresh build that slows a common query has a workload-dependent tradeoff. Report the extra query cost and the number of queries per refresh that erase the build gain. Where the workload does not decide, retain an explicitly named baseline and record the crossover. Approximate the optimistic build floor from required input and output bytes, useful CPU work at measured aggregate throughput, and the longest non-overlapping dependency chain including durability. Take the maximum of overlapping resource bounds. Adding every stage duration double-counts concurrent work.
