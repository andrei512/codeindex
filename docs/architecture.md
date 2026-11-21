---
page-type: essentials
---
# Architecture

Codeindex has two application operations. **Refresh and pin** obtains a complete generation for an observed checkout. **Query** answers from that exact generation. The CLI connects them. A query can finish against its pinned source while another process publishes an update.

## From source to answer

1. Repository inputs discover eligible paths and capture immutable source. Path indexing can start as soon as membership is known.
2. A worker borrows the source once and runs ready local consumers. An owned format parser extracts declarations, uses or Markdown structure. Lexical analysis produces search keys and ranked fields. Each producer writes into typed batches.
3. Binding resolves uses against scopes, imports and project configuration. Index builders consume independent facts as they arrive. Only work with an actual dependency waits.
4. Builders seal extents in reader layout. The generation store validates their references and publishes them together. Refresh hands the caller a pin to that generation.

A box owns a decision. It can hand data to another box with an ordinary function call. Inside one builder process, mutable ranges have one writer and sealed ranges admit many readers. A handoff transfers a handle or borrows a span. The CLI and filesystem are adapters; parsing rules, binding, query meaning and index layouts belong to Codeindex.

## Ports and the hot path

| Boundary | Input → result | Ownership |
| --- | --- | --- |
| CLI → application | Working directory and typed request → result or classified error | The application holds the generation pin until the answer's borrowed fields have been written. An output error releases the pin. |
| Refresh → repository and generation store | Locate, observe, capture, acquire lease, open validation handles, publish, pin | The filesystem adapters own OS handles. A normalized path, version observation or immutable source view crosses into the domain. |
| File analysis → binding and indexes | One source view per capture, selected mode and output ranges → local ordinals, spans, and coverage | A worker owns each mutable range. Consumers share a sealed owner. Format selection occurs outside the token loop. |
| Query → snapshot reader | Pin, product key and range → checked immutable rows and source spans | The request owns result selection and formatting scratch. The storage adapter enforces the lifetime of borrowed bytes. |

The parser can call a typed writer inline. It needs neither a heap-allocated event for each token nor a queue per box. Schedule at the unit that can yield, transfer ownership, or run independently. The same domain and application operations run against the filesystem adapter and against a deterministic test adapter that can interrupt a write or delay a prerequisite.

## What changes when a file moves?

Moving unchanged Markdown from `a/notes.md` to `b/notes.md` preserves its headings and the written destination `guide.md#setup`. It changes the qualified backlink target from `a/guide.md#setup` to `b/guide.md#setup`. Reuse the syntax, update the path-dependent indexes, and publish one consistent result. Renaming only the target heading leaves the written backlink intact.

## Topics

### Refresh

- [Checkpoints and the five-minute watcher](checkpoints.md) explains file identity, incremental work and freshness when a query follows a save.
- [Publishing small edits and upgrading an index](boxes/generations/incremental.md) shows how native updates reuse bytes, when a reader can reuse an older encoding, and where path changes still trigger a bulk build.

### Contracts

- [Domain model](domain.md) defines the entities, values, owners and invariants.
- [System catalog](boxes/README.md) locates each of the eight components and its contracts.
- [Build and query cases](build-cases.md) traces ownership and required work across refresh, failure and concurrent reads.
- [Comparing implementations](conformance.md) defines independent query witnesses and experiments for physical design choices.
