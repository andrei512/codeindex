---
page-type: landing
---
# File analysis

Extract facts that can be known from a document and its syntax mode.

Owns code parsers, Markdown parsing and lexical extraction. For code it emits declarations, uses and call sites. For Markdown it emits headings, parents and link destinations. Shared consumers refer to the same source spans. Parser scratch can be reclaimed as soon as its remaining consumers have their evidence.

Every supported format implements the same file-local contract: declare capabilities, borrow captured source, write typed output, then report coverage. Select the implementation once per input version and distinct syntax mode. Contexts that use the same mode share its output. Code formats may retain language-specific evidence for binding. Plain text only needs lexical analysis. The [document analysis schema](../../domain.md#repository-and-document-analysis) defines the shared fields.

## Topics

### Design

- [Extraction contract](extraction.md) — Specifies analyzer inputs, emitted evidence, scratch lifetime and coverage.
