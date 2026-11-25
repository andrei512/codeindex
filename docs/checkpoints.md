---
page-type: mechanism
---
# Checkpoints and the five-minute watcher

A checkpoint lets `cx` answer from work already paid for. A short-lived watcher prepares the next checkpoint as files change. On macOS each repository command renews a five-minute watcher. An unchanged query validates the working tree and uses the pinned generation. On other supported Unix targets the CLI runs the same validation and publication directly.

## What identifies a file?

One generation stores its worktree identity, optional HEAD commit and Git object format once. The native indexes use file ordinals in sorted path order. Across generations, refresh matches paths and verified source identities and translates posting IDs when the order changes. The version 2 manifest can reuse physical posting ranges across generations. Stable owner IDs across insertions and deletions remain a design target.

For a fully consumed file, compute the Git blob ID over the captured working bytes, including the standard blob length prefix in the hash. Hash the captured buffer before parsing a new source. Store the ID once, tagged **committed** if it equals the regular-file blob at that path in the generation's HEAD tree, otherwise **local**. Use the repository's SHA-1 or SHA-256 format. The tag records byte equality. It requires neither commit history nor an invocation of Git. When commit evidence is unavailable, retain local identity. An absent HEAD is valid. Missing evidence required for eligibility still fails validation.

If HEAD contains A, staging contains B and the working file contains C, index C. A commit containing C can promote that entry without a parser run. Checkout filters, CRLF and LFS can make raw bytes local even when Git reports clean. Staging can change tracked membership and must be observed independently. With unchanged HEAD, keep known tree evidence. Across commits, compare tree IDs and descend only into changed subtrees as needed. Equal tree IDs save Git lookups. Working-file evidence is still required.

A path-only binary can stop at its first validated binary witness. The current adapter retains its observed stamp and reclassifies suspect binary files from the beginning. The capture contract also defines a compact byte witness for providers with untrustworthy stamps. Computing a full digest for the unread tail buys no query result. Reclassify when the witness fails. A separately consumed binary support input needs evidence for every byte its consumer uses.

### Snapshot compatibility

The version 2 selector carries a sorted mapping from logical index ranges directly to immutable part files and a registry of producer revisions. A pin opens only its mapped parts. The writer bounds the number of open files and can pack live ranges together. Ordinary lookups follow the selected map without replaying old updates. New catalogs carry file content identities, leaving the checkpoint with a committed bit per file and sparse stamp overrides. Metadata-only publication shares the product data.

The native reader recognizes shipping 1.0 and 1.1 layouts. When the old extraction fingerprint has a known equivalent, migration can reference its existing segment. Unknown historical fingerprints in a recognized legacy format require a rebuild. The semantic registry specifies which products an understood rule change invalidates. A future required contract, unreadable version or damaged selector fails before cleanup. Existing pins continue on their open parts during publication.

## What runs on a request?

1. Locate the worktree and open validation handles for the selected generation. Upgrade them to a query pin if validation succeeds and the selector still matches. When a write is required, acquire the existing single-writer lease and recheck the selector before building. Validate membership, ignore and tracked controls, file versions, support inputs and extraction rules. The capture contract defines when file stamps suffice and when bytes are necessary. Include missing dependencies and external control files.
2. Reuse unchanged source and contributions by handle. For a suspect file, hash and classify during capture. Hold the verified source handle until the identity lookup completes. Start extraction only for missing analyses. Use the blob ID and length to find candidate sources. Verify equality against retained bytes while the capture is resident when source identity is uncertain, including hash collisions. If the bytes match an existing source, reuse its analysis for each matching parser mode and rule version. A changed timestamp alone incurs zero parses. Reuse file-local facts on a move only after verifying source and mode. Distinct paths remain distinct owners even with identical bytes.
3. Withdraw obsolete contributions and install new ones under their stable owner. Requalify relative Markdown destinations, path terms and semantic context after a move. Rebind consumers of changed scope or lookup projections, including earlier misses. Update ranked corpus statistics and affected posting partitions. Untouched files keep their contributions. Account separately for physical partition merges.
4. Publish changed extents and manifest through the existing durable selector. A HEAD or provenance change alone publishes checkpoint metadata and reuses every query extent. An unchanged generation needs no publication. The query retains a generation pin through output. On failure or lost process, recover from the durable selector, reconcile and retry under the lease. An error reaching freshness must surface to the caller.

