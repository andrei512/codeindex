---
page-type: reference
---
# Snapshot envelope v1

The proposed baseline has one selected file named `current` and immutable segment files beside it. This page fixes how a reader identifies those files, locates a product and rejects a damaged range. Each product owns the contents of its extents. The same envelope can measure multiple posting layouts without changing the publication protocol.

## Common file layout

Every integer is unsigned little-endian. Byte offsets inside a payload are relative to its first byte. The 64-byte header is followed by the payload and then one four-byte CRC32C for each consecutive 4,096-byte payload block. The final block can be short. An empty payload has no block checksums. Files have no trailing padding.

| Header byte offset | Size | Meaning |
| ---: | ---: | --- |
| 0 | 8 | Literal ASCII magic `CIXMAN01` for the selected manifest or `CIXSEG01` for a segment |
| 8 | 2 | Envelope major version, 1 |
| 10 | 2 | Envelope minor version, 0 |
| 12 | 2 | Header length, 64 |
| 14 | 2 | Flags, zero |
| 16 | 8 | Generation ID in a manifest, SegmentId in a segment. Zero is reserved. |
| 24 | 8 | Payload length in bytes |
| 32 | 8 | Root byte offset within the payload. Zero in a segment. |
| 40 | 8 | Root byte length. Zero in a segment. |
| 48 | 4 | Checksum block size, 4,096 |
| 52 | 8 | Reserved bytes, all zero |
| 60 | 4 | CRC32C of header bytes 0 through 59 |

Use CRC32C Castagnoli with reflected polynomial `0x82F63B78`, initial state and final XOR `0xffffffff`. The ASCII test vector `123456789` gives `0xe3069283`. Compute block checksums while writing the final bytes. Multiple extents can share a block. CRCs detect accidental damage within their ordinary error-detection limits. The cache is private user data and this format provides no authentication.

Validate the header checksum, exact version, constants and physical file length before trusting any offset. For payload length L the length must equal `64 + L + 4 × ceil(L / 4096)`, with checked arithmetic throughout. A 4,097-byte payload therefore ends at physical byte 4,161 and its two checksums occupy bytes [4161,4169). Verify touched blocks before exposing their contents. A pin may remember blocks it already checked for an immutable file. Account for these reads when timing cold queries. A full verifier checks every block. Both readers reject arithmetic overflow and out-of-range references.

Segment names are `segment-` followed by the SegmentId in sixteen lowercase hexadecimal digits. Check the ID in its header against the manifest. Create segment names exclusively in the repository's cache directory and skip IDs whose names already exist, including abandoned private builds. A referenced immutable name cannot be overwritten. A full ID-namespace rewrite still chooses unused segment names.

## Manifest bootstrap

The root region in a v1 manifest contains ten 24-byte descriptors in the following tag order. Each descriptor is `tag:u32, row_version:u32, offset:u64, byte_length:u64`. Its range is inside the manifest payload. Row version is 1. Nonempty ranges are pairwise disjoint and cannot overlap the root itself. Empty lists have length zero. This directory and its bootstrap tables live inline in the manifest. They can be read by range; bulky source, analysis, membership and posting data live in segments.

| Tag | Inline table | v1 representation in field order |
| ---: | --- | --- |
| 1 | Repository | Canonical absolute worktree-root path bytes on Unix, length from descriptor. Compare against the requested root before serving. |
| 2 | Predecessor | One u64 GenerationId. Zero denotes the first generation. |
| 3 | File directory root | One u64 ExtentId identifying the visible file directory. Use a real empty directory for an empty checkout. |
| 4 | Validation roots | Array of u64 ExtentIds needed to validate input observations, including eligibility and semantic support inputs. |
| 5 | Extent directory | Rows of 56 bytes sorted by extent ID: `id:u64, product:u32, codec:u32, segment:u64, offset:u64, length:u64, refs_start:u64, refs_count:u64`. The final two fields select entries in tag 6. |
| 6 | Referenced extent IDs | Array of u64 ExtentIds. IDs required to interpret a retained extent must resolve within the same manifest. |
| 7 | Products | One 24-byte row per required product in tag order: `product:u32, layout_version:u32, partition_directory:u64, coverage_root:u64`. Both roots are extent IDs. |
| 8 | Owner ranges | Rows of 40 bytes sorted by kind and first ID: `kind:u32, reserved:u32, first_id:u64, count:u64, extent:u64, first_row:u64`. Reserved is zero. Half-open ranges within a kind are disjoint. |
| 9 | ID allocation | Rows of 16 bytes sorted by kind: `kind:u32, reserved:u32, first_unused_id:u64`. Reserved is zero. Store one per allocated namespace, including ones with holes. |
| 10 | Segments | Rows of 16 bytes sorted by ID: `id:u64, payload_length:u64`. Derive the name from its ID. Check listed length against the opened segment. |

