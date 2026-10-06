# NPD: the QL rewriting's stress case

The NPD benchmark (Lanti et al., EDBT 2015; github.com/ontop/npd-benchmark, Apache-2.0)
has the richest QL TBox of our workloads: 528 existential restrictions, 47 generating
axioms on `npdv:ExplorationWellbore` alone. `benches/reasoning/prepare-npd.sh` fetches
its ontology and 31 queries at a pinned commit. Its data is a MySQL dump with R2RML
mappings and no longer published as RDF, so these measure the rewriting itself.

`star*.rq` are stars of k existential arms on `ExplorationWellbore`: k independent tree
witnesses and 2^k branches, for the bounds of docs/design/ql-rewriting.md §5 (256
branches: k = 8 is rewritten, k = 9 runs as written; 16 existential variables).
