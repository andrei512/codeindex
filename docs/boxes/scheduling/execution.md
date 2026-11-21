---
page-type: reference
---
# Build execution

Refresh elects one builder per repository. Inside that build, a worker runs file-local analysis directly until a dependency, parallel partition or memory limit requires a handoff.

## Concurrent refresh

Resolve the repository identity and inspect the current manifest header. If absent, acquire the refresh lease **before discovery**. The winner rechecks the header, then inventories and builds. Cold waiters hold no workers or buffers and do no speculative inventory. The OS releases the lease if the builder exits.

For a current candidate, acquire the store’s validation-only handles and make one freshness observation tagged with that generation. On a hit acquire the pin for exactly that generation, retrying if it switched. On a miss pass the observation and changed entries into planning if they are still valid when elected. Waiting or observing a changed predecessor requires revalidation. A stable checkout should produce one elected build; each caller still needs evidence of freshness at its own observation. A root directory timestamp cannot establish that a file's contents stayed unchanged. Final prepublication validation protects a separate interval from initial discovery.

A query against a continuously changing checkout needs a terminal outcome. The baseline allows one retry after a completed observation is invalidated by a concurrent checkout edit or selector switch. On a second invalidation return a retryable refresh-conflict error. Release private work and all handles on exit. A waiting caller whose builder dies can acquire the OS-released lease. A stable checkout can complete on a later invocation. Exercise two invalidations in succession and verify that the caller exits without publishing either stale candidate. An ordinary difference between a stable checkout and its predecessor is planned work and consumes no retry.

## Batch and allocation records

```text
BuildAttempt     = opaque identity of a single private build or revalidation attempt
WorkBatch        = { attempt: BuildAttempt, operation, partition: PartitionId,
                     inputs: Slice<InputHandle>, waiting_on: PrerequisiteSet,
                     resume: Optional<OwnedCursor>, peak_extra_bytes: byte_count }
BufferLease      = { allocation: AllocationId, charged_capacity: byte_count,
                     owner: WorkerId | PartitionId | Publisher }
SealedExtent     = { id: ExtentId, product: Product, encoding_version: u32,
                     buffer: BufferLease, used_bytes: byte_count, refs: Slice<ExtentId> }
```

These records exist at schedulable boundaries. File-local loops can append directly without creating work records. One allocation has one charge even when multiple readers borrow it. Sealing transfers write authority and the existing charge; it does not clone the payload. A blocked producer releases its worker and the scheduler preserves enough credit for a consumer to drain output.

## Memory ownership

| Allocation | Writer and readers | Release point |
| --- | --- | --- |
| Captured source | One input worker fills private, stable backing. File-local consumers borrow slices. A large file can stream into private snapshot backing and be mapped from there. | Source must survive for exact verification and snippets. Reclaim the resident build buffer after a checked write to owned private backing and the last in-memory borrow. Keep the backing alive and sync at publication. |
| Parser scratch | One worker owns reusable stacks, token scratch and temporary dictionaries. | Reset at the file or batch boundary. Move surviving binding evidence into its owning batch before reset. Return oversized scratch to the budget. |
| Local facts and binding evidence | One worker appends typed rows to a file partition. Readers see sealed ranges. | Discard scratch after its last dependent task. Retain the minimum evidence needed to rebind unchanged syntax alongside the reusable analysis, including after publication. |
| Routed posting blocks | File workers fill bounded blocks for active lookup-key partitions. Transfer a completed block by handle. Allocate blocks on demand. | Release input blocks as the partition builder consumes them. Account for input, temporary counters and output while all three coexist. |
| Final index extent | One producer fills its final encoding. A publisher accepts ownership and writes it to private generation storage. | Release its buffer after the write and last in-process use. Pin published backing while any reader depends on it. |

### Credit invariant

Let **L** be the capacity of all live charged allocations, including idle reusable worker buffers. Let **P** be bytes already promised for admitted work but still unallocated. Let **R** be the remaining credit held back for draining. Maintain **L + P + R ≤ B**, where **B** is the build's managed-memory budget. Allocating from a promise moves bytes from P to L; transferring a live block keeps L constant. A drain can draw from R, then returns that credit when it finishes. Count reallocation at its simultaneous peak when old and new buffers coexist. If the peak estimate becomes insufficient, acquire more credit or yield before growth. A reservation released twice is an accounting error.

B bounds the allocations under scheduler control. Track file-backed resident pages, thread stacks, allocator overhead and process peak RSS separately to check the model against the machine. A file-backed access path still moves bytes and can cause memory pressure. Charge any explicitly pinned windows to the build.

## Batch state transitions

| Event | State and resource change |
| --- | --- |
| Register dependencies | Waiting until all prerequisites have closed, or immediately ready if none are outstanding |
| Admit a ready batch | Claim its worker slot, output ownership and incremental byte credit together |
| Suspend on a dependency | Register a continuation without losing a concurrent completion notification; yield the slot and retain charges for live buffers |
| Finish producing | Transfer output and its existing charge to its consumer; release scratch according to the worker budget |
| Abort the generation | Stop new admissions, wake waiting tasks, release registrations and leases, and discard private output after active writers stop |

