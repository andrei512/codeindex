---
page-type: landing
---
# Binding

Determine which declaration a use refers to under the applicable project and language rules.

File analysis records written scopes and imports. Binding owns contextual scope membership, lookup dependencies, candidate targets and semantic coverage. For a method call it consumes receiver evidence from file analysis. It records an ambiguous or unresolved outcome when the evidence cannot establish one target. A changed import can require fresh binding for unchanged source.

A lookup is final when shadowing and overload candidates that could change it have settled. Close a group of cyclic imports together and propagate newly discovered bindings until stable. An unchanged use can require fresh binding when a searched scope gains a declaration. The [binding schema](../../domain.md#binding-and-coverage) makes these dependencies explicit.

## Topics

### Design

- [Resolution contract](resolution.md) — Follows a call through contextual lookup, scope closure and invalidation.
- [Language binding profiles](profiles.md) — Defines the contextual lookup, identity conventions, and conformance targets for the archived language modes.