The shared-storage product tag is 0. The six query product tags, in order, are paths 1, content 2, ranked 3, symbols 4, relationships 5 and Markdown 6. Tag 0 occurs only in the extent directory. Owner kind tags are File 1, Source 2, Analysis 3, Context 4, BindingPartition 5 and Symbol 6. Allocation also uses Extent 7, Segment 8 and Generation 9. ExtentId zero is reserved throughout. Row ordinals within an owner start at zero.

A codec number identifies one physical extent representation within its product. The product's layout version defines how its reader traverses its partition directory and coverage root. A manifest with multiple runs for a key carries every run required by that product’s lookup. The envelope never infers encoding from the bytes. A proposed layout needs a distinct registered codec number and writer/reader parity fixtures before a shipping build can emit it. The fixed header, root directory, extent references, pin semantics and crash protocol apply to all candidates.

## Baseline payload profile

Product layout version **1** uses the following random-access encoding. This is a reproducible control for measuring bytes and reads. Global IDs, local ordinals, counts, positions, lengths and offsets are u64 unless explicitly sized below. Products use the same primitive codecs. Rust enum discriminants, native struct padding and host pointers never enter the file.

### Primitives and containers

A tuple concatenates its fields in listed order. Byte strings are a u64 byte length followed by exactly that many bytes. Text uses the same framing and must be valid UTF-8. Ranked term keys are byte strings because the tokenizer's byte limit can cut a UTF-8 sequence. A span is the tuple of its two u64 byte endpoints. A variant begins with a u8 tag and then its selected tuple. Number variants from zero in the order explicitly listed in the domain schema or product schema below. An optional value is tag 0 with no payload, or tag 1 followed by the value. Adding or reordering arms requires a new codec. Decode offsets and arithmetic with overflow checks before slicing.

A vector of fixed-size elements is a u64 count followed by packed elements. A vector of variable-size elements is a u64 count, **count + 1** u64 byte offsets, then the concatenated elements. Offsets are relative to the beginning of the elements, monotonically nondecreasing, begin at zero and end at the element byte length. Each slice must decode to exactly one element. An empty variable vector therefore occupies 16 bytes. A table is a vector accessed by zero-based ordinal. A dictionary is a vector of sorted distinct keys followed by an equally sized vector of values; binary search the decoded keys. Compare numbers numerically, byte keys lexicographically unsigned, and tuples field by field. Sorting little-endian key *bytes* would misorder numeric IDs.

A posting run is a tuple of three vectors: distinct sorted keys, starts and values. Starts are u64 **value ordinals** with N+1 entries for N keys, initial zero and final value count. A value vector uses the fixed or variable framing above. A run containing no keys has starts [0]. A lookup reads the two starts and then the value slice. Structural validation rejects malformed offsets, mismatched counts and trailing bytes.

### Roots and codec numbers

The manifest's `codec` is a u32 dispatched together with its product tag. For all six products, **1** is the partition directory, **2** is coverage, **3** is the lookup extent for a named stream, and **4** is an update-contribution directory. The product's partition directory is a vector of rows `(stream:u8, lower:Optional<Key>, upper:Optional<Key>, runs:Vec<ExtentId>)`. The stream tag chooses its key type from the table below. Absent boundaries denote infinities. Rows are ordered by stream and lower bound, with disjoint half-open intervals per stream. Empty directory means every stream is empty. Ordinary lookup rows reference codec 3 of the owning product, with the ranked overflow exception below. Maintenance stream 255 references codec 4. Both begin with the matching stream tag byte. An extent may be addressed by at most one stream. Multiple runs of that stream can overlap in keys. Readers merge their contributions in logical order and apply the uniqueness rules of the product.

