---
page-type: reference
---
# Invocation contract

The CLI translates a process invocation into one application request and renders one result. Help and version work outside a repository. Query commands preserve the archived command vocabulary.

| Invocation after `cx` | Application request |
| --- | --- |
| `search TEXT`, `content TEXT`, `regex PATTERN` | Ranked text, literal content or regex content |
| `path TEXT`, `path-exact PATH`, `path-regex PATTERN`, `glob PATTERN` | Substring, exact, regex or glob lookup over repository-relative paths |
| `symbol NAME` | Declarations with this spelling |
| `definition FILE LINE COL`, `references FILE LINE COL`, `callers FILE LINE COL` | Relationship query at a one-based UTF-16 position |
| `section PATH[#ANCHOR] [--parent]`, `backlinks PATH[#ANCHOR]` | Section navigation or written backlinks |
| `status` | Coverage and state of the refreshed generation |

## Request lifecycle

No arguments, `help`, `--help` and `-h` list commands. Use `cx help COMMAND` for usage and an example. Help works outside a repository. Reject empty search, symbol and path terms, whitespace-only ranked queries, invalid regexes and malformed globs as usage errors. Parse arguments before requesting build resources. Preserve the working directory for repository discovery and interpreting file arguments. Check arity, supported switches and positive numeric coordinates. Retain the distinction between a missing fragment and an explicitly empty fragment. Pass one typed request to the application. A query obtains its pin through refresh; status describes that same generation.

A successful query writes one machine-readable result to stdout, including coverage and truncation when applicable. A valid empty search succeeds. Process exit codes are as follows. usage errors exit 2; repository, refresh, query or output failures exit 1; success exits 0. Diagnostics go to stderr.

## Acceptance

From a nested directory, the same location must resolve to the same file as an equivalent invocation at the repository root. A malformed position fails before opening the repository. An unsupported semantic operation yields explicit capability status. A broken stdout pipe cannot report successful delivery.

## Addresses and machine output

A relationship file argument may be absolute within the observed repository or relative to the invocation directory. Resolve it to a path inside the pin. Use repository-relative paths as the keys for path searches and Markdown handles. In a Markdown handle split on the first literal `#`. Escape literal percent and hash in each handle component as `%25` and `%23`. Decode those handle escapes exactly once. Thus `a%23b.md#setup` selects the heading in `a#b.md`. Bare `a.md` selects all destination fragments for backlinks; `a.md#` selects only the empty fragment.

Preserve a JSON object per command response, except for the documented null parent. The frozen indexed query payloads supply the following compatibility fields:

| Family | Shape examined by compatibility checks |
| --- | --- |
| Search, content, regex, path, glob, exact path, symbol | `root`, `generation`, `mode`, `coverage`, `relationship_status`, `truncated`, and `hits`. Each hit contains only fields belonging to its query type. |
| Relationships | `root`, `mode`, `generation`, `indexed_document`, `operation_status`, `position_encoding`, `relationship_status`, plus sibling top-level fields `result` (array), `truncated` (boolean), `reused` and `engine`. The new `contexts` and `evidence` fields explain contextual answers. |
| Section and backlinks | Section returns the section object, with exact source body and ancestry. Backlinks echoes `target` and supplies `hits` and `truncated`. Each backlink points to the source occurrence and gives the normalized target handle. |
| Status | Report the repository root, checkpoint generation, product generation, indexed HEAD, count of byte-identical committed text files, reuse, source bytes, and path and text counts. Each of the six entries in `products` gives its publication state and extent size in bytes. The separate `relationships.status` field describes semantic coverage. |

Archived relationship status values distinguish unsupported operations, unindexed input, unresolved positions and resolved positions. Preserve the distinctions in native answers. The archived CLI could start a live language server for unindexed files, returning a separate server-shaped payload. A successful legacy live answer is a coverage obligation. Verify a corresponding native answer from captured inputs or record the case as an open parity gap. The design requires every new answer to come from the generation pin.

## Relationship response schema

Every semantic query exposes the following members. JSON object member order is insignificant. Hit order, presence of nulls, and array order are significant.

```text
root: absolute worktree path                  generation: integer
mode: "definition" | "references" | "callers"
reused: boolean                              indexed_document: boolean
engine: "codeindex" | null                    position_encoding: "one-based UTF-16"
operation_status: "resolved" | "unresolved" | "unsupported" | "unindexed"
relationship_status: "indexed" | "partial"
truncated: boolean                           result: [Hit]
contexts: [ContextResult]                    evidence: [HitEvidence]
Hit = {path, line, col, symbol}               // definitions and references
    | {path, line, col, symbol, caller}       // callers
ContextResult = {context, coverage, reason, bindings}
Binding = {role, certainty, targets}          // one entry in bindings
HitEvidence = {hit, context, role, certainty, target}
// Hit and HitEvidence may carry locations. Binding may carry target_locations.
```

