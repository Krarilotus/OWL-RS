# The SPARQL stores on their home turf

Release 1.5's comparison of NRESE with the SPARQL query engines, each on what it does best
(owner, 12 Oct 2026: "each contender ... on their home turf against us ... on equal and fair
terms"). A win counts only where NRESE beats a system at its own benchmark, at its published
scale, in its documented best configuration, on the same host under the same resources. The
cases are in [../cases/sparql.toml](../cases/sparql.toml); they run through the suite's driver
and are judged by its `versus` and `standings` ([../suite/README.md](../suite/README.md)).

## Priorities (owner, 12 Oct 2026)

| Order | Benchmark | Home turf of | State |
|---|---|---|---|
| 1 | Sparqloscope on DBLP | QLever | data, queries, known answers, bar ready; NRESE qualified on phuoc-yu; QLever's reproduction queued |
| 1 | BSBM V3.1 explore and BI, 1 to 64 clients | Virtuoso | data (exact published sizes), driver, qualification ready; runs queued |
| 1 | WatDiv basic testing | Oxigraph, Jena | 10 M data and queries ready; 100 M data ready, queries to instantiate |
| 1 | G-CARE | the planner's estimates (G-CARE's estimators) | data, queries, truths, NRESE's estimates and G-CARE's runner ready |
| backlog | SP2Bench | Oxigraph, Jena | data generated; the generated documents don't reproduce the paper's result sizes (below) |
| backlog | WDBench | MillenniumDB | prepared; needs khaleesi S: or Draco |
| backlog | MillenniumDB's paths, FEASIBLE, gMark, BeSEPPI | | |

## The benchmarks

