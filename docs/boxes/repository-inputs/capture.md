---
page-type: reference
---
# Discovery and capture

Repository inputs produce one eligible path inventory for a working tree and one immutable capture of each required file version. A search runs against the captured bytes. A live checkout can change while a build or query runs.

## Operations

| Operation | Result | Consumer |
| --- | --- | --- |
| Locate(working directory) | Canonical worktree root and repository key | Refresh uses the key to inspect the current manifest before walking the tree |
| Discover(root) | Eligible entries, their observed versions, and the inputs that determined eligibility | Build coordination owns this observation and reuses it for planning |
| Capture(entry) | Path-only entry, immutable text, a changed input, or a read error | File analysis and source storage borrow the same capture |
| Validate(observation, support inputs) | Valid, changed membership or versions, or validation error | Refresh decides whether it can return a pin or publish a candidate |

Locate finds the nearest working-tree marker, including a linked worktree's marker file. A linked checkout uses its own worktree and tracked index even when it shares object storage and repository rules with another checkout. Invoking Codeindex through two spellings of the same root should select one cache. Two worktrees with different checked-out files must select distinct caches.

### Cache identity

The Unix cache path is `~/.codeindex/repos/<digest>/`, where the digest is lowercase hexadecimal SHA-256 of the canonical absolute worktree-root path bytes without a terminating NUL. Compute it once per locate. The manifest stores the full root as an identity check. On a mismatched root, return a cache identity error before acquiring any write authority over that store. A moved worktree gets a new cache. Root aliases resolving to the same canonical directory share it. Recreating a checkout at the same path still requires complete freshness validation.

If the working tree contains the global cache (for example, a home-directory repository), prune the entire engine-owned `~/.codeindex/repos/` directory before untracked discovery and validation, across all repository keys. This prevents publishing `current` from changing the next generation's inputs. Pruning only suppresses untracked discovery of engine output. Tracked paths are enumerated separately. A tracked path colliding with an engine-owned artifact is a cache-location conflict and fails explicitly before publication. Unrelated files in the checkout keep their ordinary eligibility. Build a home-directory repository, publish a second worktree’s store in between two home queries, and verify that the second home query reuses the first generation. Exclude the reserved subtree from membership comparisons even if another store is created there during validation.

## Eligible paths

The working inventory is the union of present tracked paths and eligible untracked paths. The tracked index decides membership. The current working file supplies the bytes, including unstaged edits. An absent tracked file produces no current path hit. The administrative Git directory is outside the searchable inventory.

For untracked entries, start with the configured user excludes, apply repository `info/exclude`, then apply per-directory `.gitignore` rules from the root toward the entry. Later matching patterns at the same precedence win. Directory-relative matching, negation, escaped leading punctuation, trailing-space escapes and directory-only patterns follow their Git meanings. An ignored parent prunes its untracked subtree. A separately enumerated tracked child remains eligible. Read the required metadata directly. The installed CLI must work with no Git executable.

Record which rule files, index, and configuration supplied a decision. Include their effective settings and observed absence where a newly created file changes the result. A warm observation must cover additions, deletions and replacements as well as changed contents. A root-directory timestamp alone misses in-place edits and changes below that directory.

| Observed entry | Path result | Text input |
| --- | --- | --- |
| Regular file containing valid UTF-8 and no NUL | Present | Exact captured bytes, including an empty file |
| Regular file containing NUL or invalid UTF-8 | Present | Path only |
| Symlink, including a dangling symlink | Present when eligible | Path only; keep external targets outside source capture |
| Directory, missing tracked path, or special device | Absent | None |
| Non-UTF-8 path | Excluded and accounted for | None |

Distinct repository-relative spellings retain their own path identity, including spaces and newlines. Preserve the prototype's exclusion of non-UTF-8 paths. Use the effective repository/filesystem matching rules for discovery. Never lowercase the stored display path as a shortcut.

## Capture and freshness

Open a regular file without following a substituted symlink. Compare the opened object's identity with the observation, then compare its version before and after capture. Resolve from the intended worktree and verify that the path still names the captured object. Disappearance, replacement or mutation during capture is a changed input. A permission or I/O failure is an error. On the reference Unix filesystem the observation uses device, inode, size, mtime and ctime at available precision. Reuse across observations requires a trustworthy version signal; on a filesystem whose stamps cannot distinguish edits, verify the contents or report that freshness cannot be established.

For a regular file whose only consumers require text, a NUL or definitively invalid UTF-8 proves path-only eligibility. Stop reading its payload at that point. Still check the object's version and path identity. Carry an incomplete UTF-8 sequence across read boundaries and treat it as invalid only on a bad continuation or at EOF. A separately required binary support input has its own consumer and can still require more bytes. The byte classifier can collect LF offsets while bytes are resident. Transfer those offsets to the source owner once for all modes. Discard provisional offsets if a later byte makes the file path-only. No parser receives an unvalidated source as sealed text.

On untrusted stamps, path-only classification can retain a small witness: a NUL at an offset or an invalid UTF-8 subsequence that proves invalidity at its recorded position, including required boundary or EOF information. Recheck that witness against the same opened object during validation. If the bytes or required boundaries no longer prove invalidity, classify again. A witness cannot establish the freshness of a separately needed binary support payload. Test replacing the only NUL with ASCII while size and timestamps stay constant: the next successful refresh must find the new text.

Exercise a multi-gigabyte sparse path with a NUL in its first buffer: path lookup must succeed without reading the remaining payload. Split the four bytes of a valid non-BMP scalar across two buffers, then repeat the fixture with the final byte missing. Only the complete scalar is searchable.

A searchable control file such as `.gitignore` or a project manifest can feed two consumers. Share the first captured version, parse each interpretation once, and apply directory-relative rules with the referring path. Verified hardlink aliases can share source bytes while retaining separate path entries. A rename may reuse a verified capture but must update path-dependent lookups.

Binding can read project settings or dependency metadata outside the searchable inventory. Record the versions and negative lookup dependencies it actually consumed. A failed input observation cannot prove semantic absence. Final validation checks these dependencies and membership before publication. The manifest describes the observations a published answer uses. A filesystem without checkout-wide snapshots cannot promise that every captured version coexisted at one instant.

## Acceptance

Compare a fixture's eligible paths with the frozen prototype and a test-only `git ls-files --cached --others --exclude-standard -z` oracle, filtering absent paths and applying the path-type rules above. Run the Codeindex invocation with an empty executable search path.

- Place tracked and untracked children in an ignored directory. Only the tracked child appears. Exercise a nested re-inclusion with a traversable parent, a repository-local exclude and a user exclude overridden by a nested rule.
- Change only an exclude file or stage a previously ignored file. A warm refresh must change the visible set. Stage one version and then edit its working copy. The search result must contain the working bytes.
- Replace the path with a symlink between discovery and open, or delete and recreate it during capture. No result may expose bytes from a substituted external target. Preserve the previous published generation on failure.
- Use one tracked, searchable control file in discovery and analysis. Count one source capture on stable input. A final metadata observation, retry on actual change, and independent verification in a test are distinct operations.
