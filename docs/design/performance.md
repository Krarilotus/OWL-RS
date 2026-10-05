# How NRESE is fast: the performance ideas and their evidence

The ideas behind NRESE's performance, each with where it lives and what it measured. This
is the source for the paper's performance sections. The DL engine's ideas are in
[owl2-dl-performance.md](owl2-dl-performance.md); this document covers the store, the
query engine and rule reasoning, and the performance phase that started on 5 October 2026
([plan](../plan/2026-10-05-performance-phase.md)).

Rules for every entry:
- An idea is listed with its measurement before and after, the machine and the commit or
  run. Rejected ideas stay, with the numbers that rejected them (§6).
- Numbers say what the job was: a query's rows, a store's size, the machine.
- Licensed systems (GraphDB, RDFox, Stardog, AnzoGraph) appear only by name, never with
  results, until their vendors permit it.

## 1. Where NRESE stands (office batches A and B, 3-5 October 2026)

Ryzen 9 5950X, 31 GiB, Docker; 3 runs, 3 measured repetitions each, cache off and on,
shuffled order, 300 s per query, 3,600 s per load. Run records:
[office-a](../../benches/runs/2026-10-03-office-a.toml) and
[office-b](../../benches/runs/2026-10-03-office-b.toml).

- **Queries without reasoning** (five datasets of 1.8 M to 67 M statements, materialised
  LUBM 1 to 100; QLever, Oxigraph, Virtuoso, Jena, RDF4J): NRESE has the lowest median on
  all 133 queries that it and at least one other free system answered (repeated executions, cache off). Two examples:
  - DBpedia core (67 M statements, 13 queries): 148 ms summed, against QLever's 1.5 s;
  - YAGO tiny (16.5 M, 10 queries): 229 ms, against QLever's 28.8 s.
- **OWL 2 RL materialisation** against Nemo (the same closure, plus 32 axiomatic triples):
  - LUBM 100 (13.4 M asserted, 8.7 M inferred): 6.5 s for load and reasoning, against
    Nemo's 411 s, and 127 s for nemo-sparq's LUBM-tailored rules;
  - OWL2Bench RL-1 (1.4 M inferred, mostly a symmetric-transitive clique):
    0.9 s, against Nemo's 31 min;
  - LUBM 1000 (133.6 M asserted, 87.0 M inferred): NRESE is the only system that
    completed (82 s, 14 of 14 queries). nemo-sparq ran out of memory at 28.6 GiB.
- **Where others are ahead:** store size and resident memory.
  - QLever's store is smaller (DBpedia core 2.7 GB against 3.6 GB; materialised LUBM 100
    251 MB against 536 MB), and it serves with less memory (294 MB against 498 MB).
  - NRESE's LUBM 1000 materialisation peaked at 21.7 GiB of the 31 GiB machine.

## 2. Storage and loading

| Idea | Where | Evidence |
|---|---|---|
| **Permutations as packed, block-indexed runs served from the file** (memory-mapped checkpoint): restart maps instead of rebuilding | `nrese-engine` index, checkpoint format 6+ | DBpedia core: restart 4.5 s → 0 s; resident after open 5.8 GB → 13 MiB (performance gaps §5, 1-2 Oct) |
| **Four permutations for default-graph data** (SPOG, POSG, OSPG, PSOG): the graph-first ones are rotations of these while there is one graph, and the quad layout is taken once, at the first named graph | checkpoint format 9 | DBpedia core: index 2,858 → 1,581 MiB, load 38.8 → 29.6 s, query sum unchanged |
| **Bulk loads with bounded memory**: past `budgets.bulk_load_memory`, chunks are sorted on their own pool, spilled per permutation and merged while the checkpoint is written | `engine/spill.rs` | DBpedia core: peak 5.47 → 4.02 GB at a 1 GiB budget, for 18 % more time |
| **Inline literals**: dates, date-times and integer-derived literals are ids that sort by value, so comparisons and ranges run on ids | dictionary kinds | DBpedia q05 (`xsd:nonNegativeInteger` populations) 19 → 1.3 ms |
| **The dictionary's text order** (checkpoint format 7): a prefix test is two binary searches, and string ranges follow | dictionary | Wikidata q06 `STRSTARTS` on 452 k lemmas 148 → 0.06 ms |
| **mimalloc**, with freed memory returned after loads and reasoning | `nrese-server` main | 7-20 % on query sets, 30 % on bulk loads (perf lab, 27 Sep) |
| **A process memory limit** checked inside long operations: a materialisation over it stops and applies nothing, instead of taking the machine | `nrese-exec::memory`, store pipeline | 4 Oct; 0 = off, default 75 % of the machine or container |

