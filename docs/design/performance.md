# How NRESE is fast: the performance ideas and their evidence

The ideas behind NRESE's performance, each with where it lives and what it measured. This
is the source for the paper's performance sections. The DL engine's ideas are in
[owl2-dl-performance.md](owl2-dl-performance.md); this document covers the store, the
query engine and rule reasoning, and the performance phase that started on 5 October 2026
([plan](performance.md)).

Rules for every entry:
- An idea is listed with its measurement before and after, the machine and the commit or
  run. Rejected ideas stay, with the numbers that rejected them (§6).
- Numbers say what the job was: a query's rows, a store's size, the machine.
- Licensed systems (GraphDB, RDFox, Stardog, AnzoGraph) appear only by name, never with
  results, until their vendors permit it.

## 0. Wins to keep: check these before a rewrite, and guard them

The ideas that gave the largest measured wins. They aren't fixed: a better idea may replace
one. But a rewrite of the code they live in, or a work package handed to an agent, starts by
reading this table, and keeps the win or measures its replacement against it. Each has a
**guard**: a cheap, deterministic test that fails at once if the win is lost (counts, plan
shapes, bounds), so regressions are caught where they happen rather than by chance in large
benchmark runs.

| Win | Measured | Guard |
|---|---|---|
| Drivers read the batch store's runs in place, no copied drivers (P1-F1) | LUBM 1000 peak 22.2 → 12.9 GB, reasoning −31 % | missing: a byte counter on the batch store |
| No list of derived facts; the input kept apart (P1-F4) | LUBM 1000 peak 15.2 → 13.4 GB | missing (as F1) |
| Candidates deduplicated and probed in order per morsel (P1-F8) | LUBM 1000 reasoning −7.5 % | missing: a probe counter |
| Permutations derived by a stable partition (P1-F7) | LUBM 1000 load 30.0 → 26.5 s | `planned_layouts_sort_as_few_times_as_designed` (2 sorts default graph, 3 named graphs) |
| Group counts on the index (`COUNT … GROUP BY` one pattern) | Wikidata q07 2,101 → 0.18 ms | `group_counts_walk_the_index` (plan shows `group count`) |
| Distinct values by a group walk (NOT EXISTS sides, sets) | DBpedia q13 25 → 3.1 ms | `distinct_values_walk_the_index` (`group walk`) |
| Worst-case-optimal joins for cyclic patterns | LUBM 100 q2 18 → 4.8 ms | `triangles_join_worst_case_optimally` (`wcoj`) |
| String filters on the dictionary | Wikidata q06 148 → 0.06 ms | `string_filters_test_the_dictionary` (`dictionary string test`) |
| Sideways information passing | Zebratlas Q03 about 1,200× (§3) | `patterns_joined_to_rows_are_evaluated_from_them` (`sideways`, from 2 rows) |
| LIMIT pushdown | Wikidata q04 3,380 → 21 ms | `limits_stop_the_joins_early` (`limit pushdown`, one morsel of 1,024 rows) |
| Closures by components | YAGO q08 11,432 → 24 ms | `closures_run_by_components` (`closure`) |
| EXISTS as sets | DBpedia q13 25 → 3.1 ms | `exists_runs_once_as_a_set` (`anti join`, `semi join`) |
| Eager aggregation, `MIN`/`MAX`/`AVG` included | BSBM 10 M BI q4 subquery: 214 → 56 ms (AVG), 361 → 62 ms (MIN/MAX) | `averages_over_joins_aggregate_before_joining` (`eager-aggregation` rewrite) |
| Paths planned with the patterns | DBpedia path joins 5.7–37.9 → 0.1–0.15 ms; YAGO 82 → 2.4 ms | `selective_paths_join_first` (`paths ordered with patterns`, the path first) |
| DL: lazy unfolding in a portfolio | W3C DL-204 timeout → 6 ms | `hard_search_tests_are_decided` (DL-202, 204, 206, 661) |
| DL: minimal automata, transitions the inclusions imply (P3) | ore_ont_1066 379 k → 56 k clauses, 1.42 → 0.15 s | `universals_over_a_large_role_hierarchy_stay_small` (≤ 400 clauses; 220 on 5 Oct) |
| DL: `∃R.D ⊑ C` over a non-simple R read as `D ⊑ ∀R⁻.C` (Horn), also at the top of a subclass axiom | ore_ont_10212 gave up at 60 s → 0.24 s (HermiT 0.19 s) | `existentials_over_transitive_roles_on_the_left_stay_horn` (no disjunction with an empty body) |
| DL classification: the Horn part's subsumers taken as exact per class where its saturation reaches no left-out clause (saturation coupling), with a lower-bound budget of 60 s | ore_ont_7127 (65 k classes) 43.6 → 5.3 s, ore_ont_7956 22 → 2.8 s: no class test (Konclude 6.2 / 3.0 s, same container, 1 CPU) | `an_exact_horn_part_needs_no_class_test` (2,001 classes exact, no test) |
| DL classification: class tests with nominals start from the individuals' model built once (completion-graph reuse, Steigmiller §7.1), falling back to their deterministic state; a test runs on a pooled engine and is rolled back through the trail (no copy), and reads only the labels it changed | ore_ont_16542 unsolved in 150 s → 3.5 s, 9151 unsolved → 58.5 s, 9881 81.3 → 0.84 s, 9540 51.2 → 5.7 s, 8322 8.4 → 0.37 s; same taxonomies | `tests_with_individuals_start_from_their_model` (20 tests, < 2,000 nodes; > 6,000 without) |
| DL classification: tests with nominals run on the terminology first, kept where the probed part reaches no individual (detached probes) | ore_ont_9881 77.8 → 64.0 s, 3262 10.9 → 9.1 s (before the reuse above) | `classes_away_from_the_individuals_need_no_assertions` (4 detached, 1 rerun) |
| Canonicalisation without factorial branching | a thousand twins: linear | `many_equal_children_are_twins` (search leaves bounded) |

