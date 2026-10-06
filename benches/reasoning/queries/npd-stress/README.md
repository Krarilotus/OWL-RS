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
- q17 and q20-q27, rewritten without a new answer: 1.3-2.7x (+0.9-6.1 ms). The bag form
  evaluates the pattern a second time to subtract the materialised rows (design §3:
  `P ∪ (DISTINCT π(rewriting) FILTER NOT EXISTS P)`); here an unprojected variable is
  existential, the witness finds nothing the data doesn't already state, and the second
  evaluation is all cost. The bag form's second evaluation is an open item (STATUS).
