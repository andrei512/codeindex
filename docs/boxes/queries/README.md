---
page-type: landing
---
# Query execution

A query consumes exactly one generation pin and one typed request. It owns the meaning of a result, including coverage, ordering and truncation. Index readers provide candidate records.

| Request | Required answer |
| --- | --- |
| Path, glob, exact path | Matching visible paths in path order, including eligible binary files. |
| Literal or regex | Verified source matches with line and byte column. A regex without a mandatory literal examines all eligible text. |
| Ranked search | Chunks scored with generation-wide statistics, with deterministic ties, snippets and Markdown heading ancestry where applicable. |
| Symbol lookup | Matching declarations with kind, path and source coordinates. |
| Definition, references, callers | Interpret the input as a one-based UTF-16 position. Preserve all equally shortest containing spans per applicable analysis, valid targets, optional caller owners and applicable coverage. |
| Section or parent | The requested heading's subtree and ancestry; the root has no parent. |
| Backlinks | All written links to the requested document, or to its exact normalized fragment when one was specified. |

## Edge cases that change an answer

- **Context-dependent callers.** Select all shortest containing spans independently within each applicable syntax analysis. Resolve within each applicable context before coalescing visible hits. By default, return shared hits once and preserve which contexts support different answers or have incomplete coverage. Two individually certain bindings under different configurations are distinct from ambiguity within one configuration. Coverage of the searched callers and result truncation are independent.
- **Markdown section bounds.** In a parent section with a child heading, opening the parent returns its subtree. Ranked chunks belong to the deepest owning heading; its direct text ends at the next heading. The heading order, parent and subtree end support both reads. Compute breadcrumbs by following parents for returned hits.
- **Written link addresses.** A document backlinks request includes all fragments. An explicit empty fragment selects links with an empty normalized fragment. Preserve this distinction even when the destination document is absent.

The archived CLI caps search and relationship lists at 50. Determine truncation after verification, deduplication where required, and ordering. The [CLI response contract](../cli/invocation.md#relationship-response-schema) defines the context and evidence arrays.

## Topics

### Design

- [Evaluating a request](evaluation.md) — Specifies reader order, verification, coordinates and result selection.
