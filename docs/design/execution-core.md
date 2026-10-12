# Execution core and native query engine (XC, Pf3, Pf4)

Ownership follows [ARCHITECTURE.md](../ARCHITECTURE.md); the remaining query-plan migration is in
[query-plan.md](query-plan.md). Measured wins and their guards live in
[performance.md §0](performance.md#0-wins-to-keep-check-these-before-a-rewrite-and-guard-them).

## 1. Goal

Keep query and reasoning work on encoded ids, reuse the kernels they actually share,
and bound intermediate memory. Sharing kernels does not mean sharing one executor:
SPARQL algebra, rule evaluation and DL search keep their own semantics and control flow.

## 2. Architecture

```text
nrese-sparql    SPARQL algebra -> logical rewrites -> native algebra executor
nrese-reasoner  batch and delta rule executors
                    | each uses the kernels it needs
nrese-exec     id tables, joins, grouping, closures, sort, search, memory budgets
nrese-engine   dictionary, permutations, snapshots, probe cursors, counts, statistics
```

- **`nrese-exec` knows ids, not RDF or storage.** Its
  [manifest](../../crates/nrese-exec/Cargo.toml) has no NRESE dependencies. Tables hold
  `u64` columns; operations needing values accept a caller's resolver. Storage cursors
  and statistics belong to `nrese-engine`, and callers connect them to execution.
- **The native SPARQL executor is the only production evaluator.** Queries and update
  evaluation use it. There is no spareval fallback, mixed plan or production reference
  switch. `QueryOptions::as_written` disables optimisations within the native executor.
- **Reuse is at the kernel boundary.** The reasoner's rule joins remain its own; it
  reuses sort, search, closures and memory monitoring where applicable. A query's BGP
  planner and worst-case-optimal join remain in `nrese-sparql`, not `nrese-exec`.
- **The reference evaluates the algebra independently.** Tests compare native answers
  with `nrese-sparql-reference`, which uses neither the native optimiser nor its algebra
  execution; it shares expression evaluation and aggregate values with production, so
  those need the W3C suites and an independent engine ([Jena](../../benches/oracle/README.md)).

The kernel boundary is described in
[`nrese-exec/src/lib.rs`](../../crates/nrese-exec/src/lib.rs); the query entry points and
options are in [`query.rs`](../../crates/nrese-sparql/src/query.rs).

Within the native executor, `numeric.rs` owns SPARQL numeric promotion and arithmetic
for both expressions and aggregates. `aggregate.rs` owns aggregate value helpers;
group planning, scheduling and result interning remain in the executor. Calendar and
OWL datatype semantics retain their separate owners. Owned ID-table projection moves
selected column buffers and preserves the surviving sort prefix; selecting a column
more than once copies only its additional occurrences.

### Execution and resource lifetime

This section owns the execution policy; there is no setting for it besides `dl.threads`.

- **Queries run on their caller.** Native evaluation, update `WHERE`, result encoding and
  output stay on the thread that asked; a parallel kernel (joins, sorts, closures, spill)
  forks into Rayon's global pool and joins before it returns. There is no dedicated query
  pool and no catalog-wide CPU cap. Writers and liveness callbacks run on the caller, and
  `SERVICE` callbacks run inside evaluation, on whichever thread evaluates.
- **The store's `Runtime` owns the native query budget.** A catalog and its system store
  share one; each query and update `WHERE` reserves from it. Live native results keep
  their reservation until they are dropped. `QueryOptions` and `UpdateOptions` carry one
  `Resources`: the cancellation token, the memory limit, the shared budget and a
  `Workers` handle (the current Rayon registry by default).
- **One reservation type.** `nrese_exec::Reservation` returns its charge to its `Budget`
  when it drops, on every path including errors and unwinding. Query tables, retained
  solution tables and their appended bound rows, the DL gap's scratch and the vector
  collector's candidates use it; there is no second charge-on-drop mechanism.
- **`Workers` is the DL batch width.** DL classification, realisation and exact
  candidate batches take a `Workers` handle sized by `dl.threads` (0: every core;
  `Workers::new(n)` with `n` ≤ 1, or a pool that can't start, runs serially). A narrower
  handle bounds the batch's own dispatch; it is not a semaphore
  inherited by nested Rayon work, so child work partitions its allowance explicitly.
  Full-width batches keep Rayon's work stealing; narrow ones use bounded lanes, an idle
  lane claiming the next probe while results keep their order.
- **One absolute deadline per DL operation.** A DL operation computes its deadline once;
  every phase (consistency, gate evidence, entailment batches, classification) measures
  what remains from it rather than restarting a timeout, and checks the request's
  cancellation token at the same points. Tableau portfolio siblings share a child token
  that cannot cancel their request; the first decided outcome in input order is returned.
- **What the budgets don't cover.** Accounted capacities are not process RSS or a strict
  allocation ceiling: decoded graph payloads, encoder buffers, DL compilation scratch and
  runtime overhead have their own lifetimes, and DL task capacity is separate from the
  query budget. Rule materialisation, bulk spill and background storage keep their own
  execution owners. Cache waits and their progress are discussed in
  [research-designs.md §8](research-designs.md#8-query-progress-under-shared-cache-waits-proposal).

SPARQL's [`SolutionTable`](../../crates/nrese-sparql/src/native/typed_results.rs) retains
its snapshot, computed terms and capacity reservations. Alignment rejects unrelated
dictionaries or different variable lists, then remaps query-local computed values before
ID comparison. The store owns lower/upper orchestration and candidate completeness;
SPARQL owns decoding and result lifetime. This avoids serialising and evaluating the
lower result again without treating raw IDs from separate contexts as interchangeable.

## 3. Storage access

The engine owns term encodings and sorted permutations, including asserted GPSO and
inferred PSOG for predicate scans ordered by subject. The native executor reads through
[`Snapshot`](../../crates/nrese-engine/src/engine/snapshot.rs): `scan_sorted_in`,
`count_in` and `estimate_in` have distinct contracts. Exact counting may need to scan;
it is not a universal constant-time metadata operation.

[`ProbeCursor`](../../crates/nrese-engine/src/engine/snapshot/cursor.rs) retains a
position per run and seeks forward for index joins and leapfrog intersection. Compressed
and mapped storage stay behind engine APIs. There is no shared execution-core cursor
that owns the dictionary or storage layout.

## 4. Tables and operators

`IdTable` holds column-major ids and sortedness metadata. `UNDEF` represents an unbound
value; computed ids refer to values owned by the query. Terms are resolved when value
semantics require them or when writing results; direct result writers avoid constructing
an RDF term for every output cell.

| Owner | Implemented work |
|---|---|
| `nrese-exec::join` | Merge/galloping joins on sorted keys, hash joins otherwise; left, semi and anti joins; explicit handling of unbound keys and output limits |
| `nrese-exec::group`, `table` | Grouping and column operations, with value-dependent operations supplied by the caller |
| `nrese-exec::sort`, `search`, `graph` | Sorting id keys, forward searches, adjacency and component-based closures |
| `nrese-sparql::native` | Scans, index probes, cyclic BGP joins, expression evaluation, paths, aggregation strategies, ordering, streaming and result writing |
| `nrese-reasoner` | Rule plans, semi-naive rounds, candidate admission, equality and incremental maintenance |

Batch sizes and parallel thresholds vary by operator; there is no universal 64 k-row
pipeline. Some paths stream and others materialise tables. Per-query and shared budgets
bound accounted intermediates, with cancellation checked by the executor and participating
kernels. They do not imply that every allocation is charged or every kernel is interruptible.
Query sort/distinct/group disk spilling remains unfinished; on-disk bulk-load spilling
is a separate, implemented engine path. Controls and limitations are mapped in
[hardware-and-scaling.md](hardware-and-scaling.md#configuration-and-strategy-ownership).

## 5. Planning: built and unfinished

| Stage from [query-plan.md](query-plan.md) | Current implementation |
|---|---|
| 1. Logical plan and lowering | Built in `nrese-sparql/src/plan/`: properties inspect nodes directly and consuming lowering moves buffers; execution still consumes lowered algebra |
| 2. Named rewrites | Join flattening and eager aggregation are built; additional strategy decisions still live in the native interpreter |
| 3. Estimates, physical choices, EXPLAIN before execution | Partly built: `plan_query` estimates the rewritten logical plan without executing it; BGP order uses statistics and characteristic sets/pairs. A complete physical operator plan is unfinished |
| 4. Execute the physical plan | Unfinished: `native::Context::eval_operator` still dispatches on algebra nodes |

[`native/plan.rs`](../../crates/nrese-sparql/src/native/plan.rs) chooses BGP join order
by dynamic programming through 12 patterns, then greedy search. These and the probe-cost
thresholds are code heuristics, not hardware-calibrated settings.
[`native/estimate.rs`](../../crates/nrese-sparql/src/native/estimate.rs) supplies
pre-execution estimates; `explain_query` executes and records actual operator work.
An estimated logical plan must not be presented as a fully selected physical plan.

## 6. Evidence and maintenance

- Kernel tests compare joins, sorts and closures with simpler models, including unbound
  keys where supported.
- [Native differential tests](../../crates/nrese-sparql/tests/it/native_differential_tests.rs)
  compare with the reference evaluator; the W3C suites check conformance against their
  expected results. Neither is a fallback-coverage report.
- [Plan guards](../../crates/nrese-sparql/tests/it/plan_guard_tests.rs) and the guards in
  performance.md §0 protect retained algorithms and work bounds. Refactoring a caller
  must preserve its semantics and these guards; moving code alone establishes no speedup.
- Performance measurements follow [CONTRIBUTING.md](../../CONTRIBUTING.md) and the
  [benchmark protocol](../../benches/PROTOCOL.md); their state is in [STATUS.md](../STATUS.md).