Plan guards live in `crates/nrese-sparql/tests/it/plan_guard_tests.rs`. A new win gets its
row and its guard in the same commit. Every operator EXPLAIN reports carries the planner's
estimate beside its actual rows (`explain_estimates_every_operator`), so `perf_lab
--qerror` measures them all.

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
| **Paths planned with the patterns**: each property path is an input of the join orderer beside the triple patterns, with its estimate alone (a closure from a constant by a search bounded to 4,096 nodes, exact below) and the distinct values of its ends; a selective path goes first and the patterns are probed from what it reaches. A path bound at both ends still follows from the smaller end | `native/path_joins.rs` | DBpedia, people born under Bavaria with names: 37.9 → 0.15 ms (1.49 M birthPlace rows no longer read) |
| **Eager aggregation**: a group over a join aggregates the side its aggregates read first (`COUNT`, `SUM`, `MIN`, `MAX`, `AVG` as sums and counts) | `plan.rs` | BSBM 10 M, BI q4's average per feature: 1.43 M join rows → 72.8 k, 214 → 56 ms |
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

**The phase's targets,** as set on 5 October (start values; progress is in the log below):

| # | Target | At the start | Goal |
|---|---|---|---|
| P1 | Materialisation memory at scale | LUBM 1000 peak 21.7 GiB (about 100 B per final statement) | half, at no more than 10 % more time |
| P2 | Serving memory and store size | QLever smaller (DBpedia 2.7 against 3.6 GB; LUBM-mat 100 251 against 536 MB); OWL2Bench RL-1 serves at 1.0 GiB on a 16 MiB store | the store within 1.2× of QLever's; serving memory explained and bounded |
| P3 | The DL pipeline outside the engine | ore_ont_1066: 982 ms whole, 258 ms engine; the Horn context core 9× slower than the EL classifier | the pipeline below the engine's time; the context core within 2× of EL |
| P4 | The heaviest queries | LUBM 1000 q06 1.7 s, q14 1.4 s, q09 0.6 s; OWL2Bench RL-1 q20 0.39 s | each cost explained by a profile; serialisation at memory bandwidth |
| P5 | Load throughput | 3.3 M quads/s overall at LUBM 1000 | parse and index build each scaling with cores |
| P6 | First executions | DBpedia: first 250 ms, repeated 148 ms | the gap explained |

