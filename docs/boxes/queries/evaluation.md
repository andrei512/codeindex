---
page-type: reference
---
# Evaluating a request

Query execution borrows a pinned generation and a typed request. The pin selects every source, dictionary, and coverage record the request may use. All transient cursors and ranking state belong to this request.

```text
evaluate(pin, request) → answer | invalid selector | unreadable generation
list answer = { ordered hits, truncated, coverage where applicable }
position selector = { repository path, line ≥ 1, column ≥ 1, input coordinate convention }
```

## Reader plans

| Request | Evaluation order |
| --- | --- |
| Literal or regex | Derive mandatory trigrams where possible; intersect candidate lists; verify on pinned bytes; convert true matches to source locations. A pattern without mandatory keys scans visible text. |
| Ranked chunks | Normalize each query phrase, match adjacent positions in the same field, combine phrase contributions per chunk using visible corpus statistics, then select by score and tie order. Produce snippets and breadcrumbs for returned chunks. |
| Relationship at a position | Resolve the file inside the pin, validate the selector and translate its input coordinate to a byte position, select the applicable contexts, find the shortest containing sites separately in each distinct analysis, and follow the corresponding contextual bindings, target postings and optional navigation groups. |
| Markdown | Resolve a section address to a subtree or parent. For backlinks use the normalized written destination as the lookup key. A target heading need not exist for referring links to exist. |

Do cheap candidate selection before source access. A cursor may stop after the first extra verified distinct hit only if its traversal order is the final result order. Ranked results require all relevant score contributions or a proven safe upper bound. Snippet formatting and ancestry recovery can wait until selection. Keep these costs out of the builder.

## Ranked search compatibility

The baseline splits input on whitespace, quotes each resulting item as a phrase and combines the phrases with OR. The tokenizer can split punctuation inside an item: `foo-bar baz` can therefore ask for the adjacent tokens `foo bar` or the token `baz`. Match positions within a field. Score the visible chunks with the frozen BM25 conventions, including phrase document frequency where a query item spans tokens. Term document frequency alone cannot give phrase frequency; derive it from visible positional matches for multi-token phrases. Preserve ascending baseline score, then path and first line as tie breakers. Use the body field for the excerpt, including when the match came from another indexed field.

## Repeated keys and display work

Probe a distinct mandatory trigram once. For ranked search, group identical normalized phrases for lookup and document-frequency computation, but retain each occurrence in the original scoring order. In a bounded working set, retain matched chunk IDs and phrase counts for scoring after phrase frequency is known. Above budget, compare replaying positions with spilling counts. Never erase the score contribution of a repeated query phrase.

Keep byte offsets through ordering and limiting whenever this preserves the public order. Convert coordinates and construct excerpts only for returned hits. A 51st hit can establish truncation without an excerpt or breadcrumb. For repeated hits in one section reuse the ancestry; for ordered offsets on one Unicode line advance a single conversion cursor. Keep these caches scoped to the returned results.

## Request memory and large results

A CLI invocation can finish its build and query in the same process. Release build-only credits at handoff and account for the pin and query allocations through output. Other invocations have separate address spaces. For a query, charge decoded posting blocks, match or automaton state, ranking scratch, retained evidence and output buffers to the request. Count a mapped source window once while views borrow it. The process limit applies to each invocation; several CLI processes can still compete for physical memory. Open the pin before traversal and keep it through the last write that borrows source data.

For ordered searches keep the returned page and one distinct overflow witness. For ranked search retain a bounded top-50 heap with its final tie key and track whether more distinct chunks matched. Complete the score of a chunk before deciding whether it can displace a retained hit. For frequency computations that exceed credit, spill sorted counts into request-private runs or replay the positional stream. Release runs on success, error or cancellation. A folded search key or regex might cross a source window; preserve the matcher state and required look-behind, or use a continuous checked file-backed view. Splitting into independent per-window matches loses multiline and boundary hits.

A section body can be larger than the query's memory budget. Stream its validated span through a bounded UTF-8 JSON escaper to stdout. Borrow pinned bytes until their write completes. The public result is still one JSON value. Deduplicate context and range evidence for retained hits only; when a single retained hit has many contexts, use sorted runs and merge in wire order instead of silently truncating its evidence. An allocation, spill, validation or output error ends the command unsuccessfully, including when some output bytes have already reached stdout. A caller accepts the answer only after successful exit and complete JSON.

Test the same section through a short-writing sink that splits inside an escape and a multibyte character. Compare the decoded body byte-for-byte with the pinned text. Interrupt the sink after one buffer and check that the pin and temporary runs are released. Run a regex spanning a window edge and a ranked query with more distinct chunks than fit in the accumulator. Compare the ordered result and truncation with the unconstrained query.

## Acceptance

A non-BMP character before a symbol exercises UTF-16 conversion. A verified set of 50 hits and one of 51 distinguish a full page from truncation. A regex with no usable trigram must agree with an exhaustive scan. Within one analysis, a tie retains all shortest sites. Across two analyses, if A has span [4,12) and B has span [6,8), a query at byte 7 must retain both context-specific results. Restricting to A returns A alone. Two contexts sharing A must reuse its one interval lookup. A generation switch during a query must leave its snippets, positions and backlinks pointing to the pinned source.

## Lookup and ordering rules

