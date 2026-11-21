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

## What a language must establish

A language registration pairs its extension and dialect selection with its declaration rules, dependency discovery, binding rules and capability-specific fixtures. Freeze each supported mode in the archived language registry, including the distinction between declaration-only and semantic operations. For adding a language, run the same generic capture, update, pin and query suites with the new registration. Exercise same spelling in two scopes, missing then introduced definitions, an unrelated edit, malformed input, and each supported form of import, dispatch or generated binding. Specify paths and byte ranges for all expected targets and the reason for every partial or unsupported result. A new extension using an existing grammar still needs a fixture if it selects a different syntax mode or project context.

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
