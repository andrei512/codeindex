---
page-type: landing
---
# Build coordination

Refresh and pin owns the decision to reuse a published generation, wait for an active refresh, or build. It combines repository observations with the lease and pin supplied by the generation store. Its scheduler runs the elected build within the machine's available resources.

Repository discovery, parsers and binders decide when domain inputs are complete. The scheduler admits runnable work, accounts for live memory, and lets consumers unblock producers under pressure. Most facts move inside worker-owned batches.

## Topics

### Execution

- [Build execution](execution.md) defines the refresh lease protocol, memory ownership, dispatch rule and performance checks.