`path` is repository-relative for repository hits. An external location observed by a successful reference provider uses its absolute source URI in the same string field. Positive integer line and col identify the start. `symbol` names the result target for definitions and references, and the enclosing symbol for a caller. For caller hits, `caller` is the owner's display spelling, which can differ from the identity in `symbol`. An absent owner or display name is null. For callers the hit is the call-site location. Repeated sites for the same owner remain distinct. The shared result array keeps the ordered union of visible hits across contexts.

A context is a public opaque string owned by the language registration. It must distinguish semantic configurations that differ and reproduce the same key for the same logical context across rebuilds, worker orders, and equivalent checkouts. Use the registration's repository-relative project identity and normalized semantic configuration to form that key. Generation-local integer ordinals and absolute checkout paths cannot serve as public keys. Serialize context entries in UTF-8 byte order of that key. The [context encoding and coverage reason vocabulary](../../domain.md#public-context-keys-and-coverage-reasons) fix their exact wire values. A symbol string names the language's identity for that site, including local ownership and context where needed. Preserve the archived public symbol label for a legacy-observable hit, regardless of the native producer's name. A context evidence target can further distinguish identities where the old label alone is ambiguous.

Coverage is `complete`, `partial` or `unsupported` for the requested operation and searched scope. Use null reason for complete coverage and a nonempty reason string otherwise. Bindings at the selected position carry role `definition`, `reference` or `call_target`. Certainty is `exact`, `candidates` or `unresolved`. An unresolved binding has an empty targets array. A candidate group retains its full sorted array of possible symbol strings. Several independently proven targets yield separate exact bindings. Coalesce identical binding groups within one context. Sort by role, certainty and target sequence using UTF-8 byte order.

The evidence array connects each returned hit's zero-based result index to the context, role, certainty and target symbol of the supporting incoming or outgoing edge. For a callers row with a null owner, target still identifies the called symbol. Deduplicate by (hit, context, role, certainty, target). Order by numeric hit index followed by those four strings in UTF-8 byte order. Union and sort location entries within each tuple; its locations belong only to that supporting context and target. Build evidence only for retained hits. A fully covered context with no callers still has a context entry and can have an exact binding at the queried position. A partial context that returns no hits retains its coverage and reason. The 50-hit cap applies to distinct visible hits before evidence expansion.

Apply status rules in order. A registered file profile that categorically lacks the operation yields `unsupported`, including callers for HTML, CSS, Protobuf, SQL and Starlark. Otherwise a document without semantic input yields `unindexed`. A nonempty set of applicable contexts all lacking the operation yields `unsupported`. A selected binding with a target yields `resolved`. All other indexed cases yield `unresolved`. Set `indexed_document` to whether the document has semantic input; `engine` is `codeindex` when true and null otherwise. Empty results can occur with each status. Preserve per-context coverage even when another context supplied hits.

### Legacy provider answers

Convert each successful archived provider location into the native schema using the captured document URI and the start of its UTF-16 source range. Add one to the old zero-based line and column. For a location link use its target selection range. For incoming calls use each incoming call's source URI and call-site range from `fromRanges`, plus the owning symbol when known. Legacy provider identities that were not present in the original answer may remain null on the visible hit. Project the [pinned navigation group](../../domain.md#navigation-evidence-for-additional-locations) into an optional `locations` array on each supporting evidence tuple. The same field on a visible hit is the sorted union across its evidence. An entry is `{uri, start, end, kind}`, where start and end are [one-based line, one-based UTF-16 column], end is exclusive, and kind is `selection`, `target_range`, `origin_selection`, `call_site` or `caller_range` according to the original evidence. For ordinary LSP Location use `selection`. For a LocationLink preserve both target ranges and its optional origin selection at the referring URI. For CallHierarchy preserve available caller and target item selection and body ranges using their own URIs, plus each call site in the caller. Group a caller with zero observed call-site ranges as a hit at its selection range with no `call_site` entry. A target item with zero incoming calls contributes selection evidence to its context binding and zero incoming hits; use the optional `target_locations` field on a binding, an array of `{target, locations}` ordered by target's UTF-8 bytes with one entry per target that has observed ranges. This preserves which candidate supplied each range even when a candidate has no hits. Sort distinct location entries by URI (UTF-8 bytes), start and end (each numeric line then column), then kind (UTF-8 bytes). Omit `locations` when no extra range observations exist. Compare URI and range observations in conformance as well as normalized visible hits. Keep any inputs needed for these observations inside the captured generation contract. Every successful frozen live result is required in normalized form, including external target locations; missing equivalents fail conformance. No normal query starts a live provider.

### Wire fixtures

For a single source use binding certainly to A under context `debug` and B under `release`, expect two context entries with `exact` and one target each. When both contexts give the same visible start with different observed end columns, expect one result with two evidence records. The hit contains both ranges and each evidence record contains only its own range. In a single context with uncertain alternatives A and B, expect one `candidates` binding with both targets. Deleting a target while it remains visible to a reader holding the old pin must change only responses from the new pin.
