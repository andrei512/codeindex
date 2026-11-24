---
page-type: reference
---
# Domain model

Codeindex answers a query about **one observed repository generation**. Its core distinction is between a **declaration** written in a document, a **use** written at another location, and a **binding** that says what the use denotes under particular project inputs. Those are different domain objects with different reasons to change. The schemas below are the proposed model for alignment.

## Identity and ownership

| Aggregate root | Identity | Owns and enforces |
| --- | --- | --- |
| Document analysis | Source version and syntax mode | Local scopes, declarations, uses, chunks and format-specific records. Every source span refers to these exact bytes. |
| Binding partition | Referring file, analysis, binding context and resolution-input versions | Which symbols each declaration introduces or use denotes, and the capability coverage of this partition. Every outcome belongs to one context. |
| Generation | Repository and generation ID | Visible files, analyses, bindings, source and index extents. Publication makes the complete set visible together. |

An **entity** such as a declaration has an identity even when another declaration has the same spelling. A **value object** such as a byte span or link address is compared by its contents. A **read model** such as a callers posting is derived from entities and bindings. The scheduler owns execution state outside these aggregates. A queued task never becomes the identity of a domain object.

### Reading the schemas

`Table<K,V>` is an owned contiguous row or column array indexed by a typed ordinal K. `Slice<T>` is a start and count into the owner's contiguous T array. `Dict<K,V>` means ordered keys with associated values, such as offsets into a posting array. These are layout contracts for discussion, not Rust library types. Each aggregate has one writer while building and immutable readers after sealing. Local ordinals are interpreted with their owner. Cross-aggregate references carry the owner's identity. On disk all references use offsets or IDs.

## Repository and document analysis

```text
RepoPath         = repository-relative path in the repository's path comparison rules
SourceId         = identity of one immutable captured byte sequence
Span             = { start_byte: u64, end_byte_exclusive: u64 }
Location         = { file: FileId, span: Span }                  // interpreted under a pin
SyntaxMode       = format, dialect and syntax-affecting options
AnalysisId       = identity of (SourceId, SyntaxMode, analysis rules version)
InputVersion     = observed file identity, size and change stamps for validation
File             = { path: RepoPath, version: InputVersion, content: FileContent }
FileContent      = PathOnly | Text { source: SourceId, search_analysis: AnalysisId,
                                    syntax_by_mode: Dict<SyntaxMode,AnalysisId> }
Source           = { bytes: Slice<byte>, line_lookup: LineDirectory }
LineDirectory    = DenseLineStarts | NewlineBitmapWithRank | SparseCheckpointsWithScan
DocumentAnalysis = {
  source: SourceId, mode: SyntaxMode,
  scopes: Table<ScopeId, Scope>, declarations: Table<DeclId, Declaration>,
  uses: Table<UseId, Use>, chunks: Table<ChunkId, Chunk>,
  markdown: Optional<Markdown>, evidence: FormatEvidence,
  coverage: Table<Capability, Coverage>
}
Scope            = { parent: Optional<ScopeId>, extent: Span }
Declaration      = { name: Span, kind: DeclKind, scope: ScopeId }
Use              = { name: Span, role: UseRole, scope: ScopeId, owner: Optional<DeclId> }
UseRole          = Reference | Call | Import
Chunk            = { body: Span, section: Optional<SectionId> }
```

`SourceId` can identify captured storage; assigning it requires no content hash. `FileId` selects an entry in a generation's file table. Two files may share an analysis while retaining different paths and binding contexts. Scope, declaration, use, chunk and section IDs are local to their analysis. Cross-file references must distinguish those files. A span satisfies `0 ≤ start ≤ end ≤ byte_length`; spans exposed as text end on UTF-8 boundaries. Binary or otherwise path-only files never acquire fake empty text.

Each visible file selects one search analysis for chunking and Markdown navigation. A project context selects its syntax analysis from the mode map. Reuse the same AnalysisId for both when applicable. The source-level lexical work is shared across syntax modes. Build syntax declarations from all supported active modes; deduplicate visible results by the query contract.

A line directory must translate source positions without rebuilding the entire file's line map at query time. Test newline-dense files as well as long Unicode lines; the representation and checkpoint spacing are physical choices.

