# Query progress with shared computations

Status: replacement design, not implemented or performance-qualified. The owner
requested a proper design on 10 October, fixing ownership and replacing obsolete
coordination rather than accumulating workarounds. The earlier intermediate
recomputation proposal is not approved. This is part of the
[checkpoint campaign](2026-10-10-performance-campaign.md#user-request), PR #23.

## Problem and evidence

The cache shares unfinished eligible operator results through `Flight`. A consumer
waits on its condition variable. The store's Runtime now executes native queries on
a retained physical Rayon pool. These are individually understandable mechanisms,
but their composition can prevent progress:

1. Query A owns a cache flight and starts a parallel kernel.
2. A worker helping that kernel executes an independently submitted query B.
3. B waits for A's flight while still occupying the helper's stack.
4. A cannot publish until its kernel returns; the helper cannot return until B does.

The forced two-worker Office test reproduced this ordering and required watchdog
cancellation of B. Its exact source, log and exit status are retained in the campaign
root as `flight-diagnostic.patch`, `flight-diagnostic-test.log` and
`flight-diagnostic-exit.txt`. The test establishes the dependency cycle, not its
frequency or contribution to the measured throughput regression.

Rayon 1.12.0, as pinned in Cargo.lock, documents both work stealing and the risk of
blocking communication between joined work. A condition-variable timeout or a call
to yield does not remove the stack dependency. See the upstream
[join contract](https://docs.rs/rayon/1.12.0/rayon/fn.join.html) and
[pool contract](https://docs.rs/rayon/1.12.0/rayon/struct.ThreadPool.html#method.install).

## Design direction: retain the operation, release its worker

SPARQL owns an operation's continuation: the remaining control flow and the values
already computed. Run it on the existing workers until it completes or needs an
external result. Return the whole operation to its synchronous caller while waiting;
then resume it on the same physical pool. Do not restart the query or replay its
completed operators. No additional global scheduler is needed for this design.

```text
query caller                     existing Runtime workers
     | -- owned query task ----> advance through ready operators/kernels
     | <--- task + Waiting ----- cache result not ready; worker returns
     | wait for existing flight  workers can finish its producer
     | -- task + shared Part --> continue from saved operator position
     | <--- completed result --- retained IDs/reservation or output block
```

Waiting is a normal execution state, not a query error. Advancing a ready task does
not submit every operator separately: it keeps executing within one worker entry
until an actual suspension or completion. Existing parallel kernels keep their
Rayon joins. There is no scheduling operation per row.

| Owner | Responsibility |
|---|---|
| `nrese-sparql::native` | One owned query task, operator frames, scoped evaluation state, cache/result conversion and semantic continuation |
| `nrese-sparql::cache` | Semantic identity, one active producer per claimed flight with existing failure/reentrant exceptions, publication, retention/admission and pinning |
| `nrese-exec::Workers` | Existing physical CPU pool and bounded kernel dispatch; no RDF/cache keys or query interpreter |
| `nrese-store::Runtime` | Existing shared workers and budgets; no copied SPARQL task scheduler |
| Synchronous query caller | Wait for a task's external dependency and submit its continuation; retain writer/backpressure ownership |

This moves dependency coordination above a single cache wait, into the semantic
operation that owns the unfinished work. It does not move query algorithms into
the store or server. The existing logical plan is not an executable physical plan;
completing that separate migration is not a prerequisite for a progress repair.

## State and lifetime invariants

- Preserve the current scope of compute-once. Unpinned BGP/VALUES evaluation and
  BGP-prefix lookup/offer are not currently protected by flights. Do not expand
  that contract to every kernel or prefix as part of this repair.
- A task owns one Context and executes only one segment at a time. Context stays
  movable, not concurrently mutable; replacing its RefCells with global locks is
  unnecessary. Preserve computed IDs, NOW/BNODE state, substitutions, aliases,
  graph and LIMIT scopes, trace positions and memory reservations across suspension.
- Frames own completed child results and the exact next operation. Use moved
  prepared patterns or stable node handles; no references into a movable frame
  vector and no full AST cloning on each resume.
- Replace `Computing<'c>`'s borrowed cache reference with an owned claim guard.
  It must not borrow a cache stored inside its own movable Context. Reuse the
  existing publication/drop path through an Arc-backed cache owner.
- Flight ownership identifies the logical producer/claim, not an OS thread.
  ThreadId cannot identify recursion once a task can resume on a different worker.
  Trace actual claim lineage for self-dependency; do not silently broaden Bypass.
- Carry the completed `Arc<Part>` to the continuation. A result shared with current
  waiters need not be admitted to the retained cache, so looking it up again is wrong.
- Cancelling a waiter drops its continuation and interest, not the producer's
  cancellation token. Preserve producer-failure behavior; do not quietly introduce
  a detached producer or new retry policy. Frame and claim cleanup must release
  reservations and notify dependent operations on errors and unwinding.
- Publication and waiter registration use one synchronization protocol, including
  completion before the caller starts waiting. Reuse Flight's existing locked
  completion predicate rather than adding an unrelated wakeup flag.
- A suspended task retains real memory. Existing configured accounting must include
  its retained intermediate results and owned frame capacity; suspension is not a
  new memory limit and cannot make retained bytes disappear from accounting.

## Full suspension surface

Changing only `eval_cached` or `eval_operator` is insufficient. Read-only source
reviews identify control propagation across roughly 36 Context methods, including:

| Area | State to retain; existing functionality to reuse |
|---|---|
| Algebra children, joins and grouping | Completed child tables and selected strategies; reuse ID joins, grouping, projection, sort and WCOJ kernels |
| GRAPH and LIMIT | Current graph/iterator, accumulated result, scoped limits and restoration; preserve early termination |
| EXISTS, OPTIONAL, LATERAL and sideways evaluation | Current binding/substitution, child context and row position; never repeat volatile expressions or prior callbacks |
| Set and late-equality paths | Selected projection/count strategy and canonical child evaluation; preserve specialized routes |
| Vector bind joins | Existing nearest-candidate cursor, seen/kept candidates and accumulated probe results; its synchronous keep callback currently evaluates a cacheable local query |
| SERVICE | Endpoint/chunk position, bound values and continuation after the callback; local `nrv:search` remains native CPU work |
| Entry points | Direct writing, retained typed results, EXPLAIN and update WHERE share the same operation driver |

Synchronous kernels remain synchronous where their call graph cannot wait for an
external query dependency. In particular, existing early-filter eligibility excludes
EXISTS; keep that distinction rather than turning all row expressions into tasks.
The reference evaluator remains an independent test oracle, never a production fallback.

## Boundary requiring an explicit decision

`ServiceClient::select` is an opaque synchronous application callback currently run
inside native evaluation. It can itself call the query API. Returning a suspended
child to that callback does not release the worker on which the callback is waiting.
Internal continuation conversion alone therefore cannot promise general progress.

The lean proposed boundary is to return a SERVICE effect to the caller, invoke the
existing callback there, and resume native result encoding, decoding and joins on the
worker pool. This preserves its synchronous API and exact invocation count, using
the same external-effect separation already used for writers. It **changes callback
CPU ownership**: application-supplied callback work is outside the engine's physical
worker cap. This is not authorized merely by the instruction to design the repair.

Likewise, a synchronous embedding that deliberately calls the store from inside its
own Runtime worker cannot be treated as an external waiting caller. The design must
expose a composable task/step entry for such embedding, and explicitly settle the
sync-wrapper contract before implementation. Silently rejecting previously accepted
calls, spawning spare threads, or running engine computation on callers is not a fix.

Alternatives are a resumable callback API (a broader public API migration), or
stackful suspension of arbitrary synchronous callbacks (a substantially larger,
platform-sensitive runtime change). Adding `async` to internal methods alone does
not make a blocking user callback suspendable. A choice here is required before
claiming the complete replacement is implementable under every existing constraint.

## Replacement and code-size ledger

Retain Workers, Runtime, cache keys/admission/ranking/pins, Part conversions, ID-table
and index kernels, semantic strategies, cancellation primitives and the independent
oracle. Replace recursive control edges with explicit continuation state in the same
executor. Delete converted recursive dispatch and worker-side cache waiting in the
same migration; no second permanent evaluator or bypass mode survives the gate.

Replace thread-based producer ownership and borrowed claims. Retain the condition
variable only as an external synchronous wait adapter if that boundary is accepted.
Consolidate entry-point coordination into one driver. Do not claim removal of the
existing encoder re-entry unless a separate measured change actually removes it.

The full strict-sharing migration is **low thousands of touched lines**, principally
replacing existing control flow, rather than the earlier 200–350-line contention
bypass. Its net production size may grow because native call-stack state becomes
explicit state. A net deletion or speedup cannot honestly be promised now. The review
criterion is fewer coordination mechanisms and explicit ownership, with a measured
line/deletion ledger for each implementation slice.

## Alternatives considered

| Approach | Assessment |
|---|---|
| Move whole-query admission before pool entry | Useful for root contention; intermediate dependencies and worker reentry remain |
| Recompute contended intermediate parts | Bounded alternative, but relaxes sharing and may duplicate CPU/memory; not the selected design direction |
| Yield, spin, timeout or add spare workers | Does not remove the demonstrated stack cycle, or violates physical worker ownership |
| Reserve every possible part before evaluation | Dynamic GRAPH, substitutions and selected strategies make exact preflight nontrivial; conservative serialization risks lost concurrency |
| Serialize all cached queries | Sacrifices independent-query throughput and does not establish the desired architecture |
| Generic dependency graph / new async runtime | More machinery than the synchronous API needs; consider only if caller-driven continuations prove insufficient |

## Verification and performance gate

1. Prove every cache-wait path leaves the physical worker; separately establish that
   semantic dependencies cannot form a logical cycle. Finite producers, cooperative
   cancellation and the callback/embedding contract are explicit proof assumptions.
   Removing worker starvation alone does not prove arbitrary user callbacks terminate.
   The logical-cycle proof is still outstanding and is an implementation acceptance
   prerequisite, not something established by the forced scheduling test.
2. Keep the forced two-worker reproduction and add one-worker and nested-kernel
   variants. Gate progress without watchdog rescue. Preserve eight-call compute-once,
   non-admitted result sharing, pinning, renamed variables and snapshot isolation.
3. Exercise cancellation/failure at each suspension point, callback reentry and exact
   callback counts, NOW/BNODE, correlated expressions, GRAPH/LIMIT, vector local probes,
   error paths and reservation release. Compare bags/order/taxonomies where applicable,
   not only result counts. Normal Office Rust gates remain mandatory.
4. Measure cache disabled, warm hits, cold uncontended, identical-query contention and
   partially overlapping requests. Retain QPS by client width, p50/p99, CPU/work,
   allocations and phase/lifetime memory. The no-wait path must not gain a queue hop
   per operator. Retest both hosts using verified Office-built candidates.
5. Keep this progress repair separate from the skewed triangle diagnosis: that timed
   case primarily counts weighted forest messages, not the SIMD intersection routine.
   Profile its one-worker and full-width paths before changing the relevant kernel.
6. Reject regressions and remove obsolete code only after equivalence and affected
   performance checks. Three outer pairs screen; ten pairs support published claims.

The local research catalog supports ready-work dispatch and measuring useful reuse,
not a promise of universal gains: `leis2014morsel` describes dispatching work only
after its prerequisites finish and measuring scheduling granularity/locality;
`ivanova2010recycler` evaluates reuse benefit against computation, matching and retained
memory. Both texts were consulted under the read-only `literature/corpus/stores/execution/`
catalog. Their reported gains are not OWL-RS measurements.
