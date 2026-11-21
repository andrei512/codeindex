---
page-type: landing
---
# System catalog

Each box owns one subject in Codeindex. Its folder is the home for decisions about that subject. The boxes share source bytes and typed facts within one process. Folder nesting means ownership; a consumer can use another box through its contract.

## Topics

### Agent interface

- [Command-line interface](cli/README.md) — Owns invocation, output, diagnostics, and exit status.

### Engine components

- [Repository inputs](repository-inputs/README.md) — Determine which paths belong to this build and supply one immutable view of each file version.
- [File analysis](file-analysis/README.md) — Extract facts that can be known from a document and its syntax mode.
- [Binding](binding/README.md) — Determine which declaration a use refers to under the applicable project and language rules.
- [Indexes](indexes/README.md) — Turn facts into lookup structures and define how each structure is read.
- [Build coordination](scheduling/README.md) — Owns refresh and pin, including the scheduler that admits work and accounts for live memory.
- [Generation store](generations/README.md) — Publish one consistent snapshot and keep it readable for the lifetime of a query.
- [Query execution](queries/README.md) — Turn a request into a verified answer from one pinned generation.
