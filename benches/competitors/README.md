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

## What's here

| File | Purpose |
|---|---|
| `run-load-comparison.sh <triples> [runs] [systems...]` | Bulk-load comparison. It generates or stages the dataset, builds NRESE for Linux, runs each system's bulk loader, and prints `system,triples,run,wall_ms` |
| `graphdb/repo-*.ttl` | GraphDB repository configs: `empty` for load comparisons; `rdfsplus-optimized` and `owl2-rl` for the reasoning comparisons (M3) |
| `jena/Dockerfile` | Apache Jena command-line tools (TDB2 loaders) at a pinned version |

## Systems

| System | Image | Bulk path |
|---|---|---|
| NRESE | built in `rust:1.91-bookworm` | `nrese-server load` |
| QLever | `adfreiburg/qlever:latest` | `qlever-index -p true` |
| GraphDB 11.5.1 Free | `ontotext/graphdb:11.5.1` | `importrdf preload`, ruleset `empty` |
| Jena 6.2.0 TDB2 (Fuseki) | `nrese-bench/jena:6.2.0` (`jena/Dockerfile`) | `tdb2.tdbloader --loader=parallel`, and `tdb2.xloader` |
| Oxigraph 0.5.11 | `ghcr.io/oxigraph/oxigraph:0.5.11` | `oxigraph load` |
| Virtuoso 7.2.17 Open Source | `openlink/virtuoso-opensource-7:7.2.17` | `ld_dir` + 6 × `rdf_loader_run` + `checkpoint`, excluding server start-up |
| RDFox 7.6b | `oxfordsemantic/rdfox:7.6b` | sandbox `import`; needs `RDFOX_LICENSE=/path/RDFox.lic` |

## Fairness checklist

- **Same input and host.** Every system reads the same file from the same Docker volume and writes to a fresh volume.
- **Each vendor's own bulk loader and tuning guidance:**
  - JVM tools get a 16 GB heap.
  - QLever gets 8 GB of sort memory.
  - Virtuoso gets 16 GB of buffers.
  - Everything else runs with defaults.
- **Timing.** Wall-clock from the start of the load until the store is durable and queryable. Container and JVM start-up are included (≈ 0.3–1 s); Virtuoso's server start-up is not.
- **The stores differ.** Each builds different indexes: QLever 6 permutations plus a pattern index, Virtuoso 2 full plus 3 partial indexes, TDB2 triple and quad indexes, NRESE 6 permutations plus a checkpoint. Load time alone doesn't say which store is better. It has to be read together with store size, peak memory, restart time and query speed.
- **Still missing:**
  - store size on disk and peak memory per system
  - restart time and query latency after the load
  - realistic datasets besides the synthetic one (LUBM, BSBM, a Wikidata or DBpedia slice)
  - runs on a dedicated Linux reference machine instead of Docker Desktop

## Getting licences

- **RDFox:** <https://www.oxfordsemantic.tech/free-trial>. It requires an institutional email and acceptance of the [evaluation licence](https://www.oxfordsemantic.tech/rdfox-evaluation-license).
- **GraphDB Enterprise evaluation:** email `graphdb-info@ontotext.com` and ask for an evaluation licence. It's only needed to compare against Enterprise's parallel inference in M3. Ask about publishing permission in the same email.
