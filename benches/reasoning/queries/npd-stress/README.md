# NPD: the QL rewriting's stress case

The NPD benchmark (Lanti et al., EDBT 2015; github.com/ontop/npd-benchmark, Apache-2.0)
has the richest QL TBox of our workloads: 528 existential restrictions, 47 generating
axioms on `npdv:ExplorationWellbore` alone. `benches/reasoning/prepare-npd.sh` fetches
its ontology and 31 queries at a pinned commit. Its data is a relational dump with
mappings, no longer published as RDF; `benches/reasoning/prepare-npd-data.sh` turns it
into RDF (below). The stars here measure the rewriting itself.

`star*.rq` are stars of k existential arms on `ExplorationWellbore`: k independent tree
witnesses and 2^k branches, for the bounds of docs/design/ql-rewriting.md §5 (256
branches: k = 8 is rewritten, k = 9 runs as written; 16 existential variables).

## On data (6 October 2026)

`benches/reasoning/prepare-npd-data.sh [--reference] [DIR]` loads the benchmark's
PostgreSQL dump (data NLOD) into a PostgreSQL container, materialises its mappings with
Ontop 5.1.2 without the ontology (2,045,457 asserted statements: what a store loads), and
with `--reference` answers the 31 queries with Ontop's endpoint over the database, with
its existential reasoning (OWL 2 QL certain answers). Containers and network are removed
afterwards.

**Answers.** NRESE under `owl2-ql` (the closure: 2,741,156 inferred statements in
0.6-0.8 s) with the rewriting on: all 31 queries give Ontop's row counts, every one
`complete`. With the rewriting off, three miss answers: q28 538 of 600, q29 983 of 1,096,
q30 260 of 288. Twelve queries are rewritten (q17, q20-q30), each with one tree witness
and two branches, 4-17 atoms; none reaches a bound.

**Times** (main PC, `benches/probes/ql-rewriting-ab.sh npd owl2-ql`, 4 ABBA pairs of 5
measured runs, medians; sum of the 31 medians 77.7 ms off, 103.8 ms on):
- the 19 queries not rewritten: 0.75-1.26x, noise around equal;
- q28-q30, which gain answers: 1.8-1.9x (+0.5-0.9 ms);
- q17 and q20-q27, rewritten without a new answer: 1.3-2.7x (+0.9-6.1 ms). An
  unprojected variable is existential and the witness branch finds nothing the data
  doesn't already state: q22-q27 (`DISTINCT`) pay for the branch, q17, q20 and q21
  (aggregates) for the bag form's second evaluation too.

**No cost where nothing is added** (7 October, design §3): a witness whose tree the data
has at every individual it folds to is left out. The nine are now run as written (EXPLAIN:
`realised` 1, `patterns` 0); q28-q30 keep their witness and their answers. Same protocol,
on the branch's base and then with the change (main PC, both runs on 7 October):

| | sum of 31 medians off → on | q17, q20-q27 on/off | q28-q30 on/off |
|---|---|---|---|
| before | 117.1 → 148.7 ms (1.27x) | 1.10-3.13 | 1.68-1.87 |
| after | 105.9 → 106.4 ms (1.00x) | 0.88-1.20 | 1.54-1.65 |

The queries not rewritten: 0.75-1.26 in both runs. With the check's bounds (16 questions
per query, 1,000,000 statements per question), 8 pairs under other agents' load (sums
per pair 64-208 ms; per-query medians of per-pair ratios): the nine 0.76-1.13x, the
untouched 0.57-1.08x, q28-q30 1.46-1.65x; the two quiet pairs 63.9 -> 66.7 and
67.2 -> 65.1 ms. The check runs once per snapshot
revision and question, in a query's first run: its first runs off and on differ within
their noise (up to +3.7 ms for q26, +4.9 ms for q01, which isn't rewritten).

