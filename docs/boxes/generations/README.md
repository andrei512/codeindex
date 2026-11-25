---
page-type: landing
---
# Generation store

Publish one consistent snapshot and keep it readable for the lifetime of a query.

Owns the input manifest, extent directory, version compatibility, the cross-process refresh lease, durable publication and reader pins. It accepts sealed extents from the index owners. A reader that spans a publication continues using the same source and indexes until it releases its generation.

## Topics

### Reading and refreshing

- [How a reader survives an edit](reading.md) follows the old and new versions of one document through publication.

### Storage

- [Generation format and publication](contract.md) defines the manifest, extent references, pin lifetime and crash guarantees.
- [Snapshot envelope v1](wire.md) defines file offsets, manifest tags, checksums and compatibility.