Coverage encodes a vector of `(file:Optional<FileId>, context:Optional<ContextId>, capability:Text, state)` in key order. The state tags are complete 0, partial 1 and unsupported 2; the last two carry a reason text. One complete product-wide row with both IDs absent can establish exhaustive coverage where no narrower exceptions apply. Encode explicit file/context exceptions where required. Aggregate status and errors follow the domain's coverage rules. Coverage and a product's data roots are separate.

For updates each product also reserves stream **255** with FileId or BindingPartitionId keys (binding partition for relationships, file otherwise). Its directory row references codec **4** instead of 3. The extent begins with byte 255 followed by a dictionary from contributor ID to a vector of sorted distinct `(stream:u8, partition_lower:Optional<Key>)` pairs. The lower key uses the named stream's type. A change to partition boundaries invalidates this routing information. Products whose updates can read the complete affected contribution from their owner directories need no stream 255.

| Product | Codec 3 streams and logical payload following the stream byte |
| --- | --- |
| Paths | 1 dictionary of path text → FileId; 2 posting run of trigram → FileId. |
| Content | 1 posting run of trigram → FileId. A trigram key is three u32 Unicode scalar values in source order. Reject surrogates and values beyond U+10FFFF. Memberships for each key are distinct in ascending file ID order across the complete stream. |
| Ranked | 1 sorted term dictionary with aligned frequency and posting arrays as specified below; 2 dictionary of (FileId,ChunkId) → chunk token count; 3 dictionary of constant u64 key 0 → (visible chunk count, total token count). Field tags are path 0, body 1, identifier parts 2. Repeats occupy separate positions. Term frequency is the number of positions. An empty corpus has both corpus counts zero; an absent corpus-stats row denotes these zeros only when the ranked streams are empty. |
| Symbols | 1 posting run of spelling text → (FileId, AnalysisId, DeclId). |
| Relationships | 1 posting run of FileId → BindingPartitionId; 2 dictionary of AnalysisId → vector of (start, end, prefix_max_end, Site); 3, 4, 5 posting runs keyed by SymbolId for definitions, references and callers respectively. Each value is (BindingPartitionId, Site, role ordinal, target ordinal). Site is declaration tag 0 or use tag 1 followed by its local ordinal. Resolve optional additional navigation observations in the pinned partition. |
| Markdown | 1 dictionary of (FileId,anchor text) → SectionId; 2 posting run of (normalized path text,fragment text) → (FileId,LinkId). Compare the complete path component before comparing fragments. |

Dictionaries represent one visible value per key. A product that publishes several dictionary runs must ensure their visible keys are disjoint. Posting runs contribute slices to the same key. Ranked stream 1 is a key-owning run. After its stream byte, encode four vectors in order: sorted distinct term byte strings, visible distinct-chunk frequencies (one u64 per key), starts (N+1 posting ordinals), and ranked postings. Each posting is (FileId, ChunkId, field:u8, vector of token positions). For a run with three terms, slot 1 in the frequency vector holds DF for the second term. The same key lookup locates its postings. Keys are disjoint across ranked directory runs; DF counts each chunk once across fields. Use this encoding when the run fits the bounded write budget.

For an oversized term use ranked codec **5** in its own stream-1 directory row, bounded to the term's key range. After the stream byte it is a dictionary of term bytes → (DF:u64, vector of (first FileId, first ChunkId, posting-block ExtentId)). Each block uses ranked codec **6**, stream byte 1 followed by a vector of the same ranked postings, sorted by (FileId,ChunkId,field). The block's first pair equals its directory key. Keep a chunk together. Block ranges for one term are ordered and disjoint. The directory owns the term bytes and total DF once. List the blocks in that directory extent's reference list. Choose this indirect representation only when splitting a term materially lowers peak memory; a single indivisible chunk may stream to a final extent under the scheduler's large-item rule.

### Shared rows and typed references

Shared product 0 reserves codecs 1–6 for tables of File, Source, DocumentAnalysis, Context, BindingPartition and Symbol, in owner-kind order. The owner's first row in manifest tag 8 is an ordinal within that table. Codec 7 is the sorted vector of visible FileIds used by manifest tag 3. Codec 8 is the vector of validation observations used by manifest tag 4. Codec 9 is a raw immutable byte extent. Raw extents have no inline header; their directory length is the byte length. All other shared extents contain exactly one framed table or vector.

