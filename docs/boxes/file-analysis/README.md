---
page-type: landing
---
# File analysis

Extract facts that can be known from a document and its syntax mode.

Owns code parsers, Markdown parsing and lexical extraction. For code it emits declarations, uses and call sites. For Markdown it emits headings, parents and link destinations. Shared consumers refer to the same source spans. Parser scratch can be reclaimed as soon as its remaining consumers have their evidence.

Every supported format implements the same file-local contract: declare capabilities, borrow captured source, write typed output, then report coverage. Select the implementation once per input version and distinct syntax mode. Contexts that use the same mode share its output. Code formats may retain language-specific evidence for binding. Plain text only needs lexical analysis. The [document analysis schema](../../domain.md#repository-and-document-analysis) defines the shared fields.

## What one edit produces

A change to a function body may add a reference, a searchable word and a branch. The builder captures its new text and requests the affected products. Language analysis returns declarations and uses alongside the file-local score when scoring is enabled. Each finding carries a rule, unit name, line and observed value. Binding can then revisit the use in project context. Rendering a score needs only the saved measurements.

Markdown takes a different route through the same boundary. One heading tree supplies section ranges for navigation and section-length measurements for scoring. Moving a heading can change both its parent and the breadcrumb attached to a ranked hit. See [maintainability measurements](../../scoring/README.md) for the consumer’s view.

## Topics

### Working on language support

- [Migrate language ownership](language-migration.md) traces the existing entry points, migration order and acceptance cases for every registered language.

### Design

- [Language ownership contracts](language-ownership.md) defines the owners of analysis, project inputs, binding and edit proofs, and explains which work a build can reuse.
- [Extraction contract](extraction.md) — Specifies analyzer inputs, emitted evidence, scratch lifetime and coverage.

### Reference

- [Additional language profiles](language-profiles.md) lists file selection, declaration coverage and local relationships for twenty languages.