The lexical analyzer owns token normalization and field positions. A language parser owns declaration kinds and any extra evidence its binder needs. For example, a method-call binder might use a private table keyed by UseId with the receiver expression span and receiver binding evidence. A Markdown file needs no such table. The meaning and fields of each language's private evidence require that language's design; a string tagged “AST” would not specify them.

## Binding and coverage

```text
Site             = Declaration(DeclId) | Use(UseId)               // within one analysis
ContextId        = identity of a project instance and its semantic configuration
BindingPartition = {
  file: FileId, analysis: AnalysisId, context: ContextId,
  dependencies: Slice<Dependency>, outcomes: site-indexed slices of RoleBinding,
  target_pool: Array<SymbolId>, coverage: Table<Capability, Coverage>
}
Dependency       = ObservedInput(InputKey, Present(InputVersion) | Absent(DirectoryKey, Name, Revision))
                 | ObservedProjection(ProjectionKey, Revision)
ProjectionKey    = { semantic_namespace, provider, selector }
Selector         = NameMembership(name, symbol_space) | ExportSignature(symbol)
                 | ModuleMembership | LanguageDefinedProjection
RoleBinding      = { role: Definition | Reference | CallTarget, outcome: Outcome }
Outcome          = Exact(SymbolId) | Candidates(Slice<SymbolId>) | Unresolved(Reason)
SymbolKey        = Local(language, persistent file, retained declaration slot, semantic namespace)
                 | Export(language, package instance, qualified descriptor, semantic namespace)
                 | Generated(language, owner or expansion site, role, discriminator, semantic namespace)
SymbolId         = ordinal of a SymbolKey in its retained namespace
Coverage         = Complete | Partial(Reason) | Unsupported(Reason)
BoundSite        = { partition: BindingPartitionId, site: Site }    // within one pinned revision
PersistentSite   = { file: PersistentFileId, slot: DeclSlot | UseSlot }
ReverseMember    = { source: PersistentSite, context, capability, role, target: SymbolId, certainty }
```

In the keyed storage design, each generation maps its analysis-local ordinals to persistent owner slots. A persistent binding-partition key identifies the file, context and capability independently of its current payload revision. Persist reverse memberships against retained site slots and roles, then resolve them through the pinned mapping. The physical IDs remain internal. [Keyed storage](boxes/generations/keyed-storage.md)

One site can have several independently certain bindings. In a TypeScript object literal, the shorthand token in `{x}` can denote a property and read the local value `x`. Retain both role bindings at the same span. Reserve Candidates for alternatives within a role. A call-target binding contributes to caller lookup and, when required by the language contract, reference lookup. Store the target once and project it into the required directories.

Context-sensitive identities include the distinguishing context in the semantic namespace. A language supplies descriptor rules for overloads, merged declarations, generated symbols and external definitions. Two shadowed locals or identical sources at different paths must remain distinct. A role with several jointly proven targets records several exact edges; an uncertain choice records one candidate group. Reverse lookup carries the role and certainty through to the result. Within a pinned partition, role and target ordinals address the selected outcome. Across revisions the reverse membership uses the persistent source slot, role and target identity. Reordering a candidate array cannot redirect an existing reference.

A symbol can have several declaration sites. A declaration can denote different symbols in different contexts. A failed name lookup depends on the membership of the searched scope: adding a declaration can change its outcome without changing the use's source bytes. The binder seals a partition only when its relevant scopes have closed or their coverage has been recorded. “No matching callers in the searched coverage,” “no callers anywhere in a fully covered scope,” and “more callers exist than the result limit” are different answers.

### Revision and result invariants

A dependency key names the question a consumer asked of an input. Its revision changes when the observable answer changes. For a name lookup that answer includes membership, candidate identity, precedence and coverage to the extent the language uses each. A sealed empty name bucket has a revision too. A missing candidate module path records absence under its searched parent and name; adding that file invalidates the miss even though there was no file stamp to compare. The physical location of a declaration can change while a binding to its exported identity stays valid. The positions index still follows the current declaration extent. A changed symbol identity, an added overload, or a newly nearer declaration can change a binding.

