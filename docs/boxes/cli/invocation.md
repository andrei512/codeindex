---
page-type: reference
---
# CLI invocation contract

This is the design target for the native Codeindex CLI. It defines what an agent can type, what bytes it can read back, and how it can tell a miss from an incomplete answer. The compatibility target and the current implementation are tracked separately below.

## Four forms an agent needs

| Question | Invocation | Example stdout |
| --- | --- | --- |
| Which lines match this regular expression? | `cx PATTERN [PATH…]` with ripgrep arguments | `fn retry_delay() {}` when stdout is piped and one file was specified |
| Which passages discuss this topic? | `cx -k 'retry policy'` | `docs/retries.md:18:Retry failed requests up to three times.` |
| Where is this name declared? | `cx --symbol retry_delay` | `src/retry.rs:12:pub fn retry_delay() {` |
| Where is this declaration used? | `cx --reference retry_delay` | `src/client.rs:47:    retry_delay();` |

Codeindex executes every search natively. Use the installed ripgrep executable only as a test oracle. Repository discovery, pinning and refresh belong to Codeindex. An indexed query observes a single generation.

## Ordinary text search

Inside a Git working tree, the reference for ordinary search is ripgrep 15.2.0, with the same arguments, working directory, locale, configuration, available regex features, input bytes and terminal descriptors. This covers argument interpretation, match selection, prefixes, colors, context, output formats, errors and exit status. For example, a pipe and a terminal can produce different output for the same command. A file operand and a directory containing just one file also have different prefix rules. `cx --json PATTERN` uses ripgrep's event stream. Help and version identify the actual `cx` binary and describe its extensions.

Bare words are patterns, including `help`, `intro`, `symbol` and `references`. Option operands remain opaque. Both `cx -e --reference` and `cx -- --reference` search for the text `--reference`. No arguments follow ripgrep's no-pattern behavior. Repository discovery selects storage. Inside Git, queries share a durable index rooted at the worktree. Outside Git, a line search scans its operands directly, and indexed queries use the current directory as their root and build their query products in process memory. Each invocation starts fresh. No watcher runs in this mode. Plain-folder traversal respects nested `.ignore` files, skips hidden entries by default and skips discovered symbolic links. Help, version, and piped text search work anywhere.

The ordinary search's file selection follows the query. For example, an ignored tracked file can be present in the shared index yet excluded from a default text search. An explicitly named ignored file can be absent from the index yet required by the query. Apply a query-local eligibility filter, use the pin for available bytes, and capture additional requested input with Codeindex's own reader. Stream stdin for `-` and piped-input invocations according to ripgrep's rules. One-off inputs do not expand the persistent index. Whole-repository ranked and semantic queries retain Codeindex's indexed-file eligibility.

## Indexed modes

A leading `-k` (alias `--keywords`), `--symbol` or `--reference` selects one indexed mode. Put the selector first, then its operands, then optional display switches. This keeps `cx --json PATTERN` unambiguous. A selector takes its next operand literally, including a dash-prefixed term. `--reference` takes either one unique name (including an indexed qualified method) or `FILE LINE COL`. An unknown name yields zero hits, an unresolved-name diagnostic and exit 1. A known declaration with zero recorded uses also exits 1. Its status is resolved, and partial coverage earns a warning. Ambiguous names exit 2 and direct the caller to `--symbol`.

| Rule | Contract |
| --- | --- |
| Scope and paths | Search the indexed repository. Text paths are relative to the invocation directory, including `../` when needed. A reported location can be reopened there. JSON paths remain repository-relative. |
| Match lines | Emit `path:line:source` even when piped. Copy the full source line including indentation, omit its original line terminator, and terminate each record with LF. No scores, highlighting or prose by default. |
| Coordinates | Line and column start at one. `--column` adds the UTF-8 byte column before the source. The public positional `--reference FILE LINE COL` takes these same byte coordinates. Reject columns outside the line or inside a multibyte character. Appending `--json` leaves the public byte-column input unchanged. The legacy `--cx references FILE LINE COL` consumes UTF-16 columns. Both JSON forms preserve the historical UTF-16 `col` output field. |
| Ranked anchor | Return one anchor per ranked passage, in existing BM25 order (ties by repository path bytes then passage start). Choose the line containing the starts of the most distinct matched body phrases. Break ties by earliest line. Use the first participating start on that line as the column. A metadata-only hit uses the passage's first line and column one. |
| Semantic anchors | Use the declaration or reference span's first byte. Sort by repository path bytes then byte offset. Render one match line per source line, using the leftmost retained site for its column. JSON preserves distinct sites. |
| Context | Accept `-A N`, `-B N`, `-C N` and attached counts such as `-A24`. In semantic modes merge touching or overlapping windows in a file. In ranked mode render each passage's window independently in rank order. Clip windows to file bounds. Use ripgrep context separators (e.g. `src/lib.rs-11-source`), with `--` between disjoint groups when context is requested. Each match keeps its match prefix. |
| JSON | Append `--json` for the indexed object described below. Preserve scores, excerpts, breadcrumbs, evidence and semantic coordinates where applicable. Text-only switches with indexed JSON are usage errors. |
| Bounds | Retain the existing first 50 ranked passages or semantic sites before line grouping. Mark truncation only on evidence of another hit. Context lines consume no hit slots. |
| Streams and status | Compact stdout contains only result records and context separators. Send one stderr notice per applicable condition (partial coverage, truncation, unresolved name or position). Exit 0 for one or more hits, 1 for a valid empty answer, 2 for an invalid or ambiguous request, unsupported operation, unindexed input position or execution failure. Empty partial results still emit the coverage notice. Indexed JSON carries these statuses in its payload when a query was evaluated. A valid empty JSON answer still emits its object. |