## 3. Query evaluation

| Idea | Where | Evidence |
|---|---|---|
| **Dictionary first for string filters**: `CONTAINS`/`STRSTARTS`/`LANG` on a large pattern's object run once per distinct term in one parallel `memmem` pass over the arena; the passing ids semi-join the scan (a bitmap when many) | `nrese-sparql` native | DBpedia q08 `CONTAINS` on labels 1,882 → 36 ms |
| **Filters as index ranges**: `YEAR(?d) = c` becomes an id range of inline dates | planner | YAGO q02 1,522 → 0.6 ms |
| **Group counts on the index**: `COUNT … GROUP BY` one variable of one pattern reads run lengths of the permutation sorted on it | group walk | Wikidata q07 2,101 → 0.18 ms |
| **Closures by components**: `p*`/`p+` through strongly connected components, a topological order and bitset reachability; `COUNT` of a closure from component sizes, without emitting pairs | paths | YAGO q08 11,432 → 24 ms |
| **Columnar scans**: a block and a column at a time, merged across the asserted and inferred stacks | scans | DBpedia q12 `SUM` over a 3-way star 206 → 41 ms |
| **Adaptive morsels and LIMIT pushdown**: morsels grow with observed fan-out; small ones probe the index; LIMIT cuts BGPs and OPTIONAL's left side | executor | Wikidata q04 3-way join, LIMIT 100 k: 3,380 → 21 ms |
| **Worst-case-optimal joins** for cyclic patterns, chunks sized to the threads | WCOJ | LUBM 100 q2 18 → 4.8 ms |
| **Sideways information passing**: a pattern joined to rows already computed is evaluated from them (index probes per row where the rows are few) | `native/sideways.rs` | Zebratlas Q03, one edge's provenance: 164 ms, then about 1,200× faster (0c014be, 4 Oct) |
| **A `VALUES` always seeds** the pattern it joins: its rows are given, and if probing doesn't pay the pattern runs alone as before | `join_sideways` | the same Q03 case; the estimate of a star's smallest pattern (7 statements) had kept it second |
| **EXISTS as sets**, semi- and anti-joins in place, branch-free and in parallel | exists | DBpedia q13 25 → 3.1 ms |
| **Results serialised on every core** | `nrese-sparql-results` | YAGO q10 (407 k rows, TSV) 269 → 90 ms |

## 4. Rule reasoning

| Idea | Where | Evidence |
|---|---|---|
| **The schema compiled first**: the TBox closure, then instance rules specialised into dispatch tables (`c ↦ sup⁺(c)`), so nearly every instance atom has a constant predicate | `nrese-reasoner` schema compiler | design reasoner-v2 §3.2 |
| **Vertical partitioning with sort-merge semi-naive evaluation**: per-predicate pair relations in both orders (32 B per fact), deduplication by radix sort and one merge, no shared hash set: deterministic at any thread count | batch executor | LUBM 100: 8.7 M inferred in 3.2 s (perf lab, 2 Oct); in the suite, load and reasoning 6.5 s vs Nemo's 411 s |
| **Modules for recursive shapes**: hierarchies and transitive properties by SCC condensation and bitset reachability; an equivalence property (symmetric and transitive) by union-find | modules | OWL2Bench RL-1, a 1.31 M-triple clique closure: 0.9 s, where generic rule engines take 15-31 min |
| **Equality by representatives**: facts over each `sameAs` class's smallest id, expanded at read time, instead of k³ copies | `representatives.rs` | property-tested against the replicated closure; ends on rule heads naming a non-representative since da4e92c |
| **The delta executor for commits**: the same compiled program over index-nested-loop joins, DRed maintenance, support counts | delta executor | commits cost their delta (e0dd495) |

## 5. Method