Store repeated dependencies once per partition at first. A context-wide input such as a target setting can cover many sites. A language can track a broader provider revision when maintaining finer projections would cost more than the work they save. Establish reuse by version equality in the same namespace. When equality is unknown, recompute and record the new observations.

A role binding identifies one exact target, one unresolved reason, or a group of distinct candidate IDs. An empty candidate group is invalid. A semantic answer can legitimately have zero hits in a fully covered scope. Persist the capability outcome independently of its hit count so that a future reader can tell the difference.

### Navigation evidence for additional locations

A searchable site has a visible FileId and source span. A navigation answer can also refer to an external URI, a support-only document, or several distinct ranges. The binding partition owns those observations. Store them only for selectors and operations that require them. The generation pin owns the referenced documents, input observations and evidence extents.

```text
LocationDocument = Visible(FileId) | Support(SupportDocumentId)
SupportDocument  = { uri: Utf8Uri, observed_inputs: Slice<Dependency>,
                     coordinates: Captured(SourceId) | PinnedUtf16Ranges }
RangeValue       = ByteSpan(LocationDocument, Span)
                 | Utf16Range(SupportDocumentId, start_line, start_col, end_line, end_col)
NavigationRange  = { kind: Selection | TargetRange | OriginSelection | CallSite | CallerRange,
                     range: RangeValue }
CallerDisplay    = VisibleText(FileId, Span) | PartitionUtf8(offset, length)
NavigationHit    = { primary: RangeValue, symbol: Optional<SymbolId>,
                     caller: Optional<SymbolId>, caller_display: Optional<CallerDisplay>,
                     ranges: Slice<NavigationRange> }
NavigationGroup  = { target: SymbolId, target_ranges: Slice<NavigationRange>,
                     hits: Slice<NavigationHit> }
NavigationEntry  = { site: Site, role_binding_ordinal,
                     target_ordinal_in_outcome, operation, group: NavigationGroupId }
```

The generation owner directory resolves a BindingPartitionId to its immutable partition extent. Its header supplies an optional navigation directory, a local group table and a local support-document table. Both NavigationGroupId and SupportDocumentId are zero-based ordinals in these partition-owned tables. Every nested hit, range or URI slice belongs to the same retained partition or an explicitly referenced immutable extent. A missing navigation directory means zero entries. No global support-document or group lookup is required. Check the directory bounds, group and support ordinals, URI slices and required extent references under the same generation pin as the binding.

A context partition locates optional navigation entries by local site and operation, then role and target ordinals. An entry points to an immutable group; sites with the same proven target and identical answers can share the group. When the normal forward or reverse index already supplies a visible hit, attach the extra ranges to that same hit in the query result. Additional external hits and caller items without a call-site range come from the group. A prepared target with zero incoming calls has a group with target ranges and an empty hit slice. Retain each selected target's binding evidence even in this case. An observed location without an upstream symbol descriptor gets an internal identity scoped by profile, context, target URI and selection range. The legacy visible label can remain null.

Ordinary locations retain their selection range. A definition link retains the target's body and selection ranges at the target document and the optional origin selection at the source. A call hierarchy retains the selected target's selection and body on the binding; each caller retains its own selection and body, plus each call site's range in the caller. Capture the caller item’s supplied display name independently of its optional symbol. Borrow a pinned visible span only for an exact UTF-8 byte match, otherwise retain the name in the partition’s immutable UTF-8 pool. An absent name differs from a present empty string. A caller item without a call site uses its selection as the primary location. Reopen a fixture with external range-only evidence, no caller symbol, and caller name `outer lambda`: the caller hit must return that exact name from the pin. The URI of a range belongs to the document containing it. A public result can therefore contain ranges from several documents.

Use half-open UTF-8 byte spans when captured bytes exist. For range-only evidence, preserve validated ordered half-open UTF-16 endpoints (zero-based internally, one-based on output), the captured URI and the dependencies establishing validity. Missing bytes preclude local source verification; record the evidence actually available. Reuse only while its inputs and mappings remain valid. A changed or unverifiable external input requires refreshing that evidence or reporting incomplete coverage. A query cannot open the external document to reconstruct a range. Location-only support data stays outside path, content and BM25 catalogs.

### Public context keys and coverage reasons

