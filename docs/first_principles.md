---
page-type: essentials
---
# First principles

**Do only the work required to answer the supported queries correctly. Do each required piece of work exactly once.**

## Design rules

1. **Start with a query.** Every stored fact has a consumer. A callers lookup justifies a reverse posting to an existing call site. A breadcrumb justifies a parent ID on a heading.
2. **Give each fact one producer.** Capture a file version once. Share source bytes, spans and local identities. Reuse syntax across contexts when its inputs agree. Resolve again where a changed import or configuration can change the answer.
3. **Own the data path.** Use Rust, its standard library and OS primitives. Own the parsers, resolver, memory layout, scheduler, indexes and snapshot reader. Extract the evidence queries require. Generating machine code, running a compiler or language server, and sending facts through SQLite or an interchange protocol add work outside this design.
4. **Keep work local until sharing pays.** Use one builder address space, disjoint mutable outputs, reusable worker scratch and bounded in-flight memory. Schedule independent batches. Introduce synchronization, a second pass or SIMD for a specific cost that can be measured.
5. **Publish the layout that queries read.** A complete generation survives reopen. A failed update leaves the previous generation usable. Distinguish partial semantic coverage from a verified empty answer.

## Acceptance boundary

A query runs from inside a Git working tree, discovers its root, honors ignore rules and tracked-file eligibility, and refreshes its local index under `~/.codeindex` without setup or a Git executable.

Preserve path, exact content, regex, ranked search, symbols, definitions, references, callers and Markdown navigation, including positions, ranking, snippets, truncation and coverage. The performance target is a **10× complete fresh build** against the baseline. Compare repeated builds under equivalent cache conditions from empty index through durable reopen.