Suspension must occur at a boundary with an owned continuation cursor and charged live state, or the scheduler must reserve capacity for the indivisible step before starting it. A suspended parser cannot borrow scratch that the worker resets for another batch. Resume at the saved cursor without replaying the file prefix. Only one transition may enqueue a given ready batch. Test a prerequisite completing while its dependent suspends and cancellation with a producer blocked on output capacity.

A work item belongs to one attempt. Completion of a dependency updates its waiting set once. When registration races completion, either registration observes that the prerequisite has closed or the producer wakes the registered continuation. A failed producer reports failure to its dependents. On cancellation, stop accepting new output from that attempt, join its active writers, then release buffers. A late completion cannot enqueue work or seal an extent in a replacement attempt.

## Dispatch

1. Give each worker a local deque of runnable batches. Continue the current file inline while its next step has its inputs, write ownership and enough memory credit. On idle, steal a ready batch. Split only at boundaries with independent output ownership.
2. For a missing input, record one continuation per dependent batch and return the worker. Binding owns signals such as “export scope closed”. Closing the prerequisite makes waiting batches eligible exactly once. A cyclic group propagates newly discovered bindings internally and closes only when the language's rules allow it.
3. Before starting a batch, reserve its *incremental* live bytes. A source shared with another task already has an owner and a charge. Transfer the charge with the buffer when handing it off. Reserve capacity for at least one downstream drain step, including any encoding scratch.
4. When a new batch would consume the drain reserve, stop admitting source reads. Run a ready consumer that releases charged blocks, or seal completed output to private storage. Yielding a producer also yields its worker slot. If one item cannot fit, partition it or use private file backing. Drain an **open** partition without waiting for discovery to close: spill its replay blocks or emit a private sorted run using the reserved credit. This frees capacity even if every remaining file feeds the same hot key. The final seal waits for producer closure. Exercise this case with one worker, full memory and a late undiscovered file. A permanently unadmittable task must produce an explicit resource failure.
5. At normal pressure use local FIFO admission with stealing as the baseline. Compare giving priority to ready work that can unblock the longest chain. In both cases batch or steal when independent work exists. Retain the baseline if priority bookkeeping costs more than the time it saves.

Do not require a whole-repository phase barrier to start indexing a ready file. The publication barrier is required because readers need all products from the same generation. A worker count is a ceiling.

## A one-worker pressure trace

Assume a 12 MiB managed budget with a 3 MiB drain reserve. Five MiB of exact membership blocks await scatter, 2 MiB of source remains borrowed and 2 MiB of scratch still has capacity. The 9 MiB normal allowance is full. A parser needing another block saves its cursor and yields the worker. Any live parser state moves to its continuation with its charge before the worker takes other work. A drained scratch arena remains charged until its storage is released.

The same worker now draws at most 3 MiB of reserved capacity and spills a replay block from the open key partition. The partition need not know the corpus's final size. Release that block's resident charge only after a successful transfer to private backing and its final resident borrow. Return the drain reserve, then continue extraction at the saved cursor. A failed spill, including exhausted scratch storage or file handles, aborts the private build and releases its reservations. The previous generation remains visible.

Test a late producer that emits the final membership into that same partition. It inherits registration before its parent finishes. Index sealing waits for all registered producers and all transferred blocks to drain. The fixture must make progress with one worker and verify the final membership after reopen. Count one extraction per file version and mode, zero replay of the parser's consumed prefix, and peak charged bytes within the 12 MiB allowance.

## Performance checks

| Failure mode | Observable check | Design response to test |
| --- | --- | --- |
| Idle workers with runnable work | Time with nonempty ready queues and idle capacity, separated from actual I/O wait | Steal batches; reduce the largest indivisible task |
| All workers waiting for space | One-worker run with full output and tiny memory allowance must drain or report a specific unserviceable item | Reserve drain credit; release the worker on suspension |
| Accidental repeated parsing | Count completed extractions by input version and syntax mode in a diagnostic run. Two simultaneous cold CLI requests on stable inputs must elect one builder | Coalesce shared work at the owning boundary |
| Cache or allocator contention | Compare single-worker throughput with increasing worker counts; count allocations and shared updates per batch | Reuse worker scratch; partition mutable counters and dictionaries |
| Copies hidden by a fast kernel | Track logical bytes read, routed, sorted, encoded and written separately; compare peak live bytes with reserved bytes | Remove the transfer or benchmark a different layout |
| Long serial tail | Attribute time from last producer to durable reopen to outstanding encode, writes, directory and sync | Move eligible sealing earlier; test positioned writes if the writer limits the path |

On each fixed checkout, calibrate sustainable scan bandwidth, useful compute throughput across the available core mix, and the latency of the required file and directory durability operations. Match cache conditions and working-set sizes to the full-build benchmark. Sweep worker count and batch size. If elapsed time stops falling, distinguish an exhausted ready queue from bandwidth saturation or contention using the dependency timeline, throughput, and hardware counters where available. A busy core waiting on memory contributes little useful work.

Report a range for the attainable floor with its assumptions: necessary bytes and useful operations, overlap that dependencies permit, and unavoidable serial work. Compare the measured empty-to-reopened time against that range. A claimed physical limit needs evidence ruling out redundant work as well as evidence that the remaining resource is saturated.
