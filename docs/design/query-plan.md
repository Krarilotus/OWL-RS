# The query plan

The native executor still walks SPARQL algebra after logical rewrites. The logical plan
has n-ary joins and unions; join flattening combines eligible groups, and eager
aggregation reduces a join's inputs before its product is made. These rewrites return
algebra for execution. Direct physical-plan execution is not complete.

`native::Context::eval_operator` still selects strategies while executing: paths can be
followed from their bound end, `SERVICE` receives bindings, and LIMIT can stop work
early. BGP join order is chosen in `native/plan.rs`. These choices have not yet all
become physical plan nodes consumed by the executor.

EXPLAIN without ANALYZE already reports the rewritten logical plan and estimated rows
through `plan_query` and `native/estimate.rs`; EXPLAIN ANALYZE records the operators
that ran and their actual work. Pre-execution estimates are not a complete physical
operator plan. The current stage boundaries are summarised in
[execution-core.md §5](execution-core.md#5-planning-built-and-unfinished).

## Ownership and representation

| Owner in `nrese-sparql/src/` | Responsibility |
|---|---|
| [`plan/mod.rs`](../../crates/nrese-sparql/src/plan/mod.rs) | Logical node representation and conversion: `Plan::of` builds from borrowed algebra; `into_pattern` consumes the plan, moving its payloads into algebra. `lower` remains a borrowing compatibility wrapper |
| [`plan/properties.rs`](../../crates/nrese-sparql/src/plan/properties.rs) | In-scope variables, blank-node join dependencies and order sensitivity, read directly from nodes. Scope preserves lowering's variable order, including ordered scan runs |
| [`plan/rewrite.rs`](../../crates/nrese-sparql/src/plan/rewrite.rs) | Join flattening and eager aggregation, their eligibility rules and shared child traversal. The native caller supplies the statistics-based decision about whether pre-aggregation pays |
| [`native/estimate.rs`](../../crates/nrese-sparql/src/native/estimate.rs), [`native/plan.rs`](../../crates/nrese-sparql/src/native/plan.rs) | Logical row estimates and BGP join ordering. Estimates and runtime strategies are not a fully selected physical tree |
| [`native/path_joins.rs`](../../crates/nrese-sparql/src/native/path_joins.rs) | Path/triple join eligibility and ordering. Estimation reads logical nodes directly; execution still reads algebra. Both entries share filter-scope eligibility and final validation |

Property inspection no longer lowers subtrees to inspect them. Path-join estimation's
lower-then-inspect adapter and the separate estimate-variable adapter are retired.
Production lowering moves owned data instead of cloning it; join flattening reuses the
rewrite child traversal. The previous lowering implementation remains only in
[`plan/tests/lowering_oracle.rs`](../../crates/nrese-sparql/src/plan/tests/lowering_oracle.rs).
It checks representation equivalence, not query semantics; the independent reference
algebra evaluator remains the semantic oracle.

These properties are not a general physical-property record: cardinality estimates,
ID-table sortedness and runtime resource accounting still live with their existing
owners. Global partitioning, uniqueness and resource costs are not attached to every
logical node.

## Migration: one step at a time, never a second executor

`native::optimise` still performs two round trips: algebra → logical plan → join
flattening → algebra, then algebra → logical plan → eager aggregation → algebra.
Filter pushdown then rewrites algebra. EXPLAIN constructs a logical plan from the
rewritten algebra; operator estimates during execution can also construct one.
Consuming lowering removes payload copies, not these representation transitions.

The first round trip can retire when both rewrites share one logical plan while
preserving the scan grouping and ordered-input placement currently performed by
lowering, eager-aggregation eligibility, and the reported passes that fired. Simply
chaining the two methods on an unnormalised plan is not equivalent. Moving filter
pushdown and estimate callers to that retained representation removes their algebra
boundary; this does not require a new executor framework.

For each execution slice, remove its final lowering adapter and old production dispatch
only when the same selected nodes serve every caller, including cache lookup, tracing
and limited evaluation. They must preserve scoped cache identity, computed-term
ownership, bags, errors, negation, order, cancellation and memory/result limits. The
algebra `PathJoin::of` entry retires when execution consumes the same join representation
as estimation. Unconverted operators may keep lowering; no second permanent executor
or per-row virtual dispatch is introduced.

Keep metadata counts, group walks, sideways probes, eager aggregation, LIMIT pushdown,
component closures and cyclic worst-case-optimal joins. The
[`plan tests`](../../crates/nrese-sparql/src/plan/tests.rs) guard scope/order and moved
VALUES buffers; the [path-join tests](../../crates/nrese-sparql/src/native/path_joins/tests.rs)
guard eligibility and zero inspection lowering. Native/reference differential tests and
[route guards](../../crates/nrese-sparql/tests/it/plan_guard_tests.rs) cover execution.
These work guards establish removed conversions/copying, not lower latency; integration
and performance validation precede declaring a migrated slice complete.

## Plan parts in the result cache

Eligible operator results are parts of the result cache (`nrese_sparql::cache`): they are
kept as id columns and shared across a store's queries. A part's key is its
algebra with the variables numbered in the order they occur (so `?s ?p ?o` and `?x ?y ?z`
are one part), after the context its result depends on: the snapshot's identity, the read
model, the dataset as resolved for the user's access, the active graph, the equality
reading, the base IRI and the executor's options. A basic graph pattern is one node of the
algebra but several joins of the plan: each prefix of its join order (two patterns or
more, the whole pattern included) is a part, keyed by its triples, their range hints and
the filter conjuncts applied within it, and a pattern starts from the longest prefix
cached. Single patterns are scans, not parts: an index read costs what a copy would.
`native/cached.rs` and `cache_key.rs` still consume algebra, with special treatment for
trivial, volatile and limited evaluations. A node migration must retain those contracts
and sharing across variable renamings; replacing cache keys or adding finer invalidation
is not a prerequisite for this preparation slice.
