# Breadth: each feature's benchmark, each competitor on its home turf

The standards and features beyond core SPARQL and reasoning, for release 1.5: SHACL,
GeoSPARQL, full-text search, RDF 1.2, federation, vector search and data larger than memory.
Each benchmark is the one its competitors publish on, run in their documented best
configuration ([CATALOG.md](../CATALOG.md) §3.7–3.11, §3.13, §3.16–3.17; the owner, 12
October 2026: "each contender ... on their home turf against us ... on equal and fair
terms"). A win counts only where NRESE beats a system at what that system does best.

The rules are [PROTOCOL.md](../PROTOCOL.md)'s. Results go through the suite's one schema
([suitekit/schema.py](../suite/suitekit/schema.py)) to `suite.py report` and `compare`.

## The benchmarks

| Benchmark | Home turf of | Contenders as run | Published numbers | NRESE's support | State |
|---|---|---|---|---|---|
| [ERA-SHACL](era-shacl/) | Jena, TopBraid, RDF4J, pySHACL (and maplib, the Rust rival) | upstream's engines at fe109c9, pinned versions, prebuilt; one heap for all | per engine in upstream's `results/` (6 runs, 9 combinations) | SHACL Core and SHACL-SPARQL, in memory | kit ready; data being prepared |
| [StarBench](starbench/) | GraphDB, Stardog (RDF-star); Jena, Oxigraph | the suite's adapters; GraphDB and Stardog load RDF-star, the others RDF 1.2 | Jena 4.7.0, Oxigraph 0.3.16, GraphDB 10.0.2, Stardog 8.1 (paper, Table 1) | RDF 1.2, SPARQL 1.2 | workload defined; data being prepared |
| Geographica 2 | Jena GeoSPARQL, RDF4J, GraphDB; QLever (its geo subset) | to define | RDF4J 2.4.3, GraphDB 8.6.1, Strabon et al. (2019) | GeoSPARQL 1.1 functions | data fetched |
| LUBMft | Jena text, Virtuoso, QLever | – | ESWC 2009 (Jena 2.5, Sesame 2.2, Virtuoso 5) | Blazegraph `bds:`, Jena `text:query`, GraphDB `luc:` vocabularies | **blocked: the generator is lost** (decision below) |
| FedShop | FedX, CostFed, Semagrow, HeFQUIN, FedUP | to define | all engines, 20 to 200 members (Zenodo `eval-model.zip`) | `SERVICE` with bind joins; **no source selection** | data fetched |
| ann-benchmarks | Qdrant, Milvus, pgvector (upstream modules) | to define | ann-benchmarks.com | HNSW over vector literals, through SPARQL | data fetching |
| VIBE | index libraries (hnswlib, Faiss, Glass, ...) | to define | vector-index-bench.github.io | as above | – |
| big-ann filtered | filter-aware indexes (parlayivf, puck, Faiss) | to define | NeurIPS'23, Azure D8lds v5 | filters as SPARQL patterns | – |
| VectorDBBench | Qdrant, Milvus, pgvector, LanceDB | to define | zilliz.com leaderboard | as above | – |
| Memory-budget series | NRESE's budgets against JVM heaps and QLever's sort memory | to define | – | load and materialisation budgets | data present (LUBM 1000) |

## How a benchmark is run

1. **Prepare** (`prepare.sh`): the sources as their authors publish them, checked against the
   publisher's checksum, into `$NRESE_BREADTH/data/<benchmark>` (default `~/nrese-breadth`);
   each derived file's SHA-256 goes into [datasets.toml](../datasets.toml). Heavy preparation
   runs inside `scripts/quiet-slot.sh`, so no measurement shares the disk with it.
2. **Images**: each contender as its benchmark builds it, at the pinned commit; NRESE from
   this worktree in the pinned Rust image (`kit.py build_nrese`, binaries recorded by
   SHA-256).
3. **Qualify** (one repetition, no claim): every answer checked against the reference; a
   contender's published number reproduced on our hardware within its spread before its
   configuration counts as fair.
4. **Measure** (three interleaved repetitions, a quiet slot, one host for all systems), then
   `suite.py report` and `compare`, and a run record in [runs/](../runs/).

Licensed systems (GraphDB, Stardog) run on our machines only; their rows carry
`publish = permission` and their numbers are never committed ([competitors/README.md](../competitors/README.md)).

## Findings so far

- **Vector search, filtered recall (defect):** `vector-knn-filtered`'s 0.40–0.60 is the
  probe-first route of `vector_join` (`crates/nrese-sparql/src/native/vectors.rs`) asking the
  index for unfiltered neighbours with an unwidened beam (`accepted: None`), then filtering;
  the candidate route widens the beam by 1/share and reaches 1.00. Reproduced in
  `crates/nrese-vector/examples/filtered_recall.rs`: as built 0.50; with the beam widened by
  1/share 0.98; candidate-first 1.00; unfiltered at the default beam 0.44 (uniform 64-d data).
- **Federation:** NRESE answers `SERVICE` queries, but has no source selection: FedShop's
  federation engines answer the unannotated queries; NRESE can run only the reference
  source assignments.
- **LUBMft:** the generator (UBAft), tester and query template are gone (the L3S page answers
  404 since April 2024; the Internet Archive kept only the page and the paper).
- **Docs:** `docs/ops/http-api.md` says SHACL validation is "SHACL Core only", while
  `systems.toml` and the fast suite's `shacl-lubm` case run SHACL-SPARQL.

## Decisions needed

- **LUBMft:** reconstruct its generator from the paper (names from the 1990 census, text
  from a topic model of the NIPS papers; the 20 queries are fully specified), or replace it
  as the full-text benchmark (BEIR for ranking, QLever's own text workloads for speed).
- **Interface changes to the suite** (with the framework's thread): the task `validate` in
  the result schema; `Plan.inputs_for` (a system's own form of the same data); the
  capabilities `rdf12` and `vector`; a recall field for approximate search.
