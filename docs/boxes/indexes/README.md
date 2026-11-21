---
page-type: landing
---
# Indexes

These proposed contracts specify the minimum information each reader needs and what would establish parity. The six product families cover the fourteen logical outputs compared by the archived benchmark. An implementation can choose different partitions or encodings while preserving these observable results.

| Product | Writer inputs and stored lookup | Reopened-reader check |
| --- | --- | --- |
| Paths | Eligible file identity, path bytes and candidate keys | Exact, substring and glob match the visible path set, including binary paths. Candidate filtering has no false negatives. |
| Content | One versioned source plus trigram-to-file memberships | Verify literal or regex matches against the pinned bytes. When a regex has no mandatory literal, search all eligible text. Return correct positions, order and truncation. |
| Ranked chunks | File and section ranges, field-specific term positions, lengths, document frequency and corpus totals | Match chunk boundaries, scores, tie order and snippets. Shared source ranges keep the same token contributions in every overlapping chunk. Use one generation's corpus statistics. |
| Symbols | Syntax declaration, spelling, kind and source span; name-to-declaration postings | Name lookup returns every matching declaration in the expected order. Keep distinct declarations with the same spelling. |
| Relationships | Contextual identities, occurrences, definition roles, call sites, optional owners and coverage; source and target directories | Position lookup preserves tied spans. Definitions, references and callers preserve candidate identities, missing owners, stable truncation and coverage. |
| Markdown | Section ranges, ordinals, parent IDs, anchors, link locations and normalized destination keys | Match section and parent text, heading breadcrumbs, exact-anchor backlinks and whole-document backlinks. Duplicate headings and content before the first heading have distinct addresses according to the anchor rules. |

## Acceptance

Compare logical rows independently of allocated IDs. The archived comparator covers files, paths, sources, ranked chunks, declarations, semantic coverage, identities, occurrences, calls, path postings, BM25 postings, exact postings, Markdown headings and backlinks. Add public query comparisons for ranking, snippets, positions, truncation and ambiguous results. Identical posting counts alone cannot prove any of those. Include fixtures for non-ASCII coordinates, a missing anchor, an unsupported semantic construct and a rename during a build.

## Topics

### Storage

- [Index layouts](layout.md) defines posting arrays, interval lookup and the candidate build algorithms.
