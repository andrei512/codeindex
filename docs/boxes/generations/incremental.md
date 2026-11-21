---
page-type: mechanism
---
# Publishing small edits and upgrading an index

A checkpoint selects a complete generation. The writer can replace a paragraph, migrate a product along a recognized upgrade path, or refresh parser output while existing readers finish against the previous generation. This page describes the native implementation, the cost of a path change, and how to decide when another layout earns its cost.

## What the native writer publishes

The writer packs new bytes into one immutable part. A sorted range map joins new and retained physical bytes into the logical stream that each query product already reads. A range points directly into a physical file. A read finds the first overlapping range and visits each remaining piece in order. The lookup depth is independent of generation age. A pin holds the referenced handles open across unlink. The writer limits open parts by copying live ranges out of an older part. Those copied bytes count toward the measured update cost. Queries on the same pin share the counts of text files and recognized code files that appear in the coverage response.

| Input to a query | What happens on an existing-file edit |
| --- | --- |
| File metadata, source and parser evidence | The writer retains contiguous unchanged ranges. Catalog rows use file-relative lengths so a longer file leaves the following rows reusable. |
| Content trigrams | The writer compares old and new distinct memberships. Posting lengths and an offset sample every 64 keys bound the prefix sum for lookup. A dictionary-size change can still shift samples. |
| BM25 | A tree stores record lengths under bounded leaves. Ancestors store record counts and byte sums. A change can replace a leaf and its ancestors while the writer retains other subtrees. Planning can still read unchanged directory data. |
| Symbols, relationships and Markdown | An edit outside binding inputs can retain both graph products. An unchanged Markdown link membership can retain its reverse records. |

The selector records a writer protocol, a diagnostic build ID and separate producer revisions. Protocol 3 permits the grouped content and ranking codecs. A reader that knows the prior codecs can reuse them. An older writer rejects a newer protocol before recovery, publication or reclamation.

Path membership changes and edits to binding inputs can still be expensive. Current postings use generation-relative file ordinals. An insertion, rename or deletion can take the bulk path even for a lexically last file. The stable owner design below covers that case. Query order and truncation must still follow public paths after IDs and path order diverge.

## Design for stable owners across path changes

Start with a sorted, copy-on-write page tree for each lookup directory. A checkpoint holds a fixed set of roots for products, files, paths, dependencies, owner allocation and validation. Large directories live behind those roots. A page reference contains its segment ID, offset, length and codec. Pages are immutable. A batch writes each changed leaf and its affected ancestors once and retains the addresses of untouched children. Group the new pages and any new source or analysis extents into one segment for that publication. A fresh build bulk-loads sorted leaves and builds their parents bottom up.

Give each path a stable FileId allocated under the writer lease. A same-path edit keeps it. A deletion removes it from visibility. An unproven rename is a deletion and addition, even if retained bytes let us reuse analysis. Query order uses public paths and coordinates. Pack a posting under a key such as (trigram, FileId) or (term, FileId, chunk ordinal, field). Split leaves by the **whole** key, including its owner suffix. Put oversized keys, positions and source bodies in separately referenced extents so one frequent term or large value cannot force an unbounded page rewrite.

Keep a forward contribution for each replaceable owner. It must enumerate the distinct old memberships and supply the old counts needed to withdraw them. Reuse an existing file-local term vector, link table or binding table if it already supplies that information. Compare old and new contributions, sort their changes by key, and apply each insertion, replacement or deletion once. Unchanged rows remain visible through the new root. A deleted last row removes its key. Stable file IDs prevent an unrelated insertion from renumbering postings.

## Follow one Markdown edit

