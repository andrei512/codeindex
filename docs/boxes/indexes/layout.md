---
page-type: reference
---
# Index layouts

A product owns its writer and its reopened reader. These proposed arrays serve all six query families. Local ordinals address rows within an owning extent.

```text
ChunkRef         = { file: FileId, chunk: ChunkId }
DeclarationRef   = { file: FileId, analysis: AnalysisId, decl: DeclId }
SectionRef       = { file: FileId, section: SectionId }
LinkRef          = { file: FileId, link: LinkId }
Trigram          = three Unicode scalar values
PostingLists<K,V> = { keys: Array<K>, starts: Array<ordinal>, values: Array<V> }

Paths            = { by_path: Dict<RepoPath,FileId>, grams: PostingLists<Trigram,FileId> }
Content          = { grams: PostingLists<Trigram,FileId> }
Ranked           = { by_term: PostingLists<Term,RankedPosting>, positions: Array<ordinal>,
                     lengths: chunk-aligned Array<token_count>, stats: CorpusStats }
RankedPosting    = { chunk: ChunkRef, field: Path | Body | IdentifierParts,
                     positions: Slice<ordinal> }
CorpusStats      = { visible_chunks: count, total_chunk_length: token_count }
TermStats        = visible chunk frequency stored once per visible term key
Symbols          = { by_name: PostingLists<Spelling,DeclarationRef> }
Relationships    = { partitions_by_file: PostingLists<FileId,BindingPartitionId>,
                     sites_by_analysis: Dict<AnalysisId,IntervalDirectory>,
                     definitions: PostingLists<SymbolId,BoundEdge>,
                     references: PostingLists<SymbolId,BoundEdge>,
                     callers: PostingLists<SymbolId,BoundEdge> }
BindingPartition.navigation = Optional<NavigationDirectory>
NavigationDirectory = Dict<(local_site, operation, role_binding_ordinal, target_ordinal_in_outcome), NavigationGroupId>
BoundEdge        = { site: BoundSite, role_binding_ordinal, target_ordinal_in_outcome }
IntervalDirectory = source-ordered local site refs within one analysis with a prefix maximum of span end
MarkdownLookup   = { sections: Dict<(FileId,Anchor),SectionId>,
                     backlinks: PostingLists<Address,LinkRef> }
```

For a posting dictionary with N sorted distinct keys, `starts` has N+1 offsets, begins at zero and ends at the value count. For example, keys `[abc,bcd]`, starts `[0,2,3]`, values `[7,9,9]` mean `abc` occurs in files 7 and 9 and `bcd` in file 9. Searching for `abcd` intersects the lists and verifies the literal in file 9's pinned source. Exact membership has one row per distinct key and file. Ranked positions keep repeats; the position count supplies field term frequency. Document frequency counts distinct visible chunks containing a term across indexed fields.

