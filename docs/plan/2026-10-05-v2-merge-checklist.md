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
| DL classification and realisation (3.4); Konclude outliers (cause, fix, A/B, guard); the task that differs from HermiT and Konclude | `dl/classify` | in progress | soundness fix (heads about successors), detached probes, 60 s Horn bound; `b1659c9` fixes 7127 and 7956; W2 (tests start from the consistency model) written, in its test campaign | W2 committed and measured; W1 (a saturation that never refuses); W3–W7 as they pay; re-time the dev set; merge |
| The store's DL mode (G3): `/classification`, the 7 RL W3C cases through the DL path | – | waiting for 3.4 | – | all |
| Queries (G4): `MIN`/`MAX`/`AVG` in eager aggregation, paths planned with the rest | `g4/eager-aggregates` | in progress | – (characteristic pairs were done on 3 October) | all three, then merge |
| EXPLAIN: estimated against actual rows for every operator, the §0 operators that don't report themselves | `g4/eager-aggregates` | in progress | estimates for sets and late operators being added | guards per operator |
| Materialisation memory (P1): the transient fold, unused relations | `p1/transient-fold` | in progress | guards for F1, F4 and F8, each shown to catch its revert | heap profile of LUBM 1000 (running), the fold, pruning, A/B |
| OWL 2 QL: tree-witness rewriting over the RL closure | `ql/tree-witness` | in progress | design note; witness computation (11 tests, Ontop's example) | SPARQL integration, W3C QL tests, differential test, cost |
| One error envelope across HTTP, console and CLI | – | **done** (`9dcf78c`) | every error a problem document with its request id; the fuzz test requires it | – |
| Fast regression suite (for §4's gate) | `bench/catalog-and-fast-suite` | in progress | the catalogue (`d9e580a`) | about 35 cases, NRESE in every profile, home-turf cases |

Paused until v2 is merged: the Zebratlas workstreams and every other side thread.

## 2. Measurements that decide the next reasoning gains

Ranked by expected payoff against risk to the basics (research round 3; the research agent's
ranking of 5 October).

| # | Idea | Check first | Build when |
|---|---|---|---|
| 1 | **Store less:** only asserted or rule-produced types and sub-property facts stored; inherited ones from an exact closure of the class hierarchy that queries and rules both read | **checked on LUBM (5 Oct):** inherited types 71.5 % of the inferred facts, inverses 18.2 %, sub-properties 6.2 %: 96 % of the inferred facts, 38 % of all stored ones, follow from the schema (the same at LUBM 10 and 100). **Still to check:** OWL2Bench, DBpedia with its ontology, Claros | the share holds beyond LUBM; the design keeps access labels, explanations, deletions and statistics exact |
| 2 | **Avoid rule applications** before adding cores: deltas routed only to the rule positions that consume them (GLog's trigger graphs) | **check:** a counter of redundant derivations per round on LUBM and Claros | the redundant share is large (the schema compiler already removes part of it) |
| 3 | **Equality:** canonical ids are in (compact equality, late expansion); open: incremental merges and splits | **check:** e-graph rebuilding (egg/egglog, batched congruence closure) for the open part; a differential test against axiomatised equality on `sameAs`-heavy data | the differential test passes and merges or splits are measured |
| 4 | **Machine level:** vqsort for 128-bit keys, an Eytzinger index with prefetch for membership checks (13 % of a LUBM(1000) run), SIMD intersections, PGO | **check:** each one by A/B; vqsort's licence and build (Highway, C++) | each wins beyond the confidence interval and slows nothing |
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
      in 2-CPU containers).
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