| Operation | Reader and final order |
| --- | --- |
| Exact path, substring path, glob | Exact lookup uses the repository path dictionary. Substring matching is case-sensitive. Glob matching uses the frozen `globset 0.4.21` grammar with literal separators on the full repository-relative path. Verify candidates and return distinct paths in binary UTF-8 path order. |
| Literal content, regex | Verify matches in the pinned source. Return nonoverlapping match starts in path then byte-offset order. For regex, match the frozen Rust `regex 1.13.2` Unicode string semantics, including zero-width matches and invalid-pattern errors. An alternation such as `abc|xyz` cannot require the trigrams of both arms. |
| Symbol | Match exact, case-sensitive spelling. Return path, spelling, kind, one-based LF line and one-based byte column. Coalesce identical public declaration rows across syntax modes. Order by UTF-8 path, numeric line and byte column, then UTF-8 kind and spelling to break a coordinate tie. |
| Definitions, references, callers | For selected identities, traverse the definition, non-definition reference or incoming-call directories respectively. Deduplicate visible answers after contextual resolution. A visible hit key is path, one-based line, UTF-16 column, public symbol and, for callers, public caller. Sort in that order using numeric coordinates, UTF-8 byte order for strings and null before a string. Merge equal public keys and retain distinct contextual evidence. Look up additional [pinned navigation ranges](../../domain.md#navigation-evidence-for-additional-locations) using the selected position, including external hits and a bound target with zero incoming calls. |
| Section and backlinks | Section returns the pinned subtree bytes, path, anchor, title, parent handle, line, bounds and breadcrumbs from root to selected section. Missing section is a lookup error; asking for the root's parent succeeds with null. Backlinks sort by source path, link byte offset, source section ordinal and target fragment; each hit gives the written target, referring handle and breadcrumbs. |
| Status | Read the pin's manifest and coverage summaries. Count visible paths, text files, parsed files and files with parse errors according to their capabilities. Report the generation's semantic coverage and any gaps. |

Content and symbol columns count bytes starting at one. The public `cx --reference FILE LINE COL` selector uses a one-based UTF-8 byte column. Advanced `cx --cx references FILE LINE COL` and relationship JSON hit columns count UTF-16 code units starting at one. LF separates lines; CR remains in the preceding line. Reject a selector inside a surrogate pair or beyond end of line. A position at line end is a valid boundary and can yield zero sites. For source `🙂x\r\n`, `x` starts at byte column 5 and UTF-16 column 3. UTF-16 column 2 is invalid.

## Ranking arithmetic and excerpt oracle

Let N be the visible chunk count, L the visible token total over all indexed fields, D the current chunk's length, H the distinct visible chunks matching a query phrase, and f the number of its occurrences across indexed fields in this chunk. For each phrase in input order compute:

```text
idf = ln((N - H + 0.5) / (H + 0.5))
if idf <= 0: idf = 1e-6
contribution = idf * ((f * 2.2) / (f + 1.2 * (0.25 + 0.75 * D / (L / N))))
score = negative sum of contributions
```

Use binary64 arithmetic. An empty corpus, or a query with no matching chunks, returns no hits. Choose excerpts using the frozen FTS5 body-column snippet behavior with a budget of 18 tokens, opening and closing highlights `[` and `]`, and ellipsis `…`. The conformance oracle is `snippet(chunks,3,'[',']','…',18)` and the default rank in the SQLite amalgamation pinned by the archived Cargo.lock. Compare exact excerpts, including for path-only matches. For a match in the body, the frozen snippet selector considers a window at each matching phrase start. Give the first occurrence of a query phrase in the window 1,000 points and each further occurrence one point. Center the chosen instance window around its first and last match and clamp to the body. For bodies longer than 18 tokens it also considers the nearest preceding sentence start, with a bonus of 120 at the body start or 100 at another sentence start. Sentence starts occur at the first token or after a period or colon followed by ASCII space, tab, CR or LF. Strictly better scores replace the incumbent; a tie retains the earlier candidate. With no body match, start at the beginning. Copy original source bytes over the selected range, coalesce overlapping phrase highlights and add ellipses at cuts. The exact long-window, punctuation and clipping behavior is tested against the pinned oracle. SQLite remains an offline oracle.

## Cross-product fixtures

- In `src/A.rs` and `src/sub/A.rs`, `glob src/*.rs` returns only the first path. A binary file named `src/B.rs` is also eligible for the glob and supplies no content hits.
- Given `aaaa`, literal `aa` hits byte columns 1 and 3. A zero-width start-of-line regex can match an empty line. An invalid regex returns an error. A negative result from an optional trigram cannot suppress a valid match.
- In body texts `foo bar`, `foo gap bar` and `foo bar foo bar`, `search foo-bar` matches the first and third, with the repeated-phrase chunk scoring ahead when equal-length padding fixes D. A phrase split between path and body cannot match. One exact score fixture uses rows (path, body) `(a, "foo bar pad pad")`, `(b, "foo gap bar pad")`, `(c, "foo bar foo bar")`, and `(foo, "bar pad pad pad")`, with empty parts in every row. Query `foo-bar` returns c at −1.375e−6 with excerpt `[foo bar] [foo bar]`, then a at −1e−6 with excerpt `[foo bar] pad pad`. Repeating the query item twice doubles each score and preserves those excerpts.
- For a text hit in the child of two nested headings, report the root, parent and child breadcrumbs. Opening the parent includes child bytes. Moving the referring file changes its relative backlink destination. Deleting the destination heading preserves backlinks to its written address.
- Add and remove a declaration, rename a path and replace one chunk. For every command compare incremental answers to a fresh generation with identical inputs. Compare one versus many workers after reopening, including deterministic order, coordinates, snippets, optional owner and truncation.