Resolve the selected BindingPartitionId through the pinned owner directory. Its optional header field locates the sparse navigation directory, group table and support-document table. Group and support-document ordinals are local to this partition. A reader can use the queried local site to reach a support URI and its pinned ranges even if the destination has no visible FileId. Reverse edge lists supply ordinary indexed hits. Preserve group-level target ranges even when the observed hit list is empty. See the [navigation group and its location owners](../../domain.md#navigation-evidence-for-additional-locations).

For a position query, select the applicable binding partitions for the pinned file and group them by AnalysisId. Search each distinct analysis directory once. Binary-search the last site starting at or before the byte offset and walk backwards while the prefix maximum end can still cover it. Keep every tied minimum-width containing site **within that analysis**. For each selected partition, read its context-specific bindings for those sites. A shorter site in another analysis cannot eliminate this one. The BoundEdge selects the exact role and candidate entry when one span has multiple meanings. On an update, incoming postings for old targets disappear together with the replacement bindings. For backlinks, the path prefix of the sorted address dictionary serves whole-document lookup; a full key serves exact-fragment lookup.

## Partition lifecycle

Open a partition with the product's key range and input generation. A producer transfers a bounded block of memberships or occurrences. The active writer inherits its charge. Under count-and-place, keep the input block charged across counting and scatter; release it only after final consumption or a successful transfer to private replay storage. Track closure at the product boundary: discovery holds an open registration token, registers producer batches before exposing them, then closes registration. Each batch enqueues or transfers its final block before releasing its token. After discovery closes, only a live parent producer may split its existing token to register a child. Register the child before exposing its work or dropping the parent token; no new root tokens may be minted. The last such child may deliver the final posting after discovery has finished. Include this interleaving in the closure fixture. Only touched key partitions receive blocks. An empty product is represented by a closed registration and zero pending work. Avoid sending an empty close message for every file × partition. Seal when all expected producers have closed, every block has drained, and all referenced source or binding ordinals have owners.

A sealed partition carries its encoding, lookup range, extent lengths, required extents and coverage. Validate monotonic offsets, the final count and every referenced range. An allocation or encoding failure aborts the private build. A missing producer keeps the partition incomplete; an empty dictionary is valid only after closure.

## Building the postings

**Exact content.** A file worker emits each distinct trigram membership once, in blocks grouped by lookup-key partition. A key routes deterministically to one owner. Within an admitted partition, count memberships by key, prefix-sum counts, and fill disjoint file-ID slices. To produce sorted IDs, consume file ranges in order or assign subranges from sparse per-block counts in file-range order. Sort an individual block when required. Arrival order alone cannot establish ordering. A second pass over retained membership blocks can be cheaper than global sorting. It never needs the original source again.

Counts determine the size of fixed-width lists. Delta coding additionally needs encoded lengths and correct gaps between blocks. Charge that encode pass and its buffers to the candidate. If estimated peak live bytes exceed the remaining budget, split the key partition or seal bounded sorted runs. An oversized hot key can itself split by file range. Readers of retained runs pay in dictionary probes and posting cursors.

**Ranked search.** Emit field and ordered positions; their count already supplies frequency. For the baseline BM25 ranker a chunk needs one total length across its fields. The normalized path-token sequence depends on the path, so compute it once per file and project it into each chunk. A rename updates the path field and its statistics; unchanged body tokens remain reusable. For overlapping chunks compare a bounded overlap window with direct streaming. Preserve per-chunk positions and statistics either way. Route by term and seal sorted runs per bounded partition. Preserve positions and document length for every visible chunk, including overlap. Query scoring uses one generation's statistics. Match each query phrase within its field and combine phrase scores per chunk before cutting to the best results. Benchmark consolidating only the partitions whose run fanout makes reads expensive.

Count bytes written and reread by intermediate posting buffers. Tokenize a path once per file and reuse its terms across chunks.

## Layout selection

Compare three layouts on the same facts: file-major tiles, bounded key partitions with count-and-place, and bounded sorted runs. Include routing traffic, duplicate dictionaries, temporary counters, peak bytes and bytes actually written. Time fresh build through durable reopen. Also time the first query, a rare miss, a common hit, a ranked multi-term query and a one-file update. If a merge adds build time B and saves q per query, B/q is its break-even query count when q is positive. Report the tradeoff per corpus. Correctness checks run for every candidate.

## Addressing and update parity

Interpret a local chunk or section ordinal against the file's designated search analysis. A declaration reference includes its syntax analysis. Resolve every BoundEdge through its owning binding partition and role entry before projecting a hit. Secondary directories may retain immutable ordinals across generations only when the new pin preserves the entire referenced namespace or supplies a validated translation.

A replacement removes the old file's visible memberships, positions, chunks, bindings and outgoing document links before deriving the new generation's visible counts. Physical reuse can filter superseded rows or rebuild an affected partition. In either layout, document frequency counts a chunk once even when a term occurs in path, body and identifier parts. Deleting the last posting for a term must make lookup empty. An unchanged referring Markdown file still contributes a backlink to a destination that was deleted.

For a file containing the same trigram at five offsets, verify one exact membership and all applicable ranked term positions. For a term occurring in two fields of one chunk, verify two field postings and document frequency one. Duplicate the source at a second path, then remove the first file; verify shared bytes stay reachable, the old path and postings disappear, and the remaining chunk keeps the counts and coordinates of a fresh build. A rename that changes ranked path tokens must change its path-field positions and corpus totals without changing its body.

### Replacement in the baseline layout

At seal, retain a sorted, distinct list of the posting partition IDs touched by each independently replaceable contribution. For ranked search the contribution is a file's visible chunks and fields. Record partition IDs as the existing routing operation emits blocks. Prefer one ID per touched partition to a second copy of every token or position. Include the cost of this directory and its read during refresh. A tiny index may use a single partition; edits then scan that partition. The design makes this cost visible.

For replacements, use the union of the retired contribution's partition IDs and those receiving its new blocks. Stream the old sorted lists of each affected partition against the retired contribution IDs; omit those rows, merge the new rows, and seal the replacement. Read unaffected postings through reused extents. Removed-only terms disappear. Recompute visible DF for each rebuilt term while writing its grouped postings, counting each chunk once even across multiple fields. Adjust N and total length using the old and new chunk-length ranges once per replaced contribution. Emit the new touched-partition directory and the new manifest in the same generation. If a term spans multiple physical runs, count unique visible chunk IDs across those runs before publishing that term's statistics. Directory reuse requires stable partition routing and referenced ordinals. A change to routing invalidates that directory.

A path-only rename can reuse local body extraction. The conservative baseline may still read, filter and re-emit unchanged body postings in the affected ranked partitions while replacing that file's projection. Measure visited postings, retired postings, bytes rewritten and retained-directory bytes. A finer field-specific directory is justified only if its reduced update traffic repays its storage and construction cost. For relationships apply the same rule to the retired binding partition's incoming contributions, including keys that receive no new edges. A fixture must delete a file with a unique key and update a file where one term appears in all three ranked fields, then verify exact DF and absence of retired incoming edges after reopen.
