# The query plan

The native executor (`nrese-sparql/src/native/mod.rs`, 5,400 lines) walks the SPARQL
algebra and decides as it goes: `eval_operator`'s match guards pick a strategy per node
(a path joined to a pattern is followed from its bound end, a `SERVICE` gets the values
of what it is joined to, a `LIMIT` goes down to where rows are made, the smaller side of
a join runs first), and join order is chosen inside one basic graph pattern
(`native/plan.rs`). That is fast where it applies, but every new strategy is another
guard in the interpreter, nothing is planned across the algebra's node boundaries (two
basic graph patterns joined to each other are ordered separately), and EXPLAIN can only
report what ran.

## Target

1. **A logical plan** built from the algebra: scans, paths, n-ary joins (a group's
   patterns, subgroups and paths as one join), left joins, filters, unions, minus,
   extensions, groups, orders, projections, slices, values, `SERVICE`, graph scopes, and
   `EXISTS` as semi- and anti-joins where it is correlated that way.
2. **Rewrites as named passes** over it, each with its own tests: filter placement (now
   `pushdown.rs`), joins across groups, paths from their bound end, limit placement,
   bind joins to `SERVICE`, the triple-term rewrite. EXPLAIN lists the passes that fired.
3. **A physical plan** that chooses per node the operator (index probe, merge, hash,
   leapfrog, semi-join reduction, grouped or factorised aggregation) with estimated
   rows, so EXPLAIN without ANALYZE shows the plan before anything runs; and the
   aggregate algebra over products (BSBM BI q4) as a rewrite on it.
4. **The executor over the physical plan**; `mod.rs` split into plan, execution,
   functions and federation.

## Migration: one step at a time, never a second executor

Each step keeps the executor's answers, checked by the differential tests against the
reference evaluator, the fuzz campaign, and the perf lab (no query set slower).

1. The logical plan, built from the algebra and lowered back to it; the round trip is the
   identity on the test corpora. The executor runs on the lowered algebra.
2. The first rewrites on the plan, lowered before execution: groups joined to each other
   become one join (so their patterns are ordered together), then the rewrites that now
   live in match guards, one by one.
3. Estimates on the plan (the store's statistics, characteristic sets), the physical
   choices, EXPLAIN before running.
4. The executor reads the plan instead of the algebra, node kind by node kind; the algebra
   interpreter shrinks with each.

## Plan parts in the result cache

Every operator the executor evaluates is a part of the result cache (`nrese_sparql::cache`,
merge checklist §2 item 7): its result is kept as id columns and shared by every query of
the store, as QLever keeps the result of every subtree of its plan. A part's key is its
algebra with the variables numbered in the order they occur (so `?s ?p ?o` and `?x ?y ?z`
are one part), after the context its result depends on: the snapshot's identity, the read
model, the dataset as resolved for the user's access, the active graph, the equality
reading, the base IRI and the executor's options. A basic graph pattern is one node of the
algebra but several joins of the plan: each prefix of its join order (two patterns or
more, the whole pattern included) is a part, keyed by its triples, their range hints and
the filter conjuncts applied within it, and a pattern starts from the longest prefix
cached. Single patterns are scans, not parts: an index read costs what a copy would.
Until the executor reads the physical plan (step 4), the parts are the algebra's nodes and
the join prefixes; then they become the physical plan's subtrees.
