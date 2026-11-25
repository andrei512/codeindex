---
page-type: essentials
---
# How a reader survives an edit

An answer can require a posting list, a source line and a heading range. A generation gives them a common lifetime. The query holds a pin while it follows references between these records.

Suppose a reader finds a Markdown heading at byte 200. An editor inserts a paragraph above it before the reader requests the section body. If the reader applied its old offsets to the new bytes, it could return a different section. Its pin keeps the original bytes available. A subsequent query can acquire the newly published generation and see the heading at its new offset.

The native writer can retain ranges from earlier parts and publish changed bytes in a new part. A selector identifies the published generation. Until publication succeeds, readers can continue opening the previous one. Existing-file edits can share large unchanged ranges. Changes to path membership can still require a bulk build because several postings refer to files by generation-relative ordinal. This is the reason the storage design investigates stable owners for postings.

## Topics

### Updates and compatibility

- [Publishing small edits and upgrading an index](incremental.md) distinguishes what the native writer reuses today from the proposed stable-owner layout.
