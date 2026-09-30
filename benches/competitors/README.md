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
| GraphDB Free | Ontotext Free licence | **only with Ontotext's written permission** (Art. 15.3). Reverse engineering, including "underlying ideas or algorithms", is prohibited (Art. 12.7); black-box timing is not reverse engineering |
| RDFox (evaluation licence) | OST evaluation licence | **only with OST's approval**; apply ≥ 30 days before publishing (§2.2, §2.3). Results used in papers must be reported to OST (§2.4) |
| AnzoGraph DB / Altair Graph Lakehouse (Free Edition) | Cambridge Semantics EULA | **only with CSI's prior written consent** (§3(h)). Needs their licence key (§3(j)); the Free Edition is limited to 8 GB RAM, or 16 GB if registered |

**Therefore:**
- Raw results go to `benches/competitors/results/`, which is git-ignored.
- Only numbers for the free-to-publish systems may appear in committed docs.
- GraphDB and RDFox numbers stay local until written permission exists. Record the permission (who, when, scope) in this file before publishing.
- Our designs come from the published literature cited in `docs/design/`. We never derive them from observing the vendors' products.

Results: [SCORECARD.md](SCORECARD.md) (free-to-publish systems only).

## What's here

| File | Purpose |
|---|---|
| `prepare-datasets.sh [names...]` | Downloads the real-world datasets and converts each to one validated N-Triples file in the Docker volume `nrese-bench-data`: `olympics` (1.8 M triples), `yago-tiny` (23 M), `dbpedia-core` (67 M), `wikidata-lexemes-<N>m` (first N million of 229 M) and full `wikidata-lexemes` |
| `nt-filter.py` | Drops the few lines strict parsers reject (invalid IRIs or language tags) before any system sees the file, so all systems get identical input. Reports how many lines it drops |
| `scorecard.sh <dataset> [systems...]` | **Pf0 scorecard** per system: load time, peak load memory, store bytes (per triple), restart time, server memory, query mix (1 warm-up + 5 runs per query, p50), and a cross-check that all systems return the same result counts. `LOAD_ONLY=1` skips the queries. Synthetic data comes from the harness: `generate --triples N` → `entities-N` |
| `queries/<dataset>/*.rq` | Query mixes: point lookups, star and multi-hop joins, aggregates, numeric/date/text filters, OPTIONAL, negation, property paths, full scans, large streamed results |
| `graphdb/repo-*.ttl` | GraphDB repository configs: `empty` for load comparisons; `rdfsplus-optimized` and `owl2-rl` for the reasoning comparisons (M3) |
| `jena/Dockerfile` | Apache Jena tools and Fuseki at a pinned version |

## Systems

| System | Image | Bulk path |
|---|---|---|
| NRESE | built in `rust:<rust-toolchain.toml version>-bookworm` | `nrese-server load` |
| QLever | `adfreiburg/qlever:latest` | `qlever-index -p true` |
| GraphDB 11.5.1 Free | `ontotext/graphdb:11.5.1` | `importrdf preload`, ruleset `empty`; queries need `GRAPHDB_LICENSE` |
| Jena 6.2.0 TDB2 (Fuseki) | `nrese-bench/jena:6.2.0` (`jena/Dockerfile`) | `tdb2.tdbloader --loader=parallel`. `tdb2.xloader` fails on real data in 6.2.0 (Thrift "unknown type 15" while sorting terms; the synthetic data loads) |
| Oxigraph 0.5.11 | `ghcr.io/oxigraph/oxigraph:0.5.11` | `oxigraph load` |
| Virtuoso 7.2.17 Open Source | `openlink/virtuoso-opensource-7:7.2.17` | `ld_dir` + 6 × `rdf_loader_run` + `checkpoint`, excluding server start-up |
| RDFox 7.6b | `oxfordsemantic/rdfox:7.6b` | sandbox `import`; needs `RDFOX_LICENSE=/path/RDFox.lic` |
| AnzoGraph DB 3.5.0 Free (Altair Graph Lakehouse) | `cambridgesemantics/anzograph:3.5.0` | `LOAD WITH 'global' <file:...>` into the running in-memory server (start-up excluded); at most 8 GB RAM unregistered. Run explicitly: `scorecard.sh <dataset> anzograph`. Results stay local (EULA §3(h)) |

## Fairness checklist

- **Same input and host.** Every system reads the same file from the same Docker volume and writes to a fresh volume.
- **Each vendor's own bulk loader and tuning guidance:**
  - JVM tools get a 16 GB heap.
  - QLever gets 8 GB of sort memory.
  - Virtuoso gets 16 GB of buffers.
  - Everything else runs with defaults.
- **Timing.** Wall-clock from the start of the load until the store is durable and queryable. Container and JVM start-up are included (≈ 0.3–1 s); Virtuoso's server start-up is not.
- **Vendor defaults changed for fairness:**
  - QLever's query-result cache is capped at 1 MB, so repeated runs measure evaluation rather than cache hits.
  - Virtuoso's result-row cap and cost-based query rejection are disabled, so it returns complete results.
  - NRESE's rate limits are lifted.
  - All systems use the same per-query timeout (`QUERY_TIMEOUT_S`, default 120 s).
- **Same data.** Real dumps contain a few lines strict parsers reject: 29 k of 229 M in Wikidata lexemes, plus some in DBpedia. They are dropped once for all systems (`nt-filter.py`).
- **The stores differ.** Each builds different indexes: QLever 6 permutations plus a pattern index, Virtuoso 2 full plus 3 partial indexes, TDB2 triple and quad indexes, NRESE 6 permutations plus a checkpoint. Load time alone doesn't say which store is better. It has to be read together with store size, peak memory, restart time and query speed.
- **Still missing:**
  - throughput with concurrent clients, and write latency under read load
  - LUBM and BSBM generators, for reasoning and mixed workloads
  - full Wikidata, which needs a large server
  - runs on a dedicated Linux reference machine instead of Docker Desktop

## Cleaning up after a run

A run leaves nothing behind but its results. The scorecard scripts call `scripts/bench-cleanup.sh` when they exit, also when they fail or are interrupted. It removes:
- the run's containers and store volumes;
- the dataset volume `nrese-bench-data` and local datasets under `~/nrese-bench`;
- the images built here (`nrese-bench/*`) and the images the run pulled itself (an image that was already on the machine stays);
- inferred-set dumps (`*.inferred.nt`); the scorecard keeps their counts and comparisons.

Scorecards (CSV), logs and reports stay.

For several runs in a row, set `NRESE_BENCH_KEEP=1` so datasets and images survive between them, and run `scripts/bench-cleanup.sh` once at the end. `--dry-run` shows what it would remove. It only removes things by the names these scripts use, never "everything unused", so other projects' containers, volumes and images are safe.

## Getting licences

- **RDFox:** <https://www.oxfordsemantic.tech/free-trial>. It requires an institutional email and acceptance of the [evaluation licence](https://www.oxfordsemantic.tech/rdfox-evaluation-license).
- **GraphDB Free licence (required since GraphDB 11.0):** request GraphDB Free on the Graphwise download page and the licence arrives by email. Run the scorecard with `GRAPHDB_LICENSE=/path/graphdb.license`. Without it, GraphDB still bulk-loads but answers every query with "No license was set".
- **GraphDB Enterprise evaluation:** email `graphdb-info@ontotext.com` and ask for an evaluation licence. It's only needed to compare against Enterprise's parallel inference in M3. Ask about publishing permission in the same email.