### Current cost of a content edit

For a same-path text edit, refresh verifies changed bytes, computes the changed trigram memberships, and updates affected BM25 terms and corpus statistics. New ranked records use file-relative positions. For Markdown it updates the changed document and compares outgoing link memberships before touching the reverse index. With unchanged path membership, edits outside binding inputs reuse symbol and relationship ranges. The writer retains adjacent immutable byte ranges by reference and stores only new or deliberately repacked data. Directory updates, small copied ranges, source capture and durability still cost work. Membership changes can require translating ordinals and rebuilding larger packed products.

A checkpoint records which captured sources match HEAD. Several metadata-only checkpoints can share one product generation, which can reference multiple physical parts.

[Storage design and performance gates](boxes/generations/incremental.md)

## What does a live watcher prove?

One owner per canonical worktree and cache listens on a Unix socket in a user-private runtime directory. The short socket name fits macOS path limits even for deep checkouts. Elect it under a process-lifetime OS lock separate from the refresh lease. Contenders connect or retry election after owner death. A request checks the protocol and worktree identity. Start subscriptions before the initial reconciliation. The macOS callback coalesces all notifications into one pending reconciliation. This also covers coalesced directory changes and event loss. Keep changes received during a build for the next pass.

Each accepted repository query sets the idle deadline to monotonic now plus five minutes. Help and version work outside a repository. File activity never extends this deadline. At expiry, serialize admission against shutdown. Finish admitted requests, release handles, remove the endpoint while holding the owner lock, then release the lock. Cancel any unneeded speculative build at a safe boundary. A racing client reconnects or elects a successor. A watcher startup failure uses the same refresh operation in the CLI.

On macOS use FSEvents to precompute changes. Before serving a request, reconcile against the filesystem with an observation started after admission. Batch clients already admitted at the start of that observation. A client arriving later needs a subsequent observation. A prior save that stays on disk must be visible even if the callback is delayed. Concurrent edits follow the capture contract's retry rules. A continuously unstable input may yield a freshness error. In watcher mode the owner executes the query against its pin and streams the result over IPC. The CLI renders diagnostics and preserves its ordinary stdout and exit contract. A disconnected client releases its pin.

A provider can omit the foreground walk only with a documented ordering fence and coverage for every relevant mutation, including external dependencies and hard-link aliases. Uncovered inputs still require reconciliation. Watchman observed delayed checkout events even after a cookie and synchronous flush on macOS. [Watchman query synchronization](https://facebook.github.io/watchman/docs/cookies.html#limitation-macos-fsevents)

## Implementation order and decision tests

1. Add manifest identity and stable contribution owners first. Test unborn HEAD, linked worktrees, staged and unstaged changes together, commit promotion, same-size edits, filtered files and path-only binaries. Compare each query with an empty-store build.
2. Implement delta publication before background scheduling. Insert a lexically first path, move one of two byte-identical Markdown files, delete a frequent term, and introduce a previously missing symbol. Compare paths, backlinks, scores and binding targets. Hold an old pin across the update. Count unaffected captures, parses and rewritten postings separately.
3. Add the watcher using the same refresh operation. Delay a callback until after the query returns, overflow its queue, kill the owner during publication, race two first clients and race admission with expiry. Use a fake clock to verify a request at 4:59 extends the deadline to 9:59. A stream of saves alone must let the owner exit.
4. Compare foreground-only refresh against speculative refresh on fixed representative checkouts with the same save/query sequence. Report median and p95 save-to-query latency, idle CPU, validation I/O, bytes written and peak memory. Compare partition rewrites against bounded delta segments with compaction under the same query load. Require fresh-build answer equality and measure read amplification before choosing partition sizes or a compaction threshold.