| Query input | Work for a paragraph edit |
| --- | --- |
| Source and navigation | Store the changed file and its updated line and section ranges. Reuse unchanged structure when its coordinates still apply. Moving text above a heading can change its range without changing its anchor. |
| Exact text and regex | Compare distinct trigram memberships. Add or remove a file only when membership changes. Exact matching still checks the pinned source. |
| BM25 | Compare the file's chunk and field contributions. Update changed positions, lengths, total visible chunk length, visible chunk count and term frequencies. Document frequency counts each visible chunk once per term across fields. Score candidates with statistics from the same pin. An untouched document can receive a different score without any rewrite of its postings. |
| Backlinks | Key reverse membership by destination and source link identity. Retrieve coordinates from the source link table. An unchanged destination and link identity need no backlink update when only its offset shifts. Requalify written destinations on moves and account for inserted links that change local ordinals. |
| Paths, symbols and relationships | Keep their roots for an existing Markdown paragraph edit that affects no semantic input. A declaration or project setting can invalidate successful lookups, misses and shadowed candidates. Replace both forward and reverse edges in the affected scope. Use conservative scope rebinding wherever dependency evidence is incomplete. |

The unit of source replacement can initially be a whole edited file. Replacing a large file or changing a widely used export can legitimately be expensive. For a small paragraph edit, account separately for changed source bytes, applicable posting changes, copied tree pages and the foreground filesystem validation walk.

## Which version decides what runs?

The manifest records the producing build ID for diagnostics. Compatibility decisions use a small registry compiled into the binary. Each entry names a stable contract, its supported revisions and the permitted direction of conversion.

| Contract | Decision on open |
| --- | --- |
| Manifest schema and writer protocol | Read known compatible schemas. A protocol floor fences off writers that cannot preserve this generation. Crossing the floor requires a recognized upgrade edge. |
| Physical encoding per extent | Retain every extent whose codec the reader supports and whose keys retain the same meaning. Old and new codecs may coexist. If conversion is required, decode the old extent and encode the same logical rows. |
| Meaning of facts per producer and mode | Identify the rules for each output, such as Markdown anchors, Rust extraction, ranking tokenization or ignore eligibility. Record the consumed input and upstream projection revisions. If the rule revision changes, regenerate that output and propagate only changed projections. A different query-time scoring formula requires zero index writes if the stored statistics still suffice. |
| Owner namespace, routing and key semantics | Changing the meaning of an ID or key requires translation of every referencing extent, or rebuilding that dependency closure. A byte-compatible posting with a different key meaning cannot be reused unchanged. |

Every producing change declares which contract changes. CI pairs historical writer fixtures with current readers and compares current query results against fresh builds. A registry change includes the golden case that distinguishes the old and new semantics. Older, newer and divergent semantic revisions are distinguished by explicit supported upgrade edges. A binary encountering unfamiliar required semantics or a newer writer floor returns a compatibility error before publication, recovery or reclamation.

## First upgrade and publication

1. Recognize the shipping native 1.0/1.1 manifests and their single-segment product layout. The first page-based generation uses a deliberately incompatible manifest major. Existing binaries reject it before cleanup. Import live file ordinals as initial stable IDs and set the allocation high-water mark above them. Retain a verified source or convert known postings only where the old keys, producer fingerprint and dependencies can be proven equivalent. Otherwise regenerate from verified input. This initial conversion may cost one full build.
2. Under the writer lease, reselect the predecessor and check writer compatibility before mutating the store. Combine checkout changes and migration into one plan. Revalidate working-tree evidence before switching. An older watcher, compactor or retry follows the same checks. A release with identical contracts and unchanged inputs should return the existing generation with zero index writes.
3. Write the new immutable segment and a generation-named immutable manifest using names outside the legacy janitor's cleanup pattern. The small `current` selector identifies the named manifest. Sync the new files and their directory entries, then atomically replace the selector and sync its directory. Preserve predecessor and candidate through an uncertain switch. Disk-full, failed validation and unknown-format errors leave the last durable generation recoverable. Corruption has a distinct error and an explicit recovery path.
4. Under the metadata lock, reread `current`, open its named manifest, and acquire a shared OS lease on that manifest before releasing the lock. The manifest name stays discoverable until collection can take its exclusive lease. A pre-upgrade reader can finish on the legacy segment handle it already owns. Open referenced segments lazily through a bounded handle cache. The collector takes the writer lease, identifies the selected, recovery-protected and reader-pinned generation roots under the metadata lock, and traces their transitive page references before unlinking unreachable segments. Reader death releases the lease. Run collection outside normal edit publication. A collector that cannot prove a segment unreachable retains it. Bound accumulated dead space with maintenance under disk pressure and report that cost separately.

