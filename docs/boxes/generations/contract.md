---
page-type: reference
---
# Generation format and publication

A manifest selects the exact source and index extents a query will read. Publication is the consistency boundary.

```text
ExtentRef        = { segment: SegmentId, byte_offset: u64, byte_length: u64 }
ExtentEntry      = { id: ExtentId, product: Product, encoding_version: u32,
                     storage: ExtentRef, required_extents: Slice<ExtentId> }
Manifest         = { repository: RepoKey, generation: GenerationId,
                     predecessor: Optional<GenerationId>, format_version: u32,
                     files: visible file directory root, inputs: Slice<Dependency>,
                     directory: Dict<ExtentId,ExtentEntry>,
                     products: Table<Product,ProductRoot>, owners: Table<IdKind,OwnerDirectory>,
                     next_id: Table<IdKind,HighWaterMark>, segments: Dict<SegmentId,ImmutableSegmentName> }
ProductRoot      = { partitions: ordered key ranges → lists of extent refs, coverage_root }
OwnerDirectory   = sorted disjoint ID ranges → (extent, first row, row count)
GenerationPin    = { manifest: immutable view, segments: open immutable segment handles }
```

The fixed manifest header locates each root by checked offset and length. Product roots locate the dictionary or runs for a query key. Owner directories locate files, sources, analyses, binding partitions and symbols. For a callers posting, follow its partition ID through the binding owner directory, then the partition's file and analysis IDs through their directories, then the site ordinal within that analysis. Validate the referenced kind, bounds and owning analysis before dereference. Empty required products have explicit valid empty roots. Publication requires a root for each of the six products and an accounted-for close from each expected producer. A sealed semantic partition can carry Partial or Unsupported with an explicit reason; an empty posting then means empty within its recorded coverage. A missing required extent, crashed producer or input read error fails the private generation. It cannot turn into a coverage downgrade.

Allocate IDs used across extents from per-kind monotonic ranges under the refresh lease. Preserve the ranges of reused owners; never recycle a deleted ID while a retained extent could refer to it. The logical file table can have holes. Visible path order comes from the path directory, never ID assignment. New or invalidated binding dependencies replace all affected reverse rows. A full rewrite may start a new namespace only if it rewrites every reference into it. Test inserting a path that sorts before all existing paths while reusing an unaffected posting.

Bootstrap directories live inline in the manifest. Their roots can locate larger tables in segment extents. Publication validates offsets, lengths and references before switching the current manifest. Retaining an extent across generations requires retaining or translating its referenced ID namespace, including its FileIds. A pin keeps old segments usable during replacement. The envelope and each product carry independently checked encoding versions. A recognized older version can be upgraded through an explicit conversion or rebuild. An unknown required format, producer revision or newer writer floor forbids replacement and cleanup. Never open an unknown encoding under an older interpretation. The [v1 envelope](wire.md) specifies the byte layout, integrity checks and upgrade behavior shared by every candidate. Product codecs distinguish the measured index layouts.

Check cross-references and directory coverage once when sealing or publishing new extents. On normal reopen check header, versions, root bounds and segment lengths; on traversal check offset arithmetic and row bounds before touching the requested range. A normal single-symbol query must not scan every posting to validate the snapshot. Exhaustive decoding belongs to recovery and verification tools. A readable header alone does not establish semantic parity.

## Write and publish

Default to one publisher draining already encoded extents into a private data file and recording their offsets. The handoff transfers a buffer handle. A buffered OS write can still copy bytes into the kernel. Compare parallel positioned writes if sealed extents accumulate behind that writer on the critical path. Reserve disjoint ranges only once encoded lengths are known. If order by key is required, pay for placement or sorting where that product is built.

The fresh candidate uses one immutable data file plus a small atomically replaced manifest. Make segment contents and directory entries durable before the manifest can select them. Flush and sync the private manifest before atomically replacing the selector, then sync its parent directory before acknowledgment. A mutable pointer to the live checkout cannot back a published source view. A later generation can reference old immutable data files to reuse bytes; keep referenced files alive while a reader pins them. Pin acquisition and cleanup must coordinate so cleanup cannot unlink a file between reading the manifest and opening its segments.

