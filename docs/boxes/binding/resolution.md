---
page-type: reference
---
# Resolution contract

Binding computes the meaning of a declaration or use for a particular file and project context. Its input is sealed syntax evidence plus the versioned definitions, configuration and module membership consulted during lookup.

```text
bind(file, analysis, context, dependency view) → sealed BindingPartition
lookup during construction → Waiting(prerequisite) | Exact(symbol)
                           | Candidates(symbols) | Unresolved(reason)
```

Waiting is internal build state. The scheduler can suspend the affected batch and run another producer. Only final outcomes enter the published partition. A partition records its dependencies, target pool and per-capability coverage using the [domain records](../../domain.md#binding-and-coverage).

## Work through a call

For an aliased import followed by `client.send()`, use the local scope chain to find the receiver binding, resolve the alias in the module's namespace, then use the language's receiver evidence to look up the member. Preserve declaration identity across spelling aliases. Record each consulted input and the searched namespaces whose membership could change the result. A nearer declaration can invalidate a previous success; adding an export can invalidate a previous miss.

Make export declarations available as their batches close. For cyclic imports, group dependent scopes and propagate newly established facts until further progress stops. Use the language's closure rule to decide whether an answer is final. Missing support input or unsupported generated code produces uncertainty and coverage evidence. It cannot establish an exhaustive empty result.

## What can be reused?

The stable build key is (file, analysis, context, binding-rules version). A previous result for that key is reusable when all its recorded observations still hold. The dependencies are discovered **during** binding. Requiring their complete set as a scheduling input would force the resolver to run merely to look up its previous answer. One in-flight producer may own the key within an attempt. If the build retries against changed inputs, invalidate the old attempt before accepting new results.

Retain the minimum rebind evidence in the sealed analysis, including receiver and import evidence. It can live in immutable storage after publication. An unchanged caller must be able to rebind without reopening the checkout file or reparsing its source. A retained span can still borrow its captured bytes. Record consulted dependency keys while looking up names; deduplicate the observed revisions at partition seal. Once the inputs are stable, one worklist visit for a newly available fact should wake only the batches waiting for it.

| Change | Required work | Reuse check |
| --- | --- | --- |
| A comment shifts an exported method to a later line | Replace the edited file's locations. Preserve the export revision if the visible signature, symbol identity and lookup behavior agree. | An unchanged caller whose only dependency here is that export keeps its binding. Definition navigation returns the new line. |
| An overload changes the possible target of an unchanged call | Rebind the affected partition, replace its previous reverse edges in the same generation, and preserve its analysis. | Lookups for the old target cannot return an obsolete caller; the receiver expression has one extraction across the two builds. |
| A nearer scope gains a binding after an earlier miss | Recheck the use that observed the nearer name bucket. | A matching outer declaration alone cannot justify reusing the old answer. |
| Two contexts share syntax but choose different imports | Look up each context's bindings and produce contextual outcomes A and B. | The local call span and its receiver evidence have one owner. An answer selected for context A cannot include B's targets. |

## Closing a cycle

Suppose modules A and B re-export each other. B also declares `send`. Treat the mutually dependent scopes as one group once the dependency is known. Capture each external prerequisite, seed the group with local declarations, and propagate newly supported lookup facts along the relevant import edges. Duplicate support for the same identity in the same scope adds no new fact. Close when external producers have closed and the internal worklist is exhausted under the language's lookup rules. Publish stable outcomes after closure. A candidate discovered early may lose to a higher-precedence declaration.

Exercise the same graph with `send` removed. The group must finish with a recorded closed lookup or explicit uncertainty. A batch that is waiting solely on a member of its own group cannot occupy a worker or keep the group open indefinitely. If the language requires evidence the binder cannot supply, publish the documented capability reason. An I/O or cancellation failure aborts the attempt.

## Acceptance evidence

For each fixture, compare contextual targets, roles, coverage and reverse postings after reopen. During a dependency-only edit count source captures and syntax extractions for the unchanged caller, unique binding partitions attempted, worklist insertions, and bytes retained for rebind evidence. Compare conservative provider revisions with finer name buckets on both a comment edit and an export edit. The [language binding profiles](profiles.md) specify identity and conformance fixtures for overloads, generated syntax and fallback coverage.
