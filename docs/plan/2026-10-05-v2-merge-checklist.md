# Before `refactor/engine-v2` goes to `main` (5 October 2026)

The owner's order: finish the current goals, benchmark them, merge to `main`, then make the
docs and README concise and refactor before v3; the new goals (STATUS G8–G13) wait. This is
the list to check before the merge. Items marked **check** are measurements that decide
whether something is built now or in v3; a number from a paper is a hypothesis until it is
measured here (benches/PROTOCOL.md, interleaved, confidence intervals).

## 1. Finish the current goals

- [ ] DL classification and realisation (3.4) merged; the Konclude outliers worked through
      (cause, fix, A/B, guard), the task that differs from HermiT and Konclude settled.
- [ ] The store's DL mode (G3): classification through `/classification`, the 7 OWL 2 RL
      W3C cases through the DL path.
- [ ] Queries (G4): `MIN`/`MAX`/`AVG` in eager aggregation, paths planned with the rest,
      characteristic pairs.
- [ ] Materialisation memory (P1): the transient fold, unused relations pruned.
- [ ] OWL 2 QL: tree-witness rewriting of the existential part over the RL closure.
- [ ] Usability: one error envelope across CLI, HTTP and console; EXPLAIN with estimated
      against actual rows.

## 2. Measurements that decide the next reasoning gains

Ranked by expected payoff against risk to the basics (research round 3; the research agent's
ranking of 5 October).

| # | Idea | Check first | Build when |
|---|---|---|---|
| 1 | **Store less:** only asserted or rule-produced types and sub-property facts stored; inherited ones from an exact closure of the class hierarchy that queries and rules both read | **check:** the share of LUBM(1000)'s memory by provenance (explicit, inherited types, sub-property, transitive, equality, indexes, support sets) | the share is large; the design keeps access labels, explanations, deletions and statistics exact |
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

## 4. The merge gate

- [ ] Every guard of docs/design/performance.md §0 green; every new win with its row and guard.
- [ ] W3C suites: no wrong answers; the disputed ones documented.
- [ ] The benchmark: no regression against the last recorded run beyond the confidence
      interval on any workload.
- [ ] Docs agree with the code (STATUS, the coverage plan, the configuration reference).

After the merge: the concise README and docs, the major refactoring (Miri over the unsafe
code, measured test coverage, duplicate dependency versions, the last typed error, the R1
residue), then v3.
