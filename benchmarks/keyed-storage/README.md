# Measuring one-file updates

Use the CLI benchmark for user-visible latency and the replay to isolate the cost of storing the same logical changes. Both write individual observations to JSON. Run one benchmark at a time on an idle machine.

## CLI

Build the baseline and candidate with the same Rust toolchain and release settings. Give the runner the two executables and a local Git checkout. It clones the checkout's HEAD, alternates arm order, and creates a new home and empty index for every trial. The relative Git config path selects the CLI's standalone refresh path on macOS so a background watcher cannot perform part of the timed update. The filesystem cache remains warm across trials.

```sh
python3 benchmarks/keyed-storage/cli.py /path/to/repo /path/to/before /path/to/after results.json --source src/lib.rs
python3 benchmarks/keyed-storage/cli.py /path/to/repo /path/to/before /path/to/after markdown.json
```

The source workload times a path query on a fresh index, after appending a comment, then after replacing the comment with a new declaration. Each result must come from a newly published generation. An untimed symbol query checks the declaration after each step. The Markdown workload times a backlink query after moving a link and after changing its destination. It checks the resulting line and withdrawal of the old backlink. The timer includes process startup, refresh and query output. File modification happens immediately before timing.

## Keyed storage replay

The replay requires Rust, a system SQLite development library and a clean checkout. Its Cargo manifest and dependencies belong to the benchmark.

```sh
cargo build --release --locked --manifest-path benchmarks/keyed-storage/Cargo.toml
python3 benchmarks/keyed-storage/run.py /path/to/checkout replay.json --rounds 6
cargo test --locked --manifest-path benchmarks/keyed-storage/Cargo.toml
```

The runner enumerates tracked text files and adds a small TypeScript import and Markdown fixture in memory. It parses and binds both source states before timing. The five projections cover declaration locations, name postings, resolved references, headings and backlinks. Each store bulk-loads the ordered rows, reopens, then durably publishes 32 alternating updates and reversions. Each iteration reopens the store and checks ordered 20-row queries against the fresh projection. A final compaction and reopen must preserve every row. After the last measurement the replay also reapplies the edit, reopens, and compares every row in the changed state.

- The page tree uses immutable 8 KiB pages, a 2 MiB reader cache and one sorted edit batch. Publication syncs the arena, writes and syncs a temporary selector, renames it and syncs the directory. To time those three stages individually, set `CX_REPLAY_SYNC_TIMES=1` when running `cx-storage-replay REPO NEW_DIRECTORY cow`.
- The run store uses the same pages and publication. It reads at most four roots. An edit that would create a fifth root compacts synchronously.
- SQLite uses a single `WITHOUT ROWID` table keyed by binary tuples, sorted prepared insertion, one transaction per generation, WAL, FULL synchronous mode, fullfsync, checkpoint_fullfsync, and a 2 MiB cache. Automatic checkpointing is disabled. Final maintenance checkpoints, vacuums and truncates the WAL.

The replay measures the storage layer with precomputed changes. It omits parser cost, production snapshot migration, oversized-record overflow and dependency-driven rebinding. Its temporary identity for a declaration uses a path and parser ordinal. Production adoption requires stable identities, crash-injection tests and an end-to-end CLI comparison. The native page and SQLite arms use different publication protocols. Compare edit timing together with maintenance and disk use.

## SQLite bulk-load layouts

```sh
python3 benchmarks/keyed-storage/run.py /path/to/checkout sqlite.json --arms sqlite sqlite-blocks --rounds 8
```

The `sqlite` control uses one primary-key row per fact and inserts each with a prepared statement. The `sqlite-batch` control uses the same rows, 16 KiB pages, and 128 pairs of parameters per insert. The `sqlite-blocks` writer uses 16 KiB pages and packs sorted facts in blocks targeting 3,584 bytes. It stores each key's suffix after the common prefix with its predecessor and writes one SQL row per block. The schema and contents commit together in the initial bulk transaction. Every layout uses the same WAL, cache, synchronization and final maintenance policy described above.

Each block's primary key is its largest logical key. To read from key K, SQLite seeks the first block whose largest key is at least K, then streams forward. The decoder examines keys inside that block in order. An edit batches replacements for each affected block into one transaction. A single oversized value occupies its own block, using SQLite overflow pages as needed. This trades a small amount of decoding and block rewriting for fewer SQL inserts and fewer stored key bytes. Compare edits, queries, post-checkpoint size and the WAL footprint under churn alongside fresh-build time.