**Discipline:** one target at a time on the measuring machine; every change starts from a
hypothesis out of a profile, has a differential test, is measured interleaved (medians of
at least three) on its own target and on the standard sets so no other path gets slower,
and gets a line here, kept or rejected. Two changes that shrink the same structures are
measured one at a time against the same baseline.

One line per idea tried: what, where, the measurement before and after (machine, data,
medians), and kept or rejected.

| Date | Idea | Measurement | Kept |
|---|---|---|---|
| 5 Oct | **P1-F1, drivers read in place.** A rule job copied every match of its driving atom (24 B each) before the round ran: 391 M copies (9.4 GB) in round 2 of LUBM 1000, the peak. The batch store now hands out numbered slices of its sorted runs (`Source::matches_len`, `scan_range`), so morsels read the runs directly; other sources still copy | Office PC, interleaved medians. LUBM 1000: peak heap 22.2 → 12.9 GB, reasoning 43.1 → 29.7 s. LUBM 100: 3.46 → 2.89 s, 2.89 → 2.51 GB. OWL2Bench RL-1: 0.54 → 0.48 s, 0.61 → 0.48 GB. Same closures | kept |
| 5 Oct | **P1-F6, delta disjoint from the recent run** (rejected). The delta was kept inside the recent run too, which looked like a second copy and a binary search per old fact | Office PC, interleaved medians. LUBM 1000: peak 12.7 → 13.1 GB, reasoning 33.0 → 34.4 s; LUBM 100 and RL-1 unchanged. Not a copy in practice: right after a fold the recent run *is* the delta, shared, and folds happen in the rounds that matter | rejected |
| 5 Oct | **P1-F7, permutations derived by a stable partition.** A permutation that is another's partitioned by a low-cardinality leading component (PSOG from SPOG, POSG from OSPG, graph-first from graph-last) is built by a counting partition of the keys' positions (4 B each) and packed by gathering through them, instead of re-keying and comparison-sorting 32-byte keys. The builder knows the permutations it will be asked for and sorts the ones the others derive from: default-graph quads take 2 sorts instead of 4, quads in named graphs 3 instead of 7. A first version partitioned the keys themselves: as fast, but +4.1 GB of load peak at LUBM 1000 | Office PC, interleaved medians, LUBM 1000: load 30.0 → 26.5 s, reasoning (inferred stack included) 34.1 → 30.2 s; load-phase peak 8.65 → 9.06 GB (positions; keys: 12.75 GB); reasoning peak unchanged. LUBM 100 and RL-1: faster or equal in the pairs before a colleague's session started on the machine | kept |
| 5 Oct | **P1-F8, candidates deduplicated and probed in order, per morsel.** Each morsel sorts its candidates by (predicate, subject, object) and removes duplicates before the membership checks: about half of an OWL 2 RL round's candidates repeat within it, and sorted probes walk a relation's sorted run forward instead of missing the cache at every level of every binary search (membership checks were 13 % of a LUBM 1000 run) | Main PC (Windows, 16 threads; an agent building alongside), interleaved medians: LUBM 1000 reasoning 45.2 → 41.8 s (B faster in each of 3 pairs), LUBM 100 3.26 → 2.99 s, RL-1 0.47 → 0.47 s; peak committed memory unchanged (15.5-15.9 GB both) | kept |
| 5 Oct | **P1-F4, the input kept apart; no list of derived facts.** Each relation of the reasoner's working set keeps the store's input in a run of its own (moved there after the first round, not copied), so what was added since, the base and recent runs, is what the materialisation derived: the separate list of derived facts (24 B each) and each round's copy of its new facts as triples are gone; a round reports only its count and whether schema facts came | Main PC (an agent building alongside), interleaved: LUBM 1000 peak committed memory 15.2 → 13.4 GB, reasoning 49.0 → 43.3 s (medians of 3); LUBM 100 4.40 → 4.27 s (B faster in 6 of 10 pairs); RL-1 0.68 → 0.57 s | kept |
| 5 Oct | **3.5, the datatype theory's clique check** (by the datatypes agent). 256 pairwise-unequal bytes (W3C I5.8-002): an edge-list scan in the greedy clique bound replaced by adjacency sets | 669 → 7.7 ms per check (about 10⁹ steps before); debug 122 → 7.8 s on the 256/257-byte test | kept |
| 5 Oct | **DL, lazy unfolding of definitions** (`nrese-owl` `definitions.rs`, Horrocks and Tobies 2000). `A ≡ D`, `A`'s only, acyclic definition: keep `A ⊑ D`, read each `¬A` as `¬D`, drop the universal `D ⊑ A`. Unfolding every such definition, then only those with a non-Horn reverse, then only Boolean ones (W3C suite, main PC, 20 s budget) | k_grz (DL-204) timeout → 0.2 ms with any variant. Unfolding restrictions too: k_branch (DL-661) 44 → 359 ms. Boolean only: DL-661 unchanged, but k_d4 (DL-202) 2 ms → timeout, k_dum (DL-203) 0.9 → 15.6 s, k_poly (DL-208/209) 0.3 → 1.2 s. Which clauses win depends on the ontology | kept behind the portfolio |
| 5 Oct | **DL, a portfolio of normalisations** (`tableau/portfolio.rs`). Where unfolding changes the clauses and two cores are free, the plain and the unfolded clauses race on two threads with half the memory budget each; the first decided answer cancels the other (`Config::cancel`, checked with the time budget) | W3C OWL 2 DL: 402 → 403 decided (DL-204 in 6 ms), none lost; the sum over tests both decide 4.65 → 4.89 s (+5 %: two normalisations and the threads), largest single cost +84 ms (DL-206 entailment) | kept |
| 5 Oct | **P3-a, transitions the role inclusions imply** (`Normaliser::unimplied`). Simple role inclusions are clauses (`S(x, y) → R(x, y)`), so an automaton transition on `S` beside the same transition on `R` is entailed and dropped (of mutually including roles the least is kept). Found by counting clauses per source axiom: one `DisjointClasses(A, ∃part_of.B)` gave 4,451 clauses | ore_ont_1066 (SRIQ, 52 k axioms), main PC: 379 k → 258 k clauses, engine 440 → 220 ms; same models (`clauses_and_ontologies_have_the_same_models`, 2,000 cases) | kept |
| 5 Oct | **P3-b, minimal automata where provenance needn't be exact** (`automata.rs`, `Options::exact_provenance`; the tableau reads no provenance). The automaton of a transitive role over a hierarchy of transitive subroles is spliced from theirs with ε-moves; ε-free, determinised, Moore-minimised and trimmed, it is a few states. Each role's automaton is built once and minimised before it is spliced into others; transitions carry the union of the axioms they stand for | ore_ont_1066: 258 k → 55.7 k clauses, engine about 230 → 45 ms. ORE development set, 100 consistency and instantiation tasks (main PC, 20 s budget, best of two passes each): same answers (93 consistent, 4 inconsistent, 3 undecided), whole 20.5 → 16.0 s, engine 15.3 → 11.2 s, 1.21 M → 1.00 M clauses | kept |
| 5 Oct | **P3-c, the normaliser's own costs** (profile on the office PC with `perf`). Each role's automaton prepared once (orientation, minimisation, pruning) instead of per filler; ε-closures on demand with a mark array instead of B-tree sets; GCI disjuncts sorted by their derived order instead of a `Debug` string each; foldhash for the expression interner and the clause index | ore_ont_1066, office PC: normalise 238 → 130 → 80 → 68 ms over the steps, whole run 320 → 147 ms (main PC at the start of P3: 1.42 s, normalise 0.85 s). Left: allocation (a third of the run with the system allocator), the functional-syntax reader through the lab's term table (ore_ont_1840: 0.2 s for 10.8 MB) | kept |
| 5 Oct | **DL, `∃R.D ⊑ C` over a transitive R as `D ⊑ ∀R⁻.C` at the top of a subclass axiom** (`Normaliser::sub`). Found by re-running NRESE against the stored reference results: ore_ont_10212 (ALEHIF, BFO's transitive `part_of`/`has_part`) gave up with 0 clashes and a model that only grew. Delta debugging shrank it to 15 definitions `C ≡ ∃has_part.D`; their reverse direction became `[] → C ∨ Q` (Q the automaton's start state), a disjunction at every node, whose `C` branch generates more `has_part` successors. HermiT's reading, already ours for nested occurrences, makes it Horn | ore_ont_10212: gave up at 60 s → 0.24 s whole (HermiT 0.19 s, Konclude 0.72 s). ORE dev, 100 consistency and instantiation tasks: same answers, one more decided (ore_ont_14221: gave up at 20 s → 2.0 s), the sum over tasks decided in both 36.1 → 20.1 s. W3C OWL 2 DL unchanged (403) | kept |
| 5 Oct | **Measured: where LUBM's inferred facts come from** (`benches/probes/provenance-split.py`, merge checklist §2 item 1). Each inferred fact of NRESE's OWL 2 RL closure in the first class that applies | LUBM 10 and LUBM 100 give the same shares (so LUBM 1000's 87 M inferred facts too): inherited types 71.5 % of inferred (28.2 % of all stored facts), inverses 18.2 % (7.2 %), sub-properties 6.2 % (2.4 %), rule-produced types 3.8 %, transitive 0.3 %. About 96 % of the inferred facts, 38 % of everything stored, follow from the schema and could be answered from it instead. LUBM is hierarchy-heavy: other data next (OWL2Bench, DBpedia with its ontology, Claros) | measurement |
| 5 Oct | **Eager aggregation for `MIN`, `MAX` and `AVG`** (`plan.rs`, 42040b7). `MIN`/`MAX` of the partial extremes; `AVG` as a partial `SUM` and `COUNT(*)` per early group, summed by the group and divided after it (SPARQL's division is `AVG`'s promotion; errors, unbound values and mixed kinds make the partial sum and so the quotient unbound). Checked three ways on random groups (reference, the executor's own choice, the early group forced) | BSBM 10 M (bsbmtools 0.2, 10.1 M quads, generated on the main PC), BI q4's "with feature" subquery alone: the join makes 72,801 rows instead of 1,427,507 (62,956 offers grouped into 3,215 products first), same 2,474 groups; interleaved, 5 rounds of p50 over 3 runs, main PC at 79-94 % CPU from other agents: AVG 214 → 56 ms (rounds 72-220 against 51-127), MIN/MAX 361 → 62 ms. Whole BI q4 unchanged (9.6 against 10.3 s, rounds 7.6-12.4 s): its NOT EXISTS over a cross product takes the time. No regression: dbpedia-core (13 queries), yago-tiny (10), BSBM BI (8), same rows, every median within the rounds' spread (7, 7, 5 rounds) | kept |
| 5 Oct | **Paths planned with the patterns** (`native/path_joins.rs`). A join of triple patterns and property paths is ordered as one: each path an input of the join orderer with its estimate alone (a closure from a constant by a search bounded to 4,096 nodes: exact below, at least that above; other paths by fan-outs from the index statistics) and its ends' distinct values. Before, the patterns were joined whole and the paths followed from their values. Runs of patterns stay basic graph patterns; a path after rows follows from them (the smaller end where both are bound, as before). Checked against the reference on random graphs with closures, sequences, inverses, alternatives, `?`, constants and filters | New query sets `benches/competitors/queries/{dbpedia-core,yago-tiny}-paths` (WDBench's shape: a path from a constant joined to patterns). Shapes: dbpedia q01 reads the path's 21 places and probes `birthPlace` from them (est 232, rows 51) instead of scanning 1,486,583 statements; the path's estimate 21 for 21 rows (the fan-out model had said 1.09 M). Interleaved, 5 rounds, same rows: dbpedia-core (67 M) q01 5.7 → 0.11 ms, q02 (with names) 37.9 → 0.15 ms, q03 (`*`, COUNT) 8.3 → 0.09 ms, q04 (path after a selective pattern, the same order) 12.1 → 11.4 ms; yago-tiny q01 82.4 → 2.4 ms, q02 27.8 → 1.3 ms. No regression on dbpedia-core and yago-tiny (7 rounds; yago q08 rechecked with 11: 43.3 against 42.7 ms) | kept |
