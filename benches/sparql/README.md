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
| 1 | Sparqloscope on DBLP | QLever | data, queries, bar ready; NRESE qualified once on the monthly dump (which proved the wrong data, below); QLever's reproduction queued on phuoc-yu |
| 1 | BSBM V3.1 explore and BI, 1 to 64 clients | Virtuoso | data and driver preserved; full answer qualification blocked by the driver limitations below |
| 1 | WatDiv basic testing | Oxigraph, Jena | 10 M (56 queries) and 100 M (196 queries) ready; 10 M runs the runtime's qualification |
| 1 | G-CARE | the planner's estimates (G-CARE's estimators) | data, queries, truths, both runners ready; NRESE measured on HUMAN |
| backlog | SP2Bench | Oxigraph, Jena | data generated; the generated documents don't reproduce the paper's result sizes (below) |
| backlog | WDBench | MillenniumDB | prepared; needs khaleesi S: or Draco |
| backlog | MillenniumDB's paths, FEASIBLE, gMark, BeSEPPI | | |

## The benchmarks

| Benchmark | Data (pinned in [../datasets.toml](../datasets.toml)) | Queries | Answers checked by | The bar |
|---|---|---|---|---|
| Sparqloscope | DBLP's knowledge graph as the paper's Qleverfile downloads it (`dblp_KG_with_associated_data.tar`), dblp's release of 11 Oct 2026, pinned by the archive's SHA-256: `dblp.ttl.gz`, 605.9 M statements (the paper's April 2025 release had about 502 M). Not the monthly N-Triples dump: it lacks the stream records several queries are about (`dblp:relatedStream` has no instance there, so the path queries count 0) | `ad-freiburg/sparqloscope` 61ad92f `benchmarks/dblp.benchmark.tsv`, 105 queries ([sparqloscope/import.py](sparqloscope/import.py)) | the reference systems: the paper's answers are of another release; among themselves its engines agree on 66 of 105 ([dblp-checks.toml](sparqloscope/dblp-checks.toml), kept for a run on the paper's data), differ on the rest mostly because each loaded the data differently (published triple counts 502.36 M to 788.86 M), and QLever's are wrong on several (`filter-many-results`, `regex-prefix-*`, `number-of-literals`; string MIN and MAX by a locale's collation) | the paper's Table 2: geometric mean with failures as twice the 180 s timeout, QLever 0.176 s (0 failed), Virtuoso 0.485 s (5), MillenniumDB 1.26 s, GraphDB 5.80 s, Blazegraph 13.4 s (15), Jena 15.3 s (19); Oxigraph 38.6 s (46), on an AMD Ryzen 9 9950X with 190 GiB ([dblp-published.tsv](sparqloscope/dblp-published.tsv)); here on data about 20 % larger |
| BSBM V3.1 | BIBM 0.7.8's generator, `-fc -pareto`, 28480 and 284800 products: exactly the report's 10,119,864 and 100,062,249 triples | the explore (12 queries) and BI (8) mixes, drill-down, BIBM's test driver ([bsbm/drive.py](bsbm/drive.py)) | the driver's qualification output, compared with a reference system's by BIBM's comparator | the V3.1 report (April 2013, 2 x Xeon E5-2650, 256 GB): Virtuoso 7 at 100 M, explore 47,179 / 91,505 / 188,632 / 216,119 QMpH with 1 / 4 / 8 / 64 clients, BI 997 / 5,644 / 6,402 / 7,132; Jena TDB 0.9.4 explore 15,382 / 19,036 / 24,647 / 14,838 |
| WatDiv | its authors' `watdiv.10M` and `watdiv.100M` (sources.sha256) | the 20 basic-testing templates of v0.6, instantiated from each dataset's saved generator state ([watdiv/queries.sh](watdiv/queries.sh)); 56 distinct queries at 10 M | the reference systems (no published answers) | none for Oxigraph or Jena; the WatDiv paper's systems are others |
| G-CARE | its four graphs as RDF ([gcare/convert.py](gcare/convert.py)): AIDS 0.8 M, HUMAN 0.2 M, LUBM80 20.4 M, YAGO 58.3 M statements | its 2,201 query graphs as SPARQL patterns | the published true cardinalities (G-CARE counts homomorphisms, as SPARQL does; checked on HUMAN) | G-CARE's estimators run by its own code and procedure ([gcare/run-gcare.sh](gcare/run-gcare.sh)): q-error percentiles over 30 trials per query |
| SP2Bench | v1.01 `sp2b_gen`, 1 M and 25 M triples, written as N-Triples by Jena's riot | its 17 queries | the paper's Table V | none at 25 M (the paper's systems all failed there) |

## BSBM qualification boundary (14 October 2026)

The pinned BIBM 0.7.8 source archive has SHA-256
`4f36056e99d8d2fe7dbc99efc8b80f9629dce63119ccaeffcf90c9cf0e710793`.
Its Explore mix contains 25 requests: 20 SELECT, four q9 DESCRIBE and one q12 CONSTRUCT.
Twenty qualification mixes therefore contain 500 executions: 400 SELECT and 100 graph
requests. q6 is not selected by that mix. BI's drill expansion depends on the preserved
test-data hierarchy: with L levels, twenty mixes require `80*L+20` executions.

The archived driver assembles qualification answers only for SELECT. For graph requests it
consumes the response bytes but emits empty qualification records, which its comparator
cannot compare. XML counts and process exit zero therefore do not establish full answer
qualification. This is a source audit; equivalence with the deployed bytecode has not been
demonstrated. The active descriptors have no `resultKeys`: the comparator uses positional
comparison, whose ordering assumptions also require checking for unordered queries and ties.

Keep every request in the timed mix. q12 needs graph-answer comparison; byte counts are not
equivalence. q9's description is chosen by the service under
[SPARQL 1.1 section 16.4](https://www.w3.org/TR/sparql11-query/#describe), so different graphs
are not automatically an engine error. Record that semantic policy separately from exact
cross-engine answer agreement. Complete timing coverage, comparable answers and
implementation-dependent answers must remain distinguishable; partial SELECT qualification
must not be reported as a fully qualified Explore run. The driver consolidation is pending.

## Configurations

Where (owner, 12 Oct 2026): phuoc-yu (Ryzen 7 7800X3D, 16 threads, 29 GiB), every system from its
Docker image, unpacked and run with bubblewrap (the suite's rootfs runtime: same images, the host's
network and kernel, a systemd scope with the same memory cap for every system); NRESE as a host
process under the same cap. Before any comparison counts the runtime is qualified against Docker: equal
answers on WatDiv 10 M (office-pc under Docker, phuoc-yu under rootfs) and the contenders' published
figures reproduced (QLever's Sparqloscope figure, Virtuoso's BSBM V3.1 figures). The published
configurations assume 32 GB; on 29 GiB every memory envelope is scaled by the same factor for every
system (QLever's 26 GB for queries becomes 23 GB, its 6 GB cache 5 GB; NRESE gets the same 23 GB).

| System | Sparqloscope (DBLP, the paper's settings for 32 GB) | BSBM (the V3.1 report) |
|---|---|---|
| QLever | image `adfreiburg/qlever` commit 3d7f1e9 (29 Sep 2026); index settings `{"num-triples-per-batch": 1000000}`, vocabulary `in-memory-compressed`; server `-m 26G -c 6G -s 180s`, runtime `group-by-hash-map-enabled=true` (the paper's Qleverfile) | as for DBLP, memory for the 100 M set |
| Virtuoso | 7.2.17; `NumberOfBuffers` 2720000, `MaxDirtyBuffers` 2000000 (virtuoso.ini's values for 32 GB, the paper's) | the same buffers; `ServerThreads` 100 and `ThreadsPerQuery` 8 as the report's virtuoso.ini |
| MillenniumDB | built from ecbf6dd (the paper's commit) on Ubuntu 24.04 ([../competitors/millenniumdb/Dockerfile](../competitors/millenniumdb/Dockerfile)); `--threads 2 --versioned-buffer 22GB --unversioned-buffer 2GB --strings-static 4GB --strings-dynamic 4GB --timeout 180` | - |
| Jena | 6.2.0, `tdb2.xloader` as the paper loads DBLP; Fuseki `--timeout` at the query limit; heap within the container | `tdb2.tdbloader` |
| NRESE | on-disk store; the memory QLever's configuration gives its queries, per query and in all (`QUERY_MEMORY_MIB`, `TOTAL_QUERY_MEMORY_MIB`; 23 GB on phuoc-yu) | the same |

The settings are environment variables of the suite's adapters (`QLEVER_INDEX_SETTINGS`,
`QLEVER_VOCABULARY_TYPE`, `QLEVER_CACHE_MAX_SIZE`, `QLEVER_RUNTIME`,
`VIRTUOSO_NUMBER_OF_BUFFERS`, `VIRTUOSO_MAX_DIRTY_BUFFERS`, `VIRTUOSO_THREADS_PER_QUERY`,
`VIRTUOSO_SERVER_THREADS`, `MDB_*`, `JENA_LOADER`); each job file under [jobs/](jobs/) sets
them, and the run's manifest records them.

## Running

```sh
benches/sparql/prepare.sh dblp watdiv-10m bsbm-100m gcare     # data into ~/owlrs/sparql-data (SPARQL_DATA)
benches/sparql/images.sh millenniumdb                         # images no vendor publishes
benches/sparql/watdiv/queries.sh watdiv-10m 5                 # then split.py into SPARQL_DATA/queries/watdiv-10m, pinned in queries.sha256
python3 benches/suite/queue.py submit benches/sparql/jobs/JOB.toml   # the timed runs (jobs/*-phuoc.toml: reproductions first)
```

## Findings kept for the backlog

- **SP2Bench's generated documents don't reproduce its paper's result sizes.** NRESE on
  sp2bench-1m (12 Oct 2026, phuoc-yu, 1b611e7): q1, q9, q10, q11 equal Table V, but q2 32,452
  (paper 32,770), q3a 52,406 (52,676), q3b 402 (379), q4 2,583,025 (2,586,733), q5b 34,870
  (35,241), q7 303 (292), q8 491 (400). The differences across most queries point at the
  document (the generator compiled for 64 bits with a current compiler), not at the engine;
  the reference systems settle it before the table is used as an oracle. In the same run NRESE
  answered q5a with 413 (query memory, 4 GiB default) and q6 timed out at 120 s.
