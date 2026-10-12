# Breadth: each feature's benchmark, each competitor on its home turf

The standards and features beyond core SPARQL and reasoning, for release 1.5: SHACL,
GeoSPARQL, full-text search, RDF 1.2, federation, vector search and data larger than memory.
Each benchmark is the one its competitors publish on, run in their documented best
configuration ([CATALOG.md](../CATALOG.md) §3.7–3.11, §3.13, §3.16–3.17; the owner, 12
October 2026: "each contender ... on their home turf against us ... on equal and fair
terms"). A win counts only where NRESE beats a system at what that system does best.

The rules are [PROTOCOL.md](../PROTOCOL.md)'s. The families, their order and the queue are the
suite's ([suite/workloads.toml](../suite/workloads.toml), [suite/schedule.toml](../suite/schedule.toml),
[suite/queue.py](../suite/queue.py)); results go through its one schema
([suitekit/schema.py](../suite/suitekit/schema.py)) to `suite.py report` and `compare`.

## The benchmarks, in the owner's order (12 October 2026)

First this week: the filtered vector search, ann-benchmarks, Geographica 2, ERA-SHACL. Then
the backlog.

| Benchmark | Home turf of | Contenders as run | Published numbers | NRESE's support | State |
|---|---|---|---|---|---|
| Filtered vector search (`vector-recall`) | – (NRESE's own gate) | the fast cases `vector-knn-exact`, `vector-knn-filtered` | – | `SERVICE nrv:search` | **fixed** (the probe-first route's beam); jobs queued |
| [ann-benchmarks](vectors/) | Qdrant, pgvector; hnswlib as the library reference | upstream's harness at 2e081ad, NRESE as a module (its server in the container, SPARQL per query); all at M 16; Qdrant 1.19.2, pgvector v0.8.7 | ann-benchmarks.com | HNSW over vector literals, through SPARQL | images and SIFT, GloVe jobs queued |
| [Geographica 2](geographica2/) | Jena GeoSPARQL, GraphDB (GeoSPARQL plugin), RDF4J; QLever and Virtuoso as far as they go | the suite's adapters, each system's spatial setup when a workload needs `geosparql` | RDF4J 2.4.3, GraphDB 8.6.1, Strabon, uSeekM, Parliament (2019, the paper's Tables 13/14) | GeoSPARQL 1.1 | data pinned; jobs queued |
| [ERA-SHACL](era-shacl/) | Jena, TopBraid, RDF4J, pySHACL; maplib (the Rust rival) | upstream's engines at fe109c9, their versions, prebuilt; one memory cap and heap for all | per engine in upstream's `results/` | SHACL Core and SHACL-SPARQL, in memory | data pinned; jobs queued |
| [StarBench](starbench/) | GraphDB, Stardog (RDF-star); Jena, Oxigraph | the suite's adapters; GraphDB and Stardog load RDF-star, the others RDF 1.2 | Jena 4.7.0, Oxigraph 0.3.16, GraphDB 10.0.2, Stardog 8.1 (paper, Table 1) | RDF 1.2, SPARQL 1.2 | backlog: data fetched, workload defined |
| LUBMft (`fulltext-search`) | Jena text, Virtuoso, QLever | – | ESWC 2009 | `bds:`, `text:query`, `luc:` | backlog, **blocked: the generator is lost** |
| FedShop | FedX, CostFed, Semagrow, HeFQUIN, FedUP | – | Zenodo `eval-model.zip` | `SERVICE`; **no source selection** | backlog: data fetched |
| VIBE, big-ann filtered, VectorDBBench | index libraries; filter-aware indexes; the vector databases | – | their sites | as above | backlog |
| Larger than memory | NRESE's budgets against JVM heaps and QLever's sort memory | – | – | load and materialisation budgets | backlog |

## How a benchmark is run

1. **Prepare** (`prepare.sh`): the sources as their authors publish them, checked against the
   publisher's checksum, into `$NRESE_BREADTH/data/<benchmark>` (default `~/nrese-breadth`);
   each file's SHA-256 is frozen in [datasets.toml](../datasets.toml). Heavy preparation runs
   inside `scripts/quiet-slot.sh`, so no measurement shares the disk with it.
2. **Images**: each contender as its benchmark builds it, at the pinned commit; NRESE from
   this worktree in the pinned Rust image ([kit.py](kit.py), binaries recorded by SHA-256).
   Builds wait for the quiet slot (at most as long as `cargo-guarded.sh` does).
3. **Qualify** (`jobs/*-qualify.toml`, any window): every answer checked against the
   reference before a time counts.
4. **Measure** (`jobs/*-timed.toml`, the quiet window): three interleaved repetitions, every
   system on one host under one memory cap, then `suite.py report` and `compare` and a run
   record in [runs/](../runs/).

`python benches/suite/queue.py submit benches/breadth/jobs/<job>.toml` queues a job; the
host runs it when the owner's order and its window allow. A step stopped because the host's
owners came back runs again.

Licensed systems (GraphDB, Stardog) run on our machines only; their rows carry
`publish = permission` and their numbers are never committed ([competitors/README.md](../competitors/README.md)).

## Findings so far

- **Vector search, filtered recall (defect, fixed 12 October):** `vector-knn-filtered`'s
  0.40–0.60 came from `vector_join`'s probe-first route asking the index for unfiltered
  neighbours with a beam not widened for the filter (`accepted: None`); the candidate route
  widened it by 1/share. 200,000 uniform 64-d vectors, one kind in ten: 0.50 → 0.98
  ([filtered_recall.rs](../../crates/nrese-vector/examples/filtered_recall.rs)); guard
  `a_filtered_search_that_goes_first_finds_the_true_nearest`. On this data the widened beam
  costs about what the exact scan does (runs/2026-10-12-vector-recall-phuoc.toml: beam 1024
  11.1 ms against 10.7 ms for the five queries): the graph's own recall at the default beam
  (0.44–0.48 unfiltered) is the next cause, for the index's owner.
- **Vector search, no index status:** the server doesn't say when a vector graph is built
  (only its log does), where Qdrant reports a collection status; the HNSW parameters
  (M 16, ef_construction 200) can't be set.
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
- **Interface changes to the suite** (with the framework's thread): the tasks `validate` and
  `index`; `Plan.inputs_for` (a system's own form of the same data) and `Plan.services` (the
  index a workload needs, which each adapter configures); the capabilities `rdf12` and
  `vector`; a recall field for approximate search (ann-benchmarks' recall is in `note`).
