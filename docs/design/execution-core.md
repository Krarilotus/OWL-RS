# Execution core and native query engine (XC, Pf3, Pf4)

Status: **design**, 2026-09-27. Phase 2 of [ROADMAP §3.1](../ROADMAP.md); decision D11: one execution core for SPARQL and reasoning.
Update 2026-10-01: the fallback below is gone; the native executor evaluates every query,
and the differential tests compare it with `nrese-sparql-reference`
([migration plan](../plan/2026-10-01-oxigraph-migration.md)).

## 1. Goal

Close the query gap to QLever, and give the reasoner (R2/R4) its join machinery, with one implementation.

The basics scorecard ([SCORECARD.md](../../benches/competitors/SCORECARD.md)) locates the gap. QLever is 10–2000× faster on joins and aggregates, and every cause sits in the borrowed evaluator (spareval):
- joins build hash tables over full scans instead of using sorted order or probing indexes
- FILTERs aren't pushed into range scans
- counts and group-bys don't use index order or metadata
- every value is decoded, including intermediate ones
- nothing bounds a query's memory (the server grew from 16 to 24 GiB under 8 clients)

**Targets** (perf lab, DBpedia core 67 M, p50, against today's numbers):

| Query shape | Today | Target |
|---|---|---|
| `COUNT(*)` over everything (q11) | 10.5 s | < 1 ms (metadata) |
| Types histogram (q10) | 1.19 s | < 10 ms (grouping in sort order) |
| Join and aggregate (q12, goals per team) | 3.3 s | < 20 ms |
| Star join, constant predicates (q02, q06) | 0.5–0.8 s | < 20 ms |
| Date or number range FILTER (q04) | 0.5 s | < 5 ms (range scan) |
| Point lookups, paths, large streaming results | already ≤ QLever | no regression |

Across the whole mix, every query should be within 2× of QLever or faster (ROADMAP gate), with memory bounded per query.

## 2. Architecture

```
nrese-sparql   planner: spargebra algebra ──► native plan (fully supported) ──► nrese-exec
                                         └──► spareval (anything else, unchanged semantics)
nrese-reasoner rule bodies (R2 batch, R4 delta) ─────────────────────────────► nrese-exec
nrese-exec     IdTable, operators, joins, sort, aggregation, budgets, parallelism
nrese-engine   permutations, cursors with seek, exact range counts, statistics, dictionary
```

- **`nrese-exec` knows ids, not RDF.** It depends on `nrese-engine` for `TermId`, cursors and statistics, and on nothing SPARQL-specific. The same join serves a BGP and a rule body.
- **Whole-query switch first.** A query runs natively only if the planner supports every operator in it. Otherwise it goes to spareval exactly as today, so there's no semantic risk while coverage grows.
  - The benchmark mixes (BGP, FILTER, OPTIONAL, UNION, aggregates, ORDER/LIMIT, DISTINCT) are the first coverage target.
  - Mixed plans come later: native subtrees feeding spareval.
- **Differential by construction.** Every natively supported query can also run on spareval. The tests compare both, and so can a debug switch in production.

## 3. Storage prerequisites (XC1, format v4)

1. **Order-preserving inline encodings.**
   - Integers move from two's complement to offset binary (value + 2⁵⁹), so id order equals numeric order within the kind.
   - Dates and dateTimes already sort by local time; with the timezone in the low bits, a range scan widened by ±14 h plus an exact post-check handles offsets.
   - Decimals (sorted by scale first) are post-filtered on their inline value, still without a dictionary lookup.
2. **Literal kinds split by datatype class:**
   - `String` (plain and `xsd:string`)
   - `LangString`
   - `TypedLiteral` (everything else, including `xsd:double`)

   A numeric FILTER then scans the integer id range plus the decimal and typed-literal ranges of the predicate, and skips its labels entirely. Per-predicate kind histograms (Pf4) usually prove the typed range empty.
3. **The PSO order.** Star joins on subjects (`?s p1 ?a . ?s p2 ?b`) need each `(?s p ?o)` scan sorted by subject.
   - Patterns in one graph, including every default-graph query, are answered by the graph-first permutations (GSPO, GPOS, GOSP). None of them yields `(?s p ?o)` sorted by subject.
   - So the asserted stack gets **GPSO** as a seventh permutation. The inferred stack, where a constant graph makes graph-first and graph-last orders identical, gets **PSOG** as a fourth.
   - Union-graph patterns (SPOG, POSG, OSPG) stay without a PSO order. They're rarer, and a radix sort of the scan output covers them.
   - The +1/6 index memory is recovered by Pf1 compression. Pf6's index-set setting may later drop permutations a repository never uses.
4. **Cursor API.** Scans become cursors over (permutation, key range) with `seek(prefix)`, block-wise `next_block(&mut buf)`, and `count()`.
   - `count()` is **exact**: the sum of range sizes per run minus tombstones in range. That's O(r log n) with a per-block tombstone count (Pf1), and a scan until then.
   - Exact scan cardinalities at plan time are the planner's biggest advantage over estimate-only systems.
   - Pf1 (compressed blocks) and Pf2 (mmap) change what's behind the cursor, and nothing above it.

## 4. Data model

- **`IdTable`:** column-major `u64` columns (QLever's layout), a variable per column, and metadata: the columns it's sorted on and the number of rows. `UNDEF` is a reserved id (`TermKind` tag 15), used by OPTIONAL and by aggregates without values.
- **Batches:** operators exchange batches of up to 64 k rows. Scans, filters and index nested-loop joins stream batch by batch. Sorts, hash builds, group-by and join inputs that need full materialisation are pipeline breakers.
- **Late materialisation:** ids stay ids until the output serialiser.
  - The serialisers write TSV, JSON and N-Triples straight from dictionary bytes and inline values, without building `oxrdf::Term`s.
  - A per-query decode cache covers repeated ids.

## 5. Operators

| Operator | Algorithm | Notes |
|---|---|---|
| IndexScan | cursor over the best permutation | Constants and range filters become key bounds (§3.1). The output is sorted on the remaining key order. |
| CountScan | `cursor.count()` | `COUNT(*)` over one pattern, with no rows produced |
| Filter | vectorised over id columns | Comparisons on inline kinds need no decoding. Strings and regex decode through the cache. The full SPARQL expression language only as coverage grows. |
| MergeJoin | on inputs sorted on the join variables | the default for star and chain joins |
| GallopingJoin | exponential search in the larger input | sizes differ by more than 32× |
| IndexNestedLoopJoin | seek per left row | small left side, large indexed right side; the delta executor's join (R4) |
| HashJoin | build on the smaller input | unsorted inputs of similar size |
| Leapfrog Triejoin | n-ary, over cursors or sorted tables | cyclic BGPs (LUBM q2, q9 triangles) and ≥ 3-atom rules (R2) |
| LeftJoin | merge or hash, with UNDEF | OPTIONAL, including the filter-in-optional semantics |
| AntiJoin | merge or hash | MINUS, FILTER NOT EXISTS |
| Union | concatenation, or a merge if both are sorted | |
| Distinct | adjacent dedup if sorted, else hash | |
| GroupBy + aggregates | run-length over sorted input, else hash | COUNT, SUM, MIN, MAX, AVG and SAMPLE; numeric aggregates on inline values without decoding. `GROUP BY ?type` over `(?s a ?type)` reads POS in order: no hashing, no decoding. |
| OrderBy / TopK | id order where it equals value order (inline numbers and dates); decoded keys otherwise; a heap for `LIMIT k` | Pf5's sorted vocabulary will make id order equal lexical order for strings too, as in QLever |
| Limit/Offset, Project, Values, Bind | trivial or streaming | LIMIT is pushed down into streaming pipelines |

## 6. Planner and statistics (Pf4)

- **Cardinalities:**
  - scans are exact (§3.4)
  - joins are estimated from per-predicate statistics (count, distinct subjects, distinct objects, object kind histogram)
  - star joins use **characteristic sets** (Neumann & Moerkotte)
  - statistics are rebuilt at bulk load, checkpoint and compaction, and adjusted by commit deltas in between
- **Join order:** dynamic programming over connected subsets up to 12 patterns, greedy beyond that. The cost model covers sortedness (a merge join is free if the orders match; otherwise sort or hash) and each operator's cost.
- **Cyclic BGPs** go to Leapfrog Triejoin when the estimate says the binary plan's intermediate results exceed the output bound.
- **`EXPLAIN`:** the chosen plan with estimated and actual rows and per-operator time. It's available as a query parameter, and profile output goes into server metrics (O3).

## 7. Execution properties

- **Memory budget per query:** IdTable allocations are counted against it (a configured default, overridable per request up to a server cap). Exceeding it ends the query with a clear error rather than growing the server. Spilling is a later option.
- **Cancellation:** the token is checked per batch, which closes spareval's uninterruptible loop gap.
- **Parallelism:** rayon morsels for scans over large ranges, partitioned joins, parallel sort and parallel aggregation. The result order is only guaranteed when the query has ORDER BY, as SPARQL specifies.
- **Streaming output:** the root pipeline streams into the transport as today, which keeps the 10 M-row result at +1.9 MiB server memory.

## 8. Evidence

- **Operator tests:** each operator against a naive implementation over random inputs (proptest), including UNDEF.
- **Differential query tests:** native against spareval on a random BGP/FILTER/OPTIONAL/aggregate generator over random datasets, plus the existing differential suite. The W3C SPARQL suite runs each natively supported query both ways.
- **Coverage report:** the share of each benchmark mix and of the W3C suite that runs natively.
- **Performance:** the perf lab per sub-package against the recorded baseline, and the Docker scorecard against QLever at each gate.

## 9. Work packages

| WP | Scope | Done when |
|---|---|---|
| **XC1 Storage prerequisites** | Offset-binary integers, the literal kind split, PSOG/PSO, the cursor API with seek and exact counts; format v4 | Model tests with the new permutation; the round-trip and ordering property tests; the W3C suite unchanged |
| **XC2 Core** | `nrese-exec`: IdTable, scans, filters on ids, merge/galloping/index-NL/hash joins, left and anti joins, union, distinct, group-by and aggregates, top-k and sort, limit; budgets and cancellation | Every operator equals its naive model (proptest) |
| **XC3 SPARQL on the core** | Planner for the supported algebra, the whole-query switch, direct id serialisers, server wiring, a debug switch forcing spareval | Differential tests green; the W3C suite unchanged; perf lab shows the mix's gains |
| **XC4 Statistics and optimiser (Pf4)** | Predicate statistics, characteristic sets, DP join ordering, `EXPLAIN` | Estimation error reported; no plan regressions in the perf lab |
| **XC5 WCOJ, parallelism, coverage** | Leapfrog Triejoin, morsel parallelism, expression coverage (string functions, BIND, subqueries) | Every scorecard query within 2× of QLever or faster; LUBM q2/q9 at least 10× faster than with binary joins |

The reasoner's batch executor (R2) starts once XC2 exists and shares XC5's Leapfrog Triejoin.