For shared rows, encode fields and variant arms in the order given in the [domain record definitions](../../domain.md), using the primitives above. Resolve bindings by declaration and use ordinal using separate site-aligned tables in that order. A logical slice into an explicitly named pool uses (first ordinal,count); a nested list without an explicitly named pool uses vector framing. A raw source record stores (raw byte ExtentId, first byte, byte length, vector of line starts) in the baseline dense-line codec. Store line zero's start and each byte after LF, including an empty trailing line boundary. Source byte conservation is independent of ranked-chunk end-of-file rules. A context row holds its public context-key text. A symbol row holds its typed language key and optional legacy display label. A DocumentAnalysis row appends Optional<raw ExtentId> for its owned UTF-8 pool. A binding partition stores its domain fields followed by an optional dictionary (Site, operation text, role ordinal, target ordinal) → NavigationGroupId, its local group table, support-document table and Optional<raw ExtentId> for its display UTF-8 pool. OwnedText and PartitionUtf8 offsets address their respective raw pool from byte zero. An absent pool permits no such references. Check ranges with overflow protection and require a valid UTF-8 slice. Include each pool extent in the parent extent's reference list. Every persistent span must name its source through its owner. Cross-extent refs remain valid only where the referenced owner namespace survives.

Encoding v1 file stamps use Unix device, inode and byte length as u64, then mtime seconds (i64), mtime nanoseconds (u32), ctime seconds (i64), ctime nanoseconds (u32). Signed seconds use little-endian two's complement. A validation entry carries (input key bytes, observed version). Present (tag 0) carries its stamp and one verification variant. StampOnly (0) has no payload. Captured (1) carries raw ExtentId, offset and length of the exact input bytes. InvalidTextWitness (2) carries absolute byte offset, a byte string and Optional<u64> required EOF size. The recorded subsequence and boundary must independently prove NUL or malformed UTF-8; an incomplete terminal sequence requires matching EOF. An absent input (tag 1) carries parent key, searched entry name and membership revision as in the domain. With untrustworthy stamps compare captured bytes exactly, recheck a classification witness, or reclassify a path-only input from byte zero. A lost witness forces reclassification. Recompute a required text, control or support contribution whose content cannot be proved equal. A witness only establishes binary classification. Retain raw verification extents via the validation extent's reference list; reuse the source's raw slice whenever available. Revisions and pack-defined identity components use length-framed byte keys. Encode format, capability, declaration kind, reason, namespace and operation names as their registered UTF-8 identifiers unless an explicit numeric tag is defined here. Pack-private retained evidence has a pack identifier, u32 evidence codec version and length-framed bytes. A reader must recognize that pack/version for the operation it performs.

### Example bytes

A content membership extent containing one key, `abc`, and one file, 7, uses product 2, codec 3. Its complete 61 payload bytes are below. Whitespace groups fields and is absent on disk.

```text
01                                  stream
0100000000000000                    key count
61000000 62000000 63000000          three Unicode scalars
0200000000000000                    starts count
0000000000000000 0100000000000000   first and final value ordinals
0100000000000000                    value count
0700000000000000                    FileId
```

A reader finding key `abc` reads values [0,1) and gets file 7. Changing the final start to 2 while retaining a single value is malformed, even if every block CRC is valid. Encoding a trigram as three UTF-8 bytes, delta-coding file IDs or replacing sorted runs with file-major tiles requires a new codec number and explicit reader dispatch. Those alternatives must reproduce these memberships after reopening.

## Compatibility and recovery

A reader accepts envelope 1.0 and the registered table and extent codecs it supports. An unknown required tag, unsupported version, invalid checksum, duplicate row key, truncated record or invalid extent reference is a typed open or traversal failure. Format mismatch and detected damage have distinct causes. Neither produces a successful empty query.

For a recognized older cache, follow the registered upgrade path under the refresh lease. Verify the predecessor before switching the selector, retain its files through the durable switch, and preserve active pins. An unreadable newer selector, unknown required contract or divergent semantic revision must remain intact. A process that cannot establish compatibility cannot replace the selector or reclaim its files. Detected corruption requires explicit recovery.

## Acceptance

Verify the checksum test vector, zero payload, a short final block, and a requested range crossing a block boundary. Reject a one-byte header change, a touched block with a failed CRC, a forged length that overflows, an absent required product and a dangling extent ID. A healthy query that touches one extent in a large snapshot must not checksum unrelated posting blocks. A failed incompatible-version rebuild leaves the previous selector intact. A successful rebuild uses new segment names and preserves an existing reader pin.
