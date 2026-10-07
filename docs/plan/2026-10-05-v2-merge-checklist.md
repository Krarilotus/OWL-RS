# Before `refactor/engine-v2` goes to `main` (5 October 2026)

The owner's order: finish the current goals, benchmark them, merge to `main`, then make the
docs and README concise and refactor before v3; the new goals (STATUS G8–G13) wait. This is
the list to check before the merge. Items marked **check** are measurements that decide
whether something is built now or in v3; a number from a paper is a hypothesis until it is
measured here (benches/PROTOCOL.md, interleaved, confidence intervals).

## 1. Finish the current goals

An item is done when it is merged into `refactor/engine-v2` with its tests and guards.
State as of 5 October, evening:

| Item | Branch | State | Done so far | Left |
|---|---|---|---|---|
| DL classification and realisation (3.4); Konclude outliers (cause, fix, A/B, guard); the 7 open W3C DL cases (full OWL 2 DL coverage is a v2 requirement) | `dl/classify` | in progress | merged `ce7abad`: W2 (class tests from the individuals' model: 16542 and 9151 now solved, 9881 81.3 → 0.84 s, 9540 51.2 → 5.7 s, same taxonomies), the Horn bound's 60 s budget (7127 43.6 → 5.3 s), a soundness fix, deterministic budgets in the DL tests; W1 diagnosed (the shared empty-core context holds 25–80 % of the clauses) | DL-903 decided (b1aa08a: merge pairs that must clash are skipped, 60 s → 9 ms); W1 in the context core; then (owner's decision on scope: the v2 gate is every W3C DL case decided or documented as over budget with its cause; these two are built as fast as quality allows, no date) an algebraic number module for DL-906/907/910 (pigeonhole-hard cardinality; no reference decides them); DL-662–664 decided (OWL Lite complements as one class and its negation: 24/28/47 choices on every node → 0, gave up → 5.2 / 14.4 / 0.18 ms; Konclude answered 663 and Openllet 664 once on 3 October, not reproducible cold in 5–7 runs on 7 October); 14551; the joint re-timing |
| The store's DL mode (G3), with its correctness gate (custom user rules DL-safe in `owl2-dl` mode or their answers incomplete; tests that U1 stays an upper bound with ⊥-rules dropped, Zhou et al. 2013 Thm 1, and that existential variables matched to representative constants aren't called exact, Igne et al. 2023 §8.2.2): `/classification`, the 7 RL W3C cases through the DL path; answers as bounds with a completeness status (conjunctive queries over OWL 2 DL have no known decision procedure); routing by profile (EL parts to the consequence-based core, RL/Horn parts to rules, the rest to the tableau) with a per-axiom profile checker from the spec's grammars | `g3/dl-store` | **done** (`3a55b6f`) | owl2-dl mode; consistency on commit (U1, context core, hypertableau; minimal inconsistent axiom sets); L and U1 (own stack, equal to recomputation after random commits); the query path with both exact services and a status per answer on the shared `Completeness`; classification and realisation; the 7 RL W3C cases; explanations through the proof IR; profile checker (277 of 297 W3C cases, 20 listed); the review's checks (user rules never complete, ⊥-blocked disjunct, Skolem trap, Engine/Car/Fleet complete under owl2-dl); budgets, access control, replicas | follow-ups (`g3/follow-ups`): counts in EXPLAIN, 422 for unprovable `exact`, U1 versioned with the snapshot, the semantics in `Completeness`; then the second evaluation for bounds-equal queries, per-module dispatch, realisation in L, tighter bounds |
| Queries (G4): `MIN`/`MAX`/`AVG` in eager aggregation, paths planned with the rest | `g4/eager-aggregates` | **done** (`9c66329`) | `MIN`/`MAX`/`AVG` (`42040b7`; BSBM 10 M BI q4's average per feature 214 → 56 ms, MIN/MAX 361 → 62 ms, same 2,474 rows; no regression on dbpedia-core, yago-tiny, BSBM BI: `450c3e7`); paths planned with the rest (`81144f2`; dbpedia path joins 5.7–37.9 → 0.09–0.15 ms, YAGO 82 → 2.4 ms); characteristic pairs were done on 3 October (`be7aeef`) | review, merge |
| EXPLAIN: estimated against actual rows for every operator, the §0 operators that don't report themselves | `g4/eager-aggregates` | **done** (`9c66329`) | every operator with estimated and actual rows, the four missing guards (`3c146e9`) | review, merge |
| Materialisation memory (P1): the transient fold, unused relations | `p1/transient-fold` | **done** (merged) | guards for F1, F4 and F8 (`6411987`); the transient fold and candidates without a second copy, F9/F10 (`1b0f134`); unread object orders not kept, F11, measured alone against f2c5dea (`f8dd1f9`); LUBM 1000: committed memory while reasoning 13.49 → 9.08 GB, reasoning 49.2 → 43.1 s (interleaved medians, n = 3; LUBM 100 1.99 → 1.37 GB, 3.48 → 3.38 s) | review, merge; the load's peak (now the process's) is P5's |
| OWL 2 QL: tree-witness rewriting over the RL closure; incompleteness never silent | `ql/tree-witness` | **done** (`248d949`) | pure QL: 24,000 queries equal to a chase; mixed ontologies: 23,828 queries, none wrong, all 349 misses flagged; W3C QL 51 pass, 9 listed with causes; one `Completeness` status shared with G3; budget; graph access as the RL path | done: the printer (`ql/path-printer`); the rewriting inside `EXISTS`, `NOT EXISTS`, `MINUS` and `GRAPH` naming the default graph, negation `unsound` where it may miss (`ql/exists-graph-npd`; chase differential); NPD on data: all 31 queries equal to Ontop's answers, timings in benches/reasoning/queries/npd-stress. Left: the bag form's second evaluation (+0.9–6.1 ms on 9 NPD queries that gain nothing) |
| One error envelope across HTTP, console and CLI | – | **done** (`9dcf78c`) | every error a problem document with its request id; the fuzz test requires it | – |
| Test suite cleanup: each test kept only if it represents the owning crate's behaviour better than any other test | `v2/tests` | in progress | server: 28 tests and 3 files that repeated store, reasoner or other server tests; store: 7 tests and 2 files (duplicates, reasoner rules on vendored ontologies); the RDF crates, exec, shacl, vector and fuzz reviewed, nothing to cut | after their agents merge: sparql (store's `having_may_name_a_select_alias` and `a_bgp_of_70_patterns_runs` move there), reasoner (`src/tests/v1_scenarios.rs`; the store's ruleset and datatype tests in `mutation_pipeline_tests` move there), dl (wall-clock budgets in tests made deterministic), owl, engine |
| Upgrade path from `main`: a data directory written by `main` opens on v2 (fixture and test), or a tested migration | `v2/upgrade-path` | **done** (`85562ed`) | a whole data directory written by `main`'s server (checkpoint, WAL tail, seven repositories, access state, backup) opens on v2 with the same answers and settings; two catalogue fixes (repositories `main` accepted no longer dropped; the `.trash-` data loss); rollback documented (`docs/ops/server-setup.md` §13.1) | – |
| Kernels adopted across owners (the "one execution core" gap G1): the reasoner's probes and folds, the delta executor and the DL engines on the cursor and the radix sort | – | open | the kernels are in `nrese-engine`/`nrese-exec` (b8ffc9e) | each owner adopts them where they measure better |
| Equality, one concept with three owners (rules' representatives, the store's compact mode, the tableau's merges): one union-find with reasons (G7 started) | `perf/rules` | in progress | one union-find and one SCC kernel (G7); a regression the fuzz campaign found (representatives miss facts on some seeds) being fixed | the fix with the seeds as tests; then the persistent backend of [ADR-0011](../adr/0011-equality-one-contract-three-backends.md) (B3) |
| Tiered gate: commit, push, milestone; benchmarks never in a gate; slow suites declared by their crate | `v2/gate` | **done** | `scripts/check.sh` with the tiers commit, push and all: nextest; slow suites per crate; merge commits test only what differs from both sides; the push tier runs every random test of the changed crates on three seeds drawn from the commit, in one run per seed, and names the failing tests (`scripts/lib/fuzz-targets.txt`, DL and OWL included); build slots and the quiet slot; rust-lld; all tests 462 → 116 s | – |
| Fast regression suite (for §4's gate) | `bench/catalog-and-fast-suite` | in progress | the catalogue (`d9e580a`) | about 35 cases, NRESE in every profile, home-turf cases |

Paused until v2 is merged: the Zebratlas workstreams and every other side thread.

## 2. Measurements that decide the next reasoning gains

Ranked by expected payoff against risk to the basics (research round 3; the research agent's
ranking of 5 October).

| # | Idea | Check first | Build when |
|---|---|---|---|
| 1 | **Store less** (a materialisation policy per dataset, never the default: it is QL-style answering for the class hierarchy inside an RL store; it pays with deep hierarchies and few type queries, and costs on raw-triple reads, `rdf:type` with a variable class, exports, frequent schema changes, per-fact access labels, explanations and statistics): only asserted or rule-produced types and sub-property facts stored; inherited ones from an exact closure of the class hierarchy that queries and rules both read | **checked on LUBM (5 Oct):** inherited types 71.5 % of the inferred facts, inverses 18.2 %, sub-properties 6.2 %: 96 % of the inferred facts, 38 % of all stored ones, follow from the schema (the same at LUBM 10 and 100). **Still to check:** OWL2Bench, DBpedia with its ontology, Claros | measured per workload, against the same baseline as P1's memory changes and after them; the share holds beyond LUBM; the design keeps access labels, explanations, deletions and statistics exact |
| 2 | **Avoid rule applications** before adding cores: deltas routed only to the rule positions that consume them (GLog's trigger graphs) | **check:** a counter of redundant derivations per round on LUBM and Claros | the redundant share is large (the schema compiler already removes part of it) |
| 3 | **Equality:** canonical ids are in (compact equality, late expansion); open: incremental merges and splits | **check:** e-graph rebuilding (egg/egglog, batched congruence closure) for the open part; a differential test against axiomatised equality on `sameAs`-heavy data | the differential test passes and merges or splits are measured |
| 4 | **Machine level:** a SIMD sort for 128-bit keys in Rust (a radix sort, or a vectorised sorting network after vqsort's published method; no C++ dependency: Rust first, assembly second, decided 6 October), an Eytzinger index with prefetch for membership checks (13 % of a LUBM(1000) run), SIMD intersections, PGO | **check:** each one by A/B against today's sort; vqsort itself only as a reference point outside the build | each wins beyond the confidence interval and slows nothing |
| 5 | **Bounded tails:** a work budget per deletion with a fallback (overdelete-rederive, local counts, rematerialisation) | **check:** SSPE and clique-targeted Claros-LE in the suite | p99 over the bar below |
| 6 | **DL from the persistent closure:** bounds kept per commit, the DL engine only on the gap | part of G3 above | — |

| 7 | **Caching at least at QLever's level, then below it** (behind today: only whole serialised results per revision; QLever caches every plan subtree, shared across queries, computed once when concurrent, pinnable) | **checked (6 Oct, `perf/cache`):** replayed olympics and yago-tiny logs with counting and paging variants, interleaved, n = 3: with answers' bytes kept beside the id columns (owner's decision, 6 Oct; n = 5): 81–82 % of the queries answered whole (whole-query cache: 80 %); first rounds olympics 0.253 → 0.134 s, yago-tiny 0.722 → 0.394 s; repeats as fast (0.007 → 0.007 s, 0.015 → 0.014 s; off 0.92 and 2.85 s); cache bytes 25.8 → 57.3 and 60.8 → 73.8 MiB (performance.md lab log) | v2 (owner's decision): (1) plan-subtree results as id columns, shared, computed once, pinnable: **done** (`perf/cache`; parts are the algebra's nodes and each prefix of a basic graph pattern's joins). v3: (2) invalidation by each entry's footprint (predicates, graphs, inferred stack), then delta maintenance of cached aggregates and joins; (3) common subexpressions once per query (QL rewritings: NPD over 5,000 subqueries); (4) reuse what operators build (sorted runs, hash tables, semi-join filters, closures, compiled plans; MonetDB's recycler, HashStash); (5) the cache compressed, admitted and evicted by benefit: more hits per GB than QLever |

Deferred to v3 or later: GPU, distribution, worst-case-optimal joins inside rules.

**Targets** (to test, not promises):

| Target | Bar |
|---|---|
| Memory | the same certain answers as full materialisation on LUBM(1000) and Claros at at most a third of today's peak, with at most 10–20 % p95 penalty on type queries |
| Materialisation speed | single-threaded at or below GLog's times on its Table 4 workloads; multi-threaded below RDFox on the same hardware (internal until licensed) |
| Maintenance | single deletes under 1 ms p50; p99 under 10 ms on SSPE and clique-targeted Claros (with the fallback) |
| Basics, never traded | no regression beyond the confidence interval on the SPARQL mixes with reasoning off; loads no slower |

## 3. Benchmark (benches/PROTOCOL.md)

- [ ] A core workload set for the paper (the full list of research round 3 later), with
      the Zebratlas reasoning workload when it arrives.
- [ ] The headline track with equal resources and the best-configuration track; a 2–32-core
      curve; outcome classes; confidence intervals.
- [ ] DL: the reference comparison re-run on equal resources (the 3 October references ran
      in 2-CPU containers), all outliers against Konclude and HermiT in one run once the
      classification fixes are merged.
- [ ] **Concurrent load** (measured for no system yet): clients 1 to 64 with `query-mix` on Sparqloscope (DBLP) and BSBM explore, writes under read load; throughput against p99, peak memory per concurrent client, throughput per GB. QLever with `--num-simultaneous-queries` 1 (defaults) and = cores (best configuration).
- [ ] **QLever's published bars** for the basics: Sparqloscope on DBLP (502 M) and Wikidata truthy (7.9 B; QLever 2.43 s geometric mean, 2.0 % failed), SPARQL with text, export of large results, loading and serving dblp then Wikidata truthy. Later goals take theirs: autocompletion and maps (G11/G12), GeoSPARQL spatial joins on OSM, natural language to SPARQL (GRASP/GRISP), vector search (QUIVER).
- [ ] A **defaults track** beside the best-configuration track: every system, NRESE included,
      on its out-of-the-box settings. "No tuning needed" is a claim only with this track.
- [ ] **Home turf:** each competitor measured on what it is best at, not only on our
      workloads. For example: QLever on full Wikidata or UniProt queries and text search;
      Virtuoso on many parallel clients (BSBM explore, multi-client); Oxigraph embedded and
      small stores; Jena on TDB2 loads and its rule engine; GLog/VLog and RDFox on
      materialisation (RDFox internal only); HermiT and Konclude on classification.
      Before choosing the cases, research each one's documented use cases in the literature
      wiki. A competitor we don't beat on its own ground is a finding to work on, not a row
      to leave out.

## 4. The merge gate

- [ ] Every guard of docs/design/performance.md §0 green; every new win with its row and guard.
- [ ] W3C suites: no wrong answers; the disputed ones documented.
- [ ] The benchmark: no regression against the last recorded run beyond the confidence
      interval on any workload.
- [ ] Docs agree with the code (STATUS, the coverage plan, the configuration reference).
- [x] **Upgrade path:** a store written by `main` opens on v2, or a documented and tested migration exists (checkpoint format; a `SEMANTICS_VERSION` change rebuilds the inferred stack). Done 6 October: no migration needed. A data directory written by `main`'s server (d9a4b24; `crates/nrese-server/tests/fixtures/main-store`, with its `make.sh`) opens on v2's binary with the same data, answers, settings and logins; the stacks whose rules changed (`owl2-ql`, `owl-horst`) rebuild at start, the others are kept; main's image backup restores (`upgrade_tests.rs`). Two incompatibilities found and fixed: repositories with ids main accepted but v2 refuses were skipped at start, and a repository named `.trash-*` was deleted at start (data loss). The one answer that changes is the QL rewriting's under `owl2-ql`, on purpose. Rolling back to `main` works too. How to upgrade: docs/ops/server-setup.md §13.1.
- [ ] **Soak and fuzz clean,** each run recorded (`benches/probes/soak.py`, `scripts/fuzz-campaign.sh`).
- [ ] **Safety of the new reasoning paths:** every DL and rewriting path has a budget; access control applies to inferred and rewritten answers (no derivation from graphs the user can't read); read replicas give the same DL answers and completeness status as the primary.
- [ ] **DL scope:** every W3C OWL 2 DL case decided, or documented as over budget with its cause. No deadlines (owner, 6 October): progress as fast as possible without compromising quality.

**Timing:** the ESWC 2027 paper (deadline expected early December) needs v2's Draco runs; that informs priorities, not the quality bar.

After the merge: the concise README and docs, the major refactoring (Miri over the unsafe
code, measured test coverage, duplicate dependency versions, the last typed error, the R1
residue), then v3.