- **Profile before changing**: `perf` with frame pointers (`benches/probes/perf-profile.sh`).
  Top-down, the approach first, then layout, then machine code.
- **A/B with interleaved runs**, the median of at least three. A change that slows any
  other measured path by more than noise is not kept, or is kept only behind a switch
  with the reason written down.
- **Correctness first**: every optimisation has a differential test against the
  reference evaluator or an oracle (the naive rule evaluator, the full blocking
  recompute, HermiT and Konclude for DL).

## 6. Lab log of the performance phase (from 5 October 2026)

One line per idea tried: what, where, the measurement before and after (machine, data,
medians), and kept or rejected.

| Date | Idea | Measurement | Kept |
|---|---|---|---|
| 5 Oct | **P1-F1, drivers read in place.** A rule job copied every match of its driving atom (24 B each) before the round ran: 391 M copies (9.4 GB) in round 2 of LUBM 1000, the peak. The batch store now hands out numbered slices of its sorted runs (`Source::matches_len`, `scan_range`), so morsels read the runs directly; other sources still copy | Office PC, interleaved medians. LUBM 1000: peak heap 22.2 → 12.9 GB, reasoning 43.1 → 29.7 s. LUBM 100: 3.46 → 2.89 s, 2.89 → 2.51 GB. OWL2Bench RL-1: 0.54 → 0.48 s, 0.61 → 0.48 GB. Same closures | kept |
| 5 Oct | **P1-F6, delta disjoint from the recent run** (rejected). The delta was kept inside the recent run too, which looked like a second copy and a binary search per old fact | Office PC, interleaved medians. LUBM 1000: peak 12.7 → 13.1 GB, reasoning 33.0 → 34.4 s; LUBM 100 and RL-1 unchanged. Not a copy in practice: right after a fold the recent run *is* the delta, shared, and folds happen in the rounds that matter | rejected |
| 5 Oct | **P1-F7, permutations derived by a stable partition.** A permutation that is another's partitioned by a low-cardinality leading component (PSOG from SPOG, POSG from OSPG, graph-first from graph-last) is built by a counting partition of the keys' positions (4 B each) and packed by gathering through them, instead of re-keying and comparison-sorting 32-byte keys. The builder knows the permutations it will be asked for and sorts the ones the others derive from: default-graph quads take 2 sorts instead of 4, quads in named graphs 3 instead of 7. A first version partitioned the keys themselves: as fast, but +4.1 GB of load peak at LUBM 1000 | Office PC, interleaved medians, LUBM 1000: load 30.0 → 26.5 s, reasoning (inferred stack included) 34.1 → 30.2 s; load-phase peak 8.65 → 9.06 GB (positions; keys: 12.75 GB); reasoning peak unchanged. LUBM 100 and RL-1: faster or equal in the pairs before a colleague's session started on the machine | kept |
| 5 Oct | **P1-F8, candidates deduplicated and probed in order, per morsel.** Each morsel sorts its candidates by (predicate, subject, object) and removes duplicates before the membership checks: about half of an OWL 2 RL round's candidates repeat within it, and sorted probes walk a relation's sorted run forward instead of missing the cache at every level of every binary search (membership checks were 13 % of a LUBM 1000 run) | Main PC (Windows, 16 threads; an agent building alongside), interleaved medians: LUBM 1000 reasoning 45.2 → 41.8 s (B faster in each of 3 pairs), LUBM 100 3.26 → 2.99 s, RL-1 0.47 → 0.47 s; peak committed memory unchanged (15.5-15.9 GB both) | kept |
| 5 Oct | **P1-F4, the input kept apart; no list of derived facts.** Each relation of the reasoner's working set keeps the store's input in a run of its own (moved there after the first round, not copied), so what was added since, the base and recent runs, is what the materialisation derived: the separate list of derived facts (24 B each) and each round's copy of its new facts as triples are gone; a round reports only its count and whether schema facts came | Main PC (an agent building alongside), interleaved: LUBM 1000 peak committed memory 15.2 → 13.4 GB, reasoning 49.0 → 43.3 s (medians of 3); LUBM 100 4.40 → 4.27 s (B faster in 6 of 10 pairs); RL-1 0.68 → 0.57 s | kept |
