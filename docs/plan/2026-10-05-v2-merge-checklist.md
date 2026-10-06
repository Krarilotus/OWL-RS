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
| DL classification and realisation (3.4); Konclude outliers (cause, fix, A/B, guard); the 7 open W3C DL cases (full OWL 2 DL coverage is a v2 requirement) | `dl/classify` | in progress | merged `ce7abad`: W2 (class tests from the individuals' model: 16542 and 9151 now solved, 9881 81.3 → 0.84 s, 9540 51.2 → 5.7 s, same taxonomies), the Horn bound's 60 s budget (7127 43.6 → 5.3 s), a soundness fix, deterministic budgets in the DL tests; W1 diagnosed (the shared empty-core context holds 25–80 % of the clauses) | DL-903 decided (b1aa08a: merge pairs that must clash are skipped, 60 s → 9 ms); W1 in the context core; then an algebraic number module for DL-906/907/910 (pigeonhole-hard cardinality; no reference decides them); then conflict-driven learning for DL-662–664 (thrashing; Konclude decides 663, Openllet 664); 14551; the joint re-timing |
| The store's DL mode (G3): `/classification`, the 7 RL W3C cases through the DL path; answers as bounds with a completeness status (conjunctive queries over OWL 2 DL have no known decision procedure); routing by profile (EL parts to the consequence-based core, RL/Horn parts to rules, the rest to the tableau) with a per-axiom profile checker from the spec's grammars | `g3/dl-store` | in progress | – | all |
| Queries (G4): `MIN`/`MAX`/`AVG` in eager aggregation, paths planned with the rest | `g4/eager-aggregates` | **done** (`9c66329`) | `MIN`/`MAX`/`AVG` (`42040b7`; BSBM 10 M BI q4's average per feature 214 → 56 ms, MIN/MAX 361 → 62 ms, same 2,474 rows; no regression on dbpedia-core, yago-tiny, BSBM BI: `450c3e7`); paths planned with the rest (`81144f2`; dbpedia path joins 5.7–37.9 → 0.09–0.15 ms, YAGO 82 → 2.4 ms); characteristic pairs were done on 3 October (`be7aeef`) | review, merge |
| EXPLAIN: estimated against actual rows for every operator, the §0 operators that don't report themselves | `g4/eager-aggregates` | **done** (`9c66329`) | every operator with estimated and actual rows, the four missing guards (`3c146e9`) | review, merge |
| Materialisation memory (P1): the transient fold, unused relations | `p1/transient-fold` | **done** (merged) | guards for F1, F4 and F8 (`6411987`); the transient fold and candidates without a second copy, F9/F10 (`1b0f134`); unread object orders not kept, F11, measured alone against f2c5dea (`f8dd1f9`); LUBM 1000: committed memory while reasoning 13.49 → 9.08 GB, reasoning 49.2 → 43.1 s (interleaved medians, n = 3; LUBM 100 1.99 → 1.37 GB, 3.48 → 3.38 s) | review, merge; the load's peak (now the process's) is P5's |
| OWL 2 QL: tree-witness rewriting over the RL closure | `ql/tree-witness` | in progress | design note; witness computation (11 tests, Ontop's example) | SPARQL integration, W3C QL tests, differential test, cost |
| One error envelope across HTTP, console and CLI | – | **done** (`9dcf78c`) | every error a problem document with its request id; the fuzz test requires it | – |
| Test suite cleanup: each test kept only if it represents the owning crate's behaviour better than any other test | `v2/tests` | in progress | server: 28 tests and 3 files that repeated store, reasoner or other server tests; store: 7 tests and 2 files (duplicates, reasoner rules on vendored ontologies); the RDF crates, exec, shacl, vector and fuzz reviewed, nothing to cut | after their agents merge: sparql (store's `having_may_name_a_select_alias` and `a_bgp_of_70_patterns_runs` move there), reasoner (`src/tests/v1_scenarios.rs`; the store's ruleset and datatype tests in `mutation_pipeline_tests` move there), dl (wall-clock budgets in tests made deterministic), owl, engine |
| Tiered gate: commit, push, milestone; benchmarks never in a gate; slow suites declared by their crate | `v2/gate` | in progress | baseline: a one-line engine change takes 487 s through the commit gate and runs the DL tests through a dev-dependency | the tiers, the build-speed A/Bs, test binaries merged per crate |
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

After the merge: the concise README and docs, the major refactoring (Miri over the unsafe
code, measured test coverage, duplicate dependency versions, the last typed error, the R1
residue), then v3.