Before the manifest switch, verify the expected predecessor still holds. Check input versions and repository membership, including ignore configuration. If validation discovers a change, discard or recompute affected work and revalidate before publication. On a live filesystem, observation and revalidation do not provide a transactional snapshot of the checkout. The manifest identifies the actual inputs behind the published answers.

## Reuse across updates

Reuse a fact only while both its input and interpretation context still match. Changing a file invalidates its local facts. Changing an export can invalidate references in unchanged files. Adding a declaration may invalidate a previous negative lookup. Replacing a chunk changes corpus statistics. Store dependency information at the cheapest granularity that preserves correctness; conservative recomputation is permissible when a finer dependency index costs more than it saves.

Extent reuse must preserve the meaning of every ID it contains. If file or symbol ordinals change between generations, either reuse the referenced identity mapping with the extent or rewrite the affected references. For every product, recorded dependencies include the exact file→analysis mapping behind local row IDs. Inserting a new first link can change the meaning of LinkId 0 with no bounds error. Rebuild or translate affected backlink postings even if the file ID and link count remain valid. Test this edit under an old reader pin.

## Pin acquisition and reclamation

A warm freshness check may precede a full query pin. Under the same metadata lock, open the selected manifest and every segment needed for its validation roots (or use validation data inline in the open manifest). Hold these handles while comparing the checkout, outside the lock. Upgrade only if that exact generation is still selected; otherwise release and retry. This avoids both opening all query segments for a stale generation and reading validation tables already unlinked by cleanup.

For the initial filesystem design, pin and cleanup share a short metadata lock. Under the lock, read the selected manifest and open all segment handles it requires. Release the lock before query execution. Publication and cleanup take the exclusive side when switching the selector or removing unreachable names. On the Unix target an open file handle keeps its segment readable after unlink. A platform with different removal semantics needs a deferred reclamation policy. A query that has acquired only a manifest has yet to acquire a pin.

Cleanup takes the repository refresh lease before collecting unreachable segment names. This protects a builder that has installed a new segment but has yet to switch the manifest. Pin acquisition, manifest replacement and unlink use a stable metadata lock separate from the replaced manifest inode. Retain the predecessor's segments until the selector directory sync succeeds. If that sync fails, report publication durability as uncertain, retain both segment sets and validate and re-sync the selected manifest and its directory on recovery before cleanup can remove the predecessor. Before the pin handle count approaches the platform limit, consolidate segments under the same guarded publication protocol; a failed open must release partially acquired handles.

## Failure and concurrency acceptance

| Injected event | Required observable result |
| --- | --- |
| Segment write, encode, file sync or candidate manifest sync fails before replacement | Fail refresh. Existing queries and a reopened predecessor still return the old answers. On the first build no incomplete generation appears. |
| Process dies after installing new segment names and before replacing the selector | A new request sees the predecessor, or an absent selector on first build. Cleanup may reclaim abandoned files after acquiring the refresh lease. |
| Selector replacement succeeds, then directory sync fails or the process dies | Return no durability success before the sync. After restart, open and validate the selected generation, which may be the predecessor or the candidate. Preserve both sets until recovery establishes the durable selection. On first build an absent selector triggers rebuild. |
| A required segment is truncated or a reference addresses the wrong owner | Report corruption when detected. Results must never silently lose rows. A rebuild can replace the corrupt generation only through normal publication. |
| A publication races with a warm validation or an active reader | The successful refresh pin names the exact validated generation. Already pinned reads finish from their original segments, including snippets and Markdown anchors. |
| A semantic producer reports a known unsupported construct, or instead exits without closing | The first case retains the explanation and scope in a sealed coverage record. In the second case publication fails and the previous generation stays readable. |

The refresh lease precedes the short metadata lock whenever both are needed. Readers release the metadata lock before waiting for the refresh lease or walking the checkout. Open validation handles once. On a successful upgrade reuse those handles and acquire only missing ones under the lock. A warm query should pay for its required freshness observation and selected reads without decoding all stored rows. Measure directory operations, segment opens, bytes validated and publication latency separately.

Run each failure case with an absent cache and with a durable predecessor where applicable. Force a stalled reader across publication and cleanup. Have two processes race a cold build, terminate the winning builder before selection, and verify that exactly one successor can complete the retry. Process termination exercises visibility and OS lock release. Durability at power loss additionally requires a fault-injection harness that can discard unsynced data and directory updates.
