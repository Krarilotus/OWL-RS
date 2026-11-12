# Competitor comparisons

Runs NRESE and other RDF stores on the same input, on the same host, all in Docker, so
hardware, storage and container overhead are identical.

## Licence rules for results: read before publishing anything

Some competitors' licences forbid publishing benchmark results without the vendor's
consent. This repository is public, so **committing results counts as publishing**.

| System | Licence | Publishing results |
|---|---|---|
| QLever | Apache 2.0 | free |
| Apache Jena (TDB2, Fuseki's storage) | Apache 2.0 | free |
| Oxigraph | MIT / Apache 2.0 | free |
| Virtuoso Open Source 7 | GPL v2 | free |
| Licensed systems | the vendor's licence | **only with the vendor's written consent**, recorded in this file |

**Therefore:**
- Raw results go to `benches/competitors/results/`, which is git-ignored.
- Only numbers for the free-to-publish systems may appear in committed docs.
- Licensed systems' numbers stay local until written permission exists. Record the permission (who, when, scope) in this file before publishing.
- Our designs come from the published literature cited in `docs/design/`. We never derive them from observing the vendors' products.

Results: one record per run in [../runs/](../runs/README.md).

## What's here

| File | Purpose |
|---|---|
| `prepare-datasets.sh [names...]` | Downloads the real-world datasets and converts each to one validated N-Triples file in the Docker volume `nrese-bench-data`: `olympics` (1.8 M triples), `yago-tiny` (23 M), `dbpedia-core` (67 M), `wikidata-lexemes-<N>m` (first N million of 229 M) and full `wikidata-lexemes` |
| `nt-filter.py` | Drops the few lines strict parsers reject (invalid IRIs or language tags) before any system sees the file, so all systems get identical input. Reports how many lines it drops |
| `scorecard.sh <dataset> [systems...]` | **Pf0 scorecard** per system: load time, peak load memory, store bytes (per triple), restart time, server memory, query mix (1 warm-up + 5 runs per query, p50), and a cross-check that all systems return the same result counts. `LOAD_ONLY=1` skips the queries. Synthetic data comes from the harness: `generate --triples N` → `entities-N` |
| `queries/<dataset>/*.rq` | Query mixes: point lookups, star and multi-hop joins, aggregates, numeric/date/text filters, OPTIONAL, negation, property paths, full scans, large streamed results |
| `graphdb/repo-*.ttl` | repository configurations for a licensed system's adapter |
| `jena/Dockerfile` | Apache Jena tools and Fuseki at a pinned version |

## Systems

| System | Image | Bulk path |
|---|---|---|
| NRESE | built in `rust:<rust-toolchain.toml version>-bookworm` | `nrese-server load` |
| QLever (latest of 8 Nov 2026, no release tags) | `adfreiburg/qlever:commit-0998359ab0` | `qlever-index -p true` |
| Jena 6.2.0 TDB2 (Fuseki) | `nrese-bench/jena:6.2.0` (`jena/Dockerfile`) | `tdb2.tdbloader --loader=parallel` |
| Oxigraph 0.5.13 | `ghcr.io/oxigraph/oxigraph:0.5.13` | `oxigraph load` |
| Virtuoso 7.2.17 Open Source | `openlink/virtuoso-opensource-7:7.2.17` | `ld_dir` + 6 × `rdf_loader_run` + `checkpoint`, excluding server start-up |

## Fairness checklist

The settings below describe the legacy scorecard. New comparisons use the
[suite protocol](../PROTOCOL.md); a fixed JVM
heap is not an equal total-memory budget. The current campaigns run on Office-PC,
with Phuoc-Yu reserved for compatible Office-built native binaries. Never compare
a result from one host with NRESE's from another.

- **Same input and host.** Every system reads the same file from the same Docker volume and writes to a fresh volume.
- **Each vendor's own bulk loader and tuning guidance:**
  - JVM tools get a 16 GB heap.
  - QLever gets 8 GB of sort memory.
  - Virtuoso gets 16 GB of buffers.
  - Everything else runs with defaults.
- **Timing.** Wall-clock from the start of the load until the store is durable and queryable. Container and JVM start-up are included (≈ 0.3–1 s); Virtuoso's server start-up is not.
- **Vendor defaults changed for fairness:**
  - QLever's query-result cache is off (`-k 0`: a 1 MB cap alone still kept every small result, such as a count, so until 1 October 2026 repeated runs of small-result queries were cache hits), so repeated runs measure evaluation. This is cache-disabled evaluation, not proof of cold memory or storage. The suite (`benches/suite`) runs systems with supported result-cache modes (QLever, NRESE) both ways: off and on. It also shuffles the query order and reports each query's first execution apart.
  - Virtuoso's result-row cap and cost-based query rejection are disabled, so it returns complete results.
  - NRESE's rate limits are lifted.
  - All systems use the same per-query timeout (`QUERY_TIMEOUT_S`, default 120 s).
- **Same data.** Real dumps contain a few lines strict parsers reject: 29 k of 229 M in Wikidata lexemes, plus some in DBpedia. They are dropped once for all systems (`nt-filter.py`).
- **The stores differ.** Each builds different indexes: QLever 6 permutations plus a pattern index, Virtuoso 2 full plus 3 partial indexes, TDB2 triple and quad indexes, NRESE 6 permutations plus a checkpoint. Load time alone doesn't say which store is better. It has to be read together with store size, peak memory, restart time and query speed.
- **Coverage:** LUBM, concurrent-client tools and Linux Office runs now exist in the
  wider suite. Their presence does not complete every competitor pair. BSBM and
  full Wikidata remain separate uncompleted scale work; consult the workload
  registry and dated run records instead of this legacy scorecard's scope.

## Cleaning up after a run

The legacy scorecard scripts call `scripts/bench-cleanup.sh --owner scorecard` when they exit, also when they fail or are interrupted. It removes by label only (every container and volume carries its job's namespace, checkout and tool: `scripts/lib/ns.sh`):
- the run's own containers and store volumes, never another job's on the host;
- the images the run pulled itself (an image that was already on the machine stays);
- inferred-set dumps (`*.inferred.nt`); the scorecard keeps their counts and comparisons.

The shared datasets (the volume `nrese-bench-data`, local datasets under `~/nrese-bench`) and the
images built here (`nrese-bench/*`) go only by hand: `--datasets`, `--images`.

Scorecards (CSV), logs and reports stay.

On a host with preserved inputs ([hosts.toml](../hosts.toml)) don't use that cleanup:
run the suite directly with `NRESE_BENCH_KEEP=1`, the inputs mounted read-only and
fresh writable stores outside the preservation root, and afterwards remove only the
run's own containers, stores and scratch by exact identity.
