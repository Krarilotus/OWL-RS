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

Results: one record per run in [../runs/](../runs/README.md).

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

The settings below describe the legacy scorecard. New comparisons use the
[suite protocol](../PROTOCOL.md) and the GraphDB qualification below; a fixed JVM
heap is not an equal total-memory budget. The current campaigns run on Office-PC,
with Phuoc-Yu reserved for compatible Office-built native binaries. Never compare
a GraphDB result from Office with an NRESE result from Phuoc.

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

## GraphDB comparison coverage and qualification (9 October 2026)

This is the coverage required by the current comparison, not a completed result
table. `ready` in the workload registry means some runnable machinery exists;
it does not mean GraphDB has an adapter or a validated result for that workload.
Every reported row must name the NRESE revision, GraphDB image digest and verified
edition, input hashes, ruleset, effective settings, repetitions and answer check.
Keep licensed measurements and detailed result tables in the ignored results
directory. A public run record carries only the permitted restricted status.

The current campaign uses GraphDB Free, as selected by the maintainer. Compare
both engines on Office-PC with the same one-CPU and total-memory allocation;
report one- and two-client results separately. Higher offered concurrency tests
Free's queueing behavior, not Enterprise scalability. Free documents one core,
two concurrent queries, five repositories and 32-bit entity IDs. Its Lucene
connector and GeoSPARQL support remain eligible workloads; Enterprise-only
connectors and clusters are outside this edition's comparison. See
[edition limits 11.5](https://graphdb.ontotext.com/documentation/11.5/licensing.html).

GraphDB gets its documented native paths and optimizations:

- Use offline `importrdf preload` for plain data. It performs no inference and
  skips plugins: inference and plugin-index construction must finish before a
  reasoning/search repository is called query-ready. Use parallel `importrdf load`
  for the reasoning track; report load, inference and time-to-query-ready without
  omitting required work. See [ImportRDF 11.5](https://graphdb.ontotext.com/documentation/11.5/loading-data-using-importrdf.html).
- Preserve native `sameAs` handling for equality-supporting regimes. The old
  templates disabled it globally; that setting can suppress entailments, not
  merely change storage. Validate expanded answers and bag multiplicities rather
  than require identical physical closure sizes. See [sameAs 11.5](https://graphdb.ontotext.com/documentation/11.5/sameas-optimisation.html).
- Record default and explicitly tuned variants separately. Size entity IDs and
  indexes for the data; use context, literal and predicate indexes where relevant.
  Leave optimizer/statistics support enabled. Do not force 40-bit IDs just because
  a licence permits them. See [index guidance 11.5](https://graphdb.ontotext.com/documentation/11.5/data-loading-query-optimisations.html).
- Give both systems the same total host resources. Account for heap, native memory,
  index caches, filesystem cache and any external search service. Record actual
  licence limits; a restricted edition cannot establish unrestricted multicore
  performance. See [memory guidance 11.5](https://graphdb.ontotext.com/documentation/11.5/configuring-graphdb-memory.html).
- Keep JVM warm-up, first-query latency and steady-state performance distinct.
  First query after restart is not proof of a cold OS page cache. Report cache
  modes only where the system actually exposes them; unsupported toggles must
  not create duplicate rows presented as different modes.
- Match requested semantics and transaction durability. Document optimized rulesets
  separately when they omit inferences; verify required answers before comparing
  speed. Full DL classification is a different task from rule materialization.

The complete registry maps to GraphDB as follows. The fast bridge overlaps the
other families and is not counted as another independent set of benchmarks.

| Registered workload | GraphDB comparison and purpose | Current machinery gap |
|---|---|---|
| `lubm` | 1/10/100/1000 universities; materialization, 14 queries, memory and storage | Existing suite path; qualify licensed inference and answers |
| `lubm-materialised` | 1/10/100; identical closed input, plain SPARQL query cost | Existing suite path; inference disabled for both |
| `owl2bench` | RL-1 and QL-1; profile-specific reasoning and 22 queries | Existing suite path; separate documented QL semantic exceptions |
| `basics-mix` | Olympics, YAGO, entities, DBpedia and Wikidata lexemes; load/restart/query mix | Existing suite path; qualify each size and its resource settings |
| `fast` | Supported query, reasoning, concurrency and cache cases from the 95-case registry | Partial bridge; commit, SHACL and canonicalization modes are not replayed |
| `write-scaling` | Batch uploads, individual inserts and sequential COUNT queries, 1M/10M tiers | Independent GraphDB route and numeric checks implemented; live empty-store setup and timing qualification pending |
| `ldbc-spb` | Semantic publishing with continuous updates, inference, text and spatial queries | Planned; use official generator, validation and workload mix, not a substitute labelled SPB |
| `integration-rg-gs-gnd` | Real identity links and competency questions | Partial; GraphDB adapter and cohort/full tiers open; publication permission separate |
| `real-ontologies` | DBpedia/YAGO with their actual schema; real materialization | Planned; schema inputs and equivalent entailment checks needed |
| `consistency` | Contradiction detection and rollback under supported rule semantics | Planned; enable consistency checks and verify committed state |
| `w3c-owl2-rl` | Expected entailments and non-entailments before timing reasoning | Existing NRESE conformance kit; GraphDB endpoint execution missing |
| `ore-2015` | Full DL/EL classification and canonical taxonomies | GraphDB has no matching full-DL classification adapter; report not comparable |
| `w3c-owl2-dl` | Direct-semantics DL conformance | Not a GraphDB rule-materialization performance comparison |
| `owl2bench-classify` | EL/DL classification and consistency | Planned DL kit work; not a GraphDB full-DL capability claim |
| `sparqloscope` | Broad generated SPARQL operator/query coverage | Planned; official data, driver and qualification needed |
| `bsbm` | E-commerce exploration, analytics and updates | Planned; use official qualification and mixed workload |
| `watdiv` | Diverse join shapes at 10M/100M | Planned; generator/input identity and answer qualification needed |
| `w3c-sparql11` | SPARQL query/update correctness | Existing NRESE kit; GraphDB endpoint execution missing |
| `w3c-shacl` | Expected validation reports | Existing NRESE kit; GraphDB validation adapter missing |
| `era-shacl` | Railway shapes/data; validation time and memory | Planned; data rights and batch versus commit-validation semantics to qualify |
| `fulltext-search` | Native indexed text and RDF joins | Planned; match tokenizer, language, ranking and connector dependencies |
| `geosparql` | Compliance, then spatial performance | Partial NRESE compliance kit; GraphDB adapter and Geographica performance track missing |
| `federation` | Remote joins, endpoint latency and completeness | Planned; same remote data, topology and failure model |
| `clients` | Client/protocol integration compatibility | Existing NRESE smoke tools; distinguish GraphDB-compatible operations from NRESE-only APIs |

Incremental maintenance is a required separate operation sequence, not a load/query
proxy: insert and delete ABox facts, alter schemas, split/merge equality classes,
and check inferred content before any full recomputation. Where the schema is
immutable, allow GraphDB's documented read-only schema optimization; mutable-schema
cases must actually exercise schema updates. See [reasoning and retraction 11.5](https://graphdb.ontotext.com/documentation/11.5/reasoning.html).
Cluster/failover and distributed scaling remain unqualified; two available benchmark
PCs alone do not establish a fair distributed-system comparison.

For every executable row, report correctness/completeness first, then load and
inference time, first/warm query latency, throughput and tail latency, update cost,
peak total memory and disk footprint as applicable. Missing adapters, unsupported
semantics, wrong answers, resource exhaustion and timeouts remain visible. Do not
select a winner from only the queries both engines happened to finish. Use three
independent interleaved repetitions for qualification and the protocol's ten for
a publishable claim; private measurements remain subject to the licence policy.

## Cleaning up after a run

The legacy scorecard scripts call `scripts/bench-cleanup.sh` when they exit, also when they fail or are interrupted. Its broad name-based cleanup removes:
- the run's containers and store volumes;
- the dataset volume `nrese-bench-data` and local datasets under `~/nrese-bench`;
- the images built here (`nrese-bench/*`) and the images the run pulled itself (an image that was already on the machine stays);
- inferred-set dumps (`*.inferred.nt`); the scorecard keeps their counts and comparisons.

Scorecards (CSV), logs and reports stay.

Do not use that cleanup on the shared Office/Phuoc assets or current campaigns.
Run the suite directly with `NRESE_BENCH_KEEP=1`, immutable inputs mounted read-only
and fresh writable stores outside the preservation root. After final use, remove
only the run's verified owned containers, stores and scratch by exact identity;
retain results, reproducibility metadata and assets needed for scheduled reuse.
An unused resource or a familiar benchmark name is not proof it is disposable.
The designated paths and verified migration receipts are in
[the benchmark host guide](../README.md#remote-hosts-and-shared-data).

## Getting licences

- **RDFox:** <https://www.oxfordsemantic.tech/free-trial>. It requires an institutional email and acceptance of the [evaluation licence](https://www.oxfordsemantic.tech/rdfox-evaluation-license).
- **GraphDB Free licence (required since GraphDB 11.0):** request GraphDB Free on the Graphwise download page and the licence arrives by email. Run the scorecard with `GRAPHDB_LICENSE=/path/graphdb.license`. Without it, GraphDB still bulk-loads but answers every query with "No license was set".
- **GraphDB Enterprise:** a comparison requires an existing valid licence for the
  measured host and core count. Enterprise also differs in concurrency, connectors,
  entity-ID capacity and clustering; it is not only a parallel-inference option.
  Acquiring a licence or requesting publication permission is a separate maintainer
  decision. The current campaign uses the existing Free key.