## Alternative and decision gate

Bounded sorted delta runs are the competing design. They can write fewer bytes when one edit changes a few keys scattered across many leaves. They make each lookup merge matching runs. Each run must carry explicit replacements and deletions for full logical keys. Select the latest visible value before scoring or truncating. Retire a deletion only after merging every older overlapping run it hides. Enforce a hard run-count bound with maintenance on the foreground path if the watcher is absent. A complete file-local index per edit would make repository-wide misses probe too many files.

Build one vertical slice through the real CLI using a Markdown edit. Compare copy-on-write pages and bounded runs on identical inputs, including a long edit sequence with maintenance enabled. Measure fresh build, immediate save, unchanged query, rare miss and common-term lookup. Report foreground and deferred bytes separately, directory pages copied, read fan-out, retained disk space and p50/p95 latency. Vary page size and run limit. Prefer pages unless runs reduce total write traffic and save latency while meeting the same measured query and space budgets. Establish those budgets against the unchanged release before choosing tuning constants. Neither speed nor a fixed page size has been established for this proposal.

For acceptance, double unrelated files and then double postings for a term in the edit. One edit may pay for tree height and changed bounded leaves. It must write zero unrelated source, graph or posting leaves. Count unchanged neighboring rows copied inside an affected leaf as write amplification. Include a BM25 edit that changes total chunk length while chunk count stays fixed, and check scores on untouched chunks against a fresh build. Compare every query family with a fresh build after deletion, rename, heading collision, a newly resolved symbol and a changed high-scoring chunk. Hold an old pin while migrating, compacting and reclaiming. Exercise a pure codec upgrade, a Markdown-only rule upgrade, an unknown future selector, a stale writer, and failures on both sides of the durable switch. Include peak open handles and storage retained by a long-lived pin.

### Performance gates for shipping a layout

Keep the current release as the control. A product can keep its packed representation where sequential scans or intersections win. Page boundaries are a candidate update mechanism. Each product owns the representation its queries traverse. Pin once per request, traverse only the selected product roots, and keep long posting scans sequential where possible. Check version compatibility at pin acquisition and codec dispatch at extent open. The ordinary query loop must never apply migrations or replay edit history.

Run paired control and candidate builds on fixed, representative revisions. Alternate run order, use the same queries, and match filesystem cache, watcher and durability conditions. Record both end-to-end CLI latency and query execution on an already validated pin. The latter catches a slower lookup hidden by filesystem validation. Baseline the current binary against itself first to establish measurement noise and freeze the comparison before tuning.

- **Queries.** Compare p50 and p95 per repository and query family, for warm and first-open queries. Include misses, frequent terms, BM25 top results, exact search, regex with and without a mandatory literal, path search, symbol and relationship lookups, and Markdown navigation. Repeat after many edits and while maintenance runs. One faster query family cannot offset a regression in another.
- **Fresh builds.** Compare empty store through durable reopen. Keep the bulk sequential build path. Record peak memory and index size alongside elapsed time.
- **Updates.** Compare immediate save-to-correct-answer latency, index bytes read and written, and deferred maintenance after the same edit sequence. Validate results against a fresh build. Time filesystem validation separately while including it in the CLI result.

A repeatable slowdown in queries or fresh builds blocks adopting that layout, even if its update benchmark wins. An inconclusive comparison calls for more paired samples. Ship only after demonstrating a repeatable update improvement with query and build performance preserved. These are acceptance criteria for the proposed implementation; the existing Markdown edit benchmark alone does not establish them.