## Existing scripts and discoverability

The target reserves `--cx` as an explicit namespace for existing operations. For example, migrate `cx callers NAME` to `cx --cx callers NAME`, `cx section HANDLE` to `cx --cx section HANDLE`, and scripts using `cx symbol NAME | jq …` to `cx --symbol NAME --json | jq …`. The namespace retains content, regex, path, path-exact, path-regex, glob, symbol, definition, references, callers, section and backlinks, with their existing JSON payloads, empty-result success convention and UTF-16 relationship coordinates. Keep `cx --cx intro` for the short agent guide. List the four primary forms in `cx --help`. Removing positional verb dispatch and changing public positional reference columns require migration notes when shipped.

## Implementation gaps and acceptance

The native implementation includes indexed selectors, source anchors, context windows, separate output streams, the advanced namespace, and common ordinary search switches. Differential fixtures cover pipes, terminal prefixes, stdin, context, and explicit paths. Full ripgrep parity is open, including binary and non-UTF-8 input handling, multiline and PCRE2 search, ripgrep configuration, file-type selectors and ignore overrides. Ordinary JSON emits UTF-8 search events and native search statistics. Run `python3 benchmarks/cli_parity.py target/debug/cx` for the current executable comparison. The current default walk uses Codeindex membership, with a hidden-path filter. Ignored tracked files can still differ from ripgrep.

- Run the same fixtures through `cx` and the pinned oracle. Compare stdout bytes, stderr and status in pipes and PTYs for search invocations. Check Codeindex help and version against their own fixtures. Normalize executable identity, nondeterministic timing fields, and unspecified file traversal order only. Force a sort when testing ordering. Repeat native runs with the oracle unavailable on PATH.
- Exercise single-file and directory operands, multiple paths, stdin, empty matching lines, no matches, invalid regex, CRLF, Unicode, non-UTF-8 paths, ignore overrides, configured flags, grouped options, `-A24`, `-e --reference`, `-- --symbol` and ordinary search for every former verb. A blank matching line has a hit even if it contains zero source bytes.
- For the indexed modes assert exact stdout, stderr and exit status. Include a metadata-only ranked hit, competing anchor lines, two sites on one line, a multibyte prefix round-trip, nested working directories, partial empty results, ambiguous names, 50 versus 51 hits, and independent ranked context windows. Compare fresh, warm and watcher-served answers from the same inputs.
- Gate claims of ripgrep compatibility on the differential suite. Track unsupported flags, regex facilities and file-selection behaviors as gaps. An early error or different answer is a failed fixture.

## Addresses and machine output

A relationship file argument may be absolute within the observed repository or relative to the invocation directory. Resolve it to a path inside the pin. Use repository-relative paths as the keys for path searches and Markdown handles. In a Markdown handle split on the first literal `#`. Escape literal percent and hash in each handle component as `%25` and `%23`. Decode those handle escapes exactly once. Thus `a%23b.md#setup` selects the heading in `a#b.md`. Bare `a.md` selects all destination fragments for backlinks; `a.md#` selects only the empty fragment.

For indexed JSON output, preserve a JSON object per response, except for the documented null parent. The frozen indexed query payloads supply the following compatibility fields:

| Family | Shape examined by compatibility checks |
| --- | --- |
| Search, content, regex, path, glob, exact path, symbol | `root`, `generation`, `mode`, `coverage`, `relationship_status`, `truncated`, and `hits`. Each hit contains only fields belonging to its query type. |
| Relationships | `root`, `mode`, `generation`, `indexed_document`, `operation_status`, `position_encoding`, `relationship_status`, plus sibling top-level fields `result` (array), `truncated` (boolean), `reused` and `engine`. The new `contexts` and `evidence` fields explain contextual answers. |
| Section and backlinks | Section returns the section object, with exact source body and ancestry. Backlinks echoes `target` and supplies `hits` and `truncated`. Each backlink points to the source occurrence and gives the normalized target handle. |

Archived relationship status values distinguish unsupported operations, unindexed input, unresolved positions and resolved positions. Preserve the distinctions in native answers. The archived CLI could start a live language server for unindexed files, returning a separate server-shaped payload. A successful legacy live answer is a coverage obligation. Verify a corresponding native answer from captured inputs or record the case as an open parity gap. The design requires every new answer to come from the generation pin.

## Relationship response schema

A relationship query in JSON exposes the following members. JSON object member order is insignificant. Hit order, presence of nulls, and array order are significant.

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
