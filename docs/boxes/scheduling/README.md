---
page-type: landing
---
# Build coordination

Refresh and pin owns the decision to reuse a published generation, wait for an active refresh, or build. It combines repository observations with the lease and pin supplied by the generation store. Its scheduler runs the elected build within the machine's available resources.

Repository discovery, parsers and binders decide when domain inputs are complete. The scheduler admits runnable work, accounts for live memory, and lets consumers unblock producers under pressure. Most facts move inside worker-owned batches.

## Where the current builder spends its parallelism

Capture groups files by estimated byte cost. When even the largest simultaneous jobs fit the allowance, it gives larger jobs to the least-loaded worker first. Those workers consume their assigned files independently. If the largest jobs would exceed the allowance together, workers reserve a fitting job from a shared queue. Completing a job returns its credit. An oversized file can run alone, so waiting for credit cannot permanently exclude it. The allowance controls admitted capture cost; it is not a bound on total process memory.

Fresh content postings have a different constraint: file IDs must appear in order. The builder splits contiguous ranges by estimated input bytes, produces a run per range, then joins in range order. Finishing the last file first cannot reorder the posting list. The execution contract below describes the broader scheduling design and the measurements used to judge it.

## Topics

### Execution

- [Build execution](execution.md) defines the refresh lease protocol, memory ownership, dispatch rule and performance checks.