A context has an internal ordinal and a stable public key. Its payload is the four-element list [pack ID string, repository-relative project identity string, target string, effective semantic configuration map]. Include environment, package instance, dialect and lookup options whenever they distinguish the project instance. Use the language registry mode for the pack string. Project identity is the repository-relative entry path with slash separators (use `.` for a root directory). Target is the effective target or the empty string for a target-independent profile. The map contains effective settings and their defaults under the stable names assigned by the profile. Normalize repository paths lexically under the root. Normalize external roots to their recorded package, SDK or fixture identities. Retain case and exact UTF-8 scalar sequence, including combining marks; do not case-fold or apply Unicode normalization. Sort map keys by original UTF-8 bytes. Preserve order in search paths and other ordered lists; sort set elements by their encoded bytes.

```text
key        = "ctx1/" value
string     = "s" byte_count "/" lowercase_hex_utf8
list       = "l" item_count "/" value repeated item_count times
map        = "m" pair_count "/" (string value) repeated pair_count times
integer    = "i" signed_decimal "/"
boolean    = "t" | "f"
null       = "n"
value      = string | list | map | integer | boolean | null
```

Counts and integer magnitudes use unsigned decimal digits without leading zeros except zero itself. Negative integers use a leading minus; negative zero is invalid. The hex string has exactly twice byte_count digits. As an encoding example, ["rust", ".", "", {}] becomes `ctx1/l4/s4/72757374s1/2es0/m0/`. Different effective lookup orders or package instances require different keys. Equivalent checkouts can therefore emit identical keys regardless of worker completion or generation ID.

Coverage reason strings use a fixed vocabulary: `unsupported_operation` for operations outside the pack, `generated_input_unavailable` for required generated evidence, `missing_support` for absent dependencies, `parse_error` for affected syntax, and `unverified_context` for incomplete project scope. If several affect the same public context record, choose the first applicable reason in that listed order, retaining all affected causes internally. Complete coverage has a null reason. An individual unresolved site in a fully searched scope can have no target without degrading scope coverage.

## Markdown addresses and hierarchy

```text
Markdown         = { sections: Table<SectionId, Section>, links: Table<LinkId, Link> }
Section          = { level: u8, title: DocumentName | TextRef, anchor: TextRef,
                     parent: Optional<SectionId>, subtree: Span }
TextRef          = SourceSpan(Span) | OwnedText(string_pool_offset, byte_length)
Link             = { source: Span, section: SectionId, destination: TextRef }
Address          = { path: RepoPath, fragment: normalized text }
BacklinkSelector = WholeDocument(RepoPath) | Exactly(Address)
```

Section zero is the document root. Its DocumentName title is obtained from the referring file when rendering; a filename is never embedded in reusable source-only syntax. Each other section has an earlier parent of lower heading level. Siblings have disjoint subtree spans. Heading order supplies the next heading boundary for direct-text chunking; subtree bounds supply the complete section body. A link's decoded destination may need owned text for a reference-style or escaped link. Relative addresses are qualified against the **referring file's path**. A missing destination heading still has a valid backlink key.

## A call with actual values

In file 7, suppose the source begins with `client.send(msg)`. The parser assigns use 3 the span `[7,11)`, role `Call`, scope 0 and no enclosing declaration. The four bytes at that span are `send`. Binding partition 12, for file 7 in context A, gives `Use(3) → Exact(41)`. Partition 19 for context B gives the same use `Exact(83)`. The callers list for symbol 41 contains `(12, Use(3))`; it leads to file 7 and the original span. Both contexts reuse one source and one call site. A reader can recover which context supports each answer.

## Model boundaries

The [language binding profiles](boxes/binding/profiles.md) define the distinguishing context, evidence, identity, and differential fixtures for the registered modes. The schemas here govern ownership, addressing and public normalization. Pack implementations must demonstrate the recorded observations against the pinned offline providers, including any successful legacy live answers.

## Topics

### Derived and operational records

- [Index layouts](boxes/indexes/layout.md) defines lookup structures derived from domain facts.
- [Generation format](boxes/generations/contract.md) defines persistent identities and pin lifetime.
- [Build execution](boxes/scheduling/execution.md) defines temporary batches, leases and dispatch.
