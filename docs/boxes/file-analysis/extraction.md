---
page-type: reference
---
# Extraction contract

A format implementation consumes captured bytes and a syntax mode. Its output contains just enough evidence for the enabled queries. The builder already owns the source, so extraction receives a borrow.

```text
select(format, dialect) → capabilities and extraction rules
extract(source view, syntax mode, worker scratch, typed output ranges)
    → sealed local facts, private binder evidence, coverage by capability
```

The pack registry selects owned Rust code using the captured format, dialect and syntax-affecting options. A pack can compile grammar tables during development and ship immutable tables with an iterative state machine. A handwritten scanner can implement the same entry point. Select once per analysis and call typed append operations in the hot loop. Unicode folding tables, parser tables and other versioned language data are inputs to the rules version. A rule change invalidates the products that depend on it.

At an admitted yield point the parser returns one of three outcomes. `Done` transfers sealed row ranges and coverage. `Yield` transfers the next byte cursor, explicit stack and lexical state, open output ownership and its existing memory charge. `Failed` releases provisional ranges and reports the failure. Only a completed analysis is reusable. Dialect-specific backtracking, lookahead or macro expansion that revisits tokens must identify the construct and the facts that require it. Count revisits separately from parser restarts. Conformance varies yield boundaries through strings, Unicode, nested scopes and incomplete constructs, and compares the final rows and coverage with uninterrupted extraction.

## What to emit

| Construct | Local evidence | Consumer |
| --- | --- | --- |
| Scope or declaration | Scope parent, name span, kind and owning scope | Name index and binder |
| Call or name use | Use span, role, scope and optional enclosing declaration. For a member call, retain a format-local receiver reference keyed by use ID. | Binder |
| Import alias | Local alias declaration and reference to the written import path and imported name, including namespace where the language distinguishes it | Module and name resolution |
| Document heading | Level, title, unique anchor, parent and subtree bounds | Section lookup, chunking and breadcrumbs |
| Local document link | Written or decoded destination, link span and source section | Path-qualified backlink projection |

For Markdown, keep a stack of open headings. When a heading arrives, close headings of equal or greater level and use the remaining top as its parent. Close all open subtrees at end of input. The direct text of a section stops at the next heading. Its navigation body includes its descendants. Reference-style links can use a document-local label table and pending link IDs until their destinations are known. Code-looking text inside a fenced block contributes searchable content without creating document headings.

Text consumers share line information and source spans. Trigram membership, identifier-part extraction and ranked field positions obey different token rules. Reuse their intermediate results only when those rules agree. Parser scratch resets after extraction; evidence still needed by the binder has its own owning batch. An interrupted worker cannot seal partial output as complete. A recoverable syntax error records the affected capability and preserves independently valid output.

## Acceptance

Check that a shadowed name retains its own scope, a top-level call can lack an owner, duplicate headings receive distinct anchors, and a skipped heading level still finds the correct parent. Relocating a Markdown file with unchanged bytes must permit syntax reuse while changing relative backlink keys. The [language binding profiles](../binding/profiles.md) identify the evidence and comparison fixtures for dispatch, overloads and generated constructs.

## Search fields and chunk boundaries

Use the captured UTF-8 text verbatim for source spans. An LF ends a line and belongs to that line's body. A final unterminated line counts. A final LF does not introduce an extra ranked chunk. For ordinary text emit at most 60 lines per chunk, starting at lines 1, 51, 101 and so on. Stop after emitting the first chunk reaching the end. Empty text has zero chunks. For Markdown, restart this rule at each heading's first line and stop at the next heading. The synthetic root supplies any preamble. A 61-line section produces chunks covering lines 1–60 and 51–61 relative to its own start.

Tokenize the path, body slice and identifier parts as separate fields. Compatibility uses the frozen FTS5 `unicode61` rules with underscore as an additional token character and default diacritic removal. A query uses the same token boundaries and folding. Limit each normalized term key to the first 32,768 bytes, as the frozen writer and reader do. Keep byte endpoints over the original token for highlighting. Generate identifier parts from source runs of alphanumerics or underscores. Split at underscores, lower-to-upper transitions and the final uppercase letter of an uppercase run followed by a lowercase letter. Emit the nonempty parts for words that split. For example, `HTTPServer_id` contributes `HTTP Server id` to the parts field. Keep the original word in the body. The prototype's `src/analysis.rs` and its chunk fixtures are the differential oracle.

## Markdown identity and destinations

Select Markdown for ASCII-case-insensitive extensions `md`, `markdown`, `mdown` and `mkd`. Use the archived Markdown options for tables, strikethrough, heading attributes, task lists, footnotes and GFM. Recognize ATX and setext headings, including nested list and quote containers. The subtree starts at the beginning of the heading's source line, including container markers. Assemble the title from rendered text, code and footnote-reference events, joining line breaks with a space, then trim it. An explicit heading ID supplies the base anchor verbatim. Otherwise lowercase the title, retain Unicode letters, marks, numbers, underscores, hyphens and ASCII spaces, and replace each retained space with one hyphen. In source order choose the base if free, otherwise the first available suffix `-1`, `-2`, and so on. Reserve the empty anchor for the root. For headings `# A`, `# A {#a-1}`, `# A`, the anchors are `a`, `a-1`, `a-2`.

For a parsed local link, split at the first fragment delimiter and remove the address query string before percent-decoding the address and fragment as UTF-8. Reject invalid escapes, NUL, URI schemes, protocol-relative links, backslashes in the address, and paths that would ascend above the repository. Resolve a relative address against the referring file's directory, an absolute slash against the repository root, and an empty address against the referring file. Preserve the fragment's case. Skip external links, email links, and image destinations. The archived Markdown parser and its fixtures define compatibility for syntax such as escaped or reference-style links. The emitted source offset is the opening of the link in the referring document.