| Benchmark | Data (pinned in [../datasets.toml](../datasets.toml)) | Queries | Answers checked by | The bar |
|---|---|---|---|---|
| Sparqloscope | DBLP of 1 April 2025, CC0 (doi:10.4230/dblp.rdf.ntriples.2025-04-01; MD5 as published), 503.9 M statements | `ad-freiburg/sparqloscope` 61ad92f `benchmarks/dblp.benchmark.tsv`, 105 queries ([sparqloscope/import.py](sparqloscope/import.py)) | the 66 answers every engine of the paper published alike ([dblp-checks.toml](sparqloscope/dblp-checks.toml)); the other 39 against the reference systems: the engines' published answers differ there, mostly because each loaded the data differently (published triple counts 502.36 M to 788.86 M), and QLever's are wrong on several (`filter-many-results`, `regex-prefix-*`, `number-of-literals`; string MIN and MAX by a locale's collation) | the paper's Table 2: geometric mean with failures as twice the 180 s timeout, QLever 0.176 s (0 failed), Virtuoso 0.485 s (5), MillenniumDB 1.26 s, GraphDB 5.80 s, Blazegraph 13.4 s (15), Jena 15.3 s (19); Oxigraph 38.6 s (46), on an AMD Ryzen 9 9950X with 190 GiB ([dblp-published.tsv](sparqloscope/dblp-published.tsv)) |
| BSBM V3.1 | BIBM 0.7.8's generator, `-fc -pareto`, 28480 and 284800 products: exactly the report's 10,119,864 and 100,062,249 triples | the explore (12 queries) and BI (8) mixes, drill-down, BIBM's test driver ([bsbm/drive.py](bsbm/drive.py)) | the driver's qualification output, compared with a reference system's by BIBM's comparator | the V3.1 report (April 2013, 2 x Xeon E5-2650, 256 GB): Virtuoso 7 at 100 M, explore 47,179 / 91,505 / 188,632 / 216,119 QMpH with 1 / 4 / 8 / 64 clients, BI 997 / 5,644 / 6,402 / 7,132; Jena TDB 0.9.4 explore 15,382 / 19,036 / 24,647 / 14,838 |
| WatDiv | its authors' `watdiv.10M` and `watdiv.100M` (sources.sha256) | the 20 basic-testing templates of v0.6, instantiated from each dataset's saved generator state ([watdiv/queries.sh](watdiv/queries.sh)); 56 distinct queries at 10 M | the reference systems (no published answers) | none for Oxigraph or Jena; the WatDiv paper's systems are others |
| G-CARE | its four graphs as RDF ([gcare/convert.py](gcare/convert.py)): AIDS 0.8 M, HUMAN 0.2 M, LUBM80 20.4 M, YAGO 58.3 M statements | its 2,201 query graphs as SPARQL patterns | the published true cardinalities (G-CARE counts homomorphisms, as SPARQL does; checked on HUMAN) | G-CARE's estimators run by its own code and procedure ([gcare/run-gcare.sh](gcare/run-gcare.sh)): q-error percentiles over 30 trials per query |
| SP2Bench | v1.01 `sp2b_gen`, 1 M and 25 M triples, written as N-Triples by Jena's riot | its 17 queries | the paper's Table V | none at 25 M (the paper's systems all failed there) |

## Configurations

Each system as its own documentation configures it for the benchmark, on office-pc (31 GiB,
Ryzen 9 5950X), every system in Docker under the same container memory limit:

| System | Sparqloscope (DBLP, the paper's settings for 32 GB) | BSBM (the V3.1 report) |
|---|---|---|
| QLever | image `adfreiburg/qlever` commit 3d7f1e9 (29 Sep 2026); index settings `{"num-triples-per-batch": 1000000}`, vocabulary `in-memory-compressed`; server `-m 26G -c 6G -s 180s`, runtime `group-by-hash-map-enabled=true` (the paper's Qleverfile) | as for DBLP, memory for the 100 M set |
| Virtuoso | 7.2.17; `NumberOfBuffers` 2720000, `MaxDirtyBuffers` 2000000 (virtuoso.ini's values for 32 GB, the paper's) | the same buffers; `ServerThreads` 100 and `ThreadsPerQuery` 8 as the report's virtuoso.ini |
| MillenniumDB | built from ecbf6dd (the paper's commit) on Ubuntu 24.04 ([../competitors/millenniumdb/Dockerfile](../competitors/millenniumdb/Dockerfile)); `--threads 2 --versioned-buffer 22GB --unversioned-buffer 2GB --strings-static 4GB --strings-dynamic 4GB --timeout 180` | - |
| Jena | 6.2.0, `tdb2.xloader` as the paper loads DBLP; Fuseki `--timeout` at the query limit; heap within the container | `tdb2.tdbloader` |
| NRESE | on-disk store; query memory 20 GB (`QUERY_MEMORY_MIB`, NRESE's equivalent of QLever's 26 GB for queries) | the same |

The settings are environment variables of the suite's adapters (`QLEVER_INDEX_SETTINGS`,
`QLEVER_VOCABULARY_TYPE`, `QLEVER_CACHE_MAX_SIZE`, `QLEVER_RUNTIME`,
`VIRTUOSO_NUMBER_OF_BUFFERS`, `VIRTUOSO_MAX_DIRTY_BUFFERS`, `VIRTUOSO_THREADS_PER_QUERY`,
`VIRTUOSO_SERVER_THREADS`, `MDB_*`, `JENA_LOADER`); each job file under [jobs/](jobs/) sets
them, and the run's manifest records them.

## Running

```sh
benches/sparql/prepare.sh dblp watdiv-10m bsbm-100m gcare     # data into ~/owlrs/sparql-data (SPARQL_DATA)
benches/sparql/images.sh millenniumdb                         # images no vendor publishes
benches/sparql/watdiv/queries.sh watdiv-10m 5                 # then split.py into benches/competitors/queries
python3 benches/suite/queue.py submit benches/sparql/jobs/JOB.toml   # the timed runs, at night
```

## Findings kept for the backlog

- **SP2Bench's generated documents don't reproduce its paper's result sizes.** NRESE on
  sp2bench-1m (12 Oct 2026, phuoc-yu, 1b611e7): q1, q9, q10, q11 equal Table V, but q2 32,452
  (paper 32,770), q3a 52,406 (52,676), q3b 402 (379), q4 2,583,025 (2,586,733), q5b 34,870
  (35,241), q7 303 (292), q8 491 (400). The differences across most queries point at the
  document (the generator compiled for 64 bits with a current compiler), not at the engine;
  the reference systems settle it before the table is used as an oracle. In the same run NRESE
  answered q5a with 413 (query memory, 4 GiB default) and q6 timed out at 120 s.
