# Benchmark status

Generated 2026-10-12 by `python benches/suite/suite.py status --write` from [suite/systems.toml](suite/systems.toml), [suite/workloads.toml](suite/workloads.toml) and the run records in [runs/](runs/README.md). Don't edit it by hand: change those and regenerate. How to read and extend it: [README.md](README.md).

## At a glance

- **Workloads:** 41 (13 partial, 15 planned, 13 ready)
- **Systems:** 33 (28 free to publish, 5 only with the vendor's permission)
- **Pairs** (standard tier × system that can run it; build variants apart): 608; run at least once: 103; ok on their newest run: 80
- **NRESE:** 19 of its 97 standard tiers ok on their newest run
- **Run records:** 35, newest 2026-10-12

## Schedule

The order the families are measured in ([suite/schedule.toml](suite/schedule.toml), the owner's of 12 Oct 2026): the measurement queue starts jobs in it. A family without a workload entry yet is a benchmark of [CATALOG.md](CATALOG.md), named by its section.

### This week, in this order

| # | Family | State | Newest run | What |
|---|---|---|---|---|
| 1 | sparqloscope | ready | - | Sparqloscope: 105 generated queries covering the SPARQL 1.1 features one by one |
| 2 | bsbm | ready | - | Berlin SPARQL Benchmark V3.1: e-commerce explore and business intelligence mixes, 1 to 64 clients |
| 3 | watdiv | partial | - | WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test |
| 4 | gcare | planned | - | G-CARE: cardinality estimation for subgraph queries (SIGMOD 2020) |
| 5 | owl2bench | ready | 2026-10-12 | OWL2Bench: one TBox per OWL 2 profile, 22 queries |
| 6 | lubm | ready | 2026-10-09 | LUBM: universities, 14 queries |
| 7 | claros | partial | - | Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques |
| 8 | ldbc-spb | partial | - | LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates |
| 9 | datalog-graphs | partial | - | Datalog graph programs: transitive closure, same generation, CSPA |
| 10 | commit-latency | partial | - | Commit latency under 0, 1, 8 and 64 readers, the closure complete at every commit |
| 11 | ore-2015 | partial | 2026-10-03 | ORE 2015: classification of 1,920 real ontologies |
| 12 | elk-ontologies | planned | - | Classification of large EL and near-EL ontologies: GO, ChEBI, NCIt, GALEN |
| 13 | pagoda-suite | planned | - | PAGOdA's test suite: query answering beyond OWL 2 RL |
| 14 | vector-recall | partial | - | Filtered k-NN: recall and time of vector search under a filter |
| 15 | ann-benchmarks | partial | - | ann-benchmarks: recall and throughput curves of approximate k-NN |
| 16 | geographica2 | planned | - | Geographica 2: GeoSPARQL micro and macro workloads on real data, synthetic and scalability sets |
| 17 | era-shacl | partial | - | ERA-SHACL-Benchmark: validation time and memory on a real railway knowledge graph |
| 18 | fast | partial | 2026-10-10 | The fast suite's cases on their comparators: each case under the semantics a system supports |

### Backlog

| # | Family | State | Newest run | What |
|---|---|---|---|---|
| 1 | lubm-materialised | ready | 2026-10-03 | LUBM with the closure precomputed: the 14 queries on every SPARQL engine |
| 2 | owl2bench-classify | planned | - | OWL2Bench EL and DL: classification and consistency of the generated ontologies |
| 3 | real-ontologies | planned | - | DBpedia and YAGO with their ontologies: materialisation over real hierarchies |
| 4 | consistency | planned | - | Injected contradictions: detection and explanation |
| 5 | integration-rg-gs-gnd | partial | - | RG x GS x GND integration: identity reasoning over real links, 10 competency questions |
| 6 | fulltext-search | planned | - | Keyword search: ResearchSpace's search queries; LUBMft |
| 7 | federation | planned | - | FedBench / W3C federation tests |
| 8 | write-scaling | ready | 2026-10-09 | Single-statement and batch writes at 1 M, 10 M, 100 M; writes under read load |
| 9 | basics-mix | ready | 2026-10-03 | Load, store size, restart and a query mix on real datasets |
| 10 | wdbench | planned | - | WDBench: about 2,650 real queries from Wikidata's logs (BGPs, OPTIONAL, paths, C2RPQs) |
| 11 | sp2bench | ready | - | SP2Bench: DBLP-shaped documents, 17 queries (OPTIONAL, negation, long chains, ASK) |
| 12 | npd | catalogue only | - | CATALOG.md §3.6: NPD, OBDA over the Norwegian Petroleum Directorate (Ontop's home turf) |
| 13 | lubm-exists | catalogue only | - | CATALOG.md §3.6: LUBM∃, LUBM with existential rules (Ontop, Stardog, PAGOdA) |
| 14 | starbench | partial | - | StarBench: 56 SPARQL-star queries over BKR (61 M statements as RDF-star) |
| 15 | fedshop | planned | - | FedShop200: federated BSBM-like shops, 20 to 200 members, 120 queries |
| 16 | beir | catalogue only | - | CATALOG.md §3.9: BEIR, retrieval quality and time |
| 17 | vibe | planned | - | VIBE: recall against throughput on modern embeddings, in and out of distribution |
| 18 | vector-dbbench | planned | - | VectorDBBench: load, recall, QPS and p99 of vector databases, with and without filters |
| 19 | soak | catalogue only | - | CATALOG.md §3.16: soak, hours of mixed load |
| 20 | larger-than-memory | planned | - | LUBM 1000 under memory caps: load, OWL 2 RL closure and the 14 queries at 16, 12, 8, 6, 4 and 2 GB |
| 21 | sspe | planned | - | SSPE: single-source path graphs where one deletion invalidates long derivations |
| 22 | doop | catalogue only | - | CATALOG.md §3.4: Doop on DaCapo, Soufflé's own workload (programs vendored in benches/reasoning/datalog) |
| 23 | galen | catalogue only | - | CATALOG.md §3.4: Galen as Datalog, FlowLog's suite (a case of benches/reasoning/datalog) |
| 24 | reactome-uniprot | catalogue only | - | CATALOG.md §3.4: Reactome and UniProt, RDFox's and GLog's datasets |
| 25 | chasebench | catalogue only | - | CATALOG.md §3.4: ChaseBench, existential rules (Nemo, VLog, RDFox) |
| 26 | uobm | catalogue only | - | CATALOG.md §3.4: UOBM, LUBM with richer OWL (GLog's package has its L program) |

### Last: not expected to give data for polishing or optimisation

| # | Family | State | Newest run | What |
|---|---|---|---|---|
| 1 | w3c-owl2-rl | ready | 2026-10-09 | W3C OWL 2 test cases, RL profile: entailment and consistency |
| 2 | w3c-owl2-dl | ready | 2026-10-09 | W3C OWL 2 test cases, species DL, direct semantics: consistency, entailment, non-entailment |
| 3 | w3c-sparql11 | ready | 2026-10-09 | W3C SPARQL 1.1 test suite: query, update, syntax, result formats |
| 4 | w3c-shacl | ready | 2026-10-09 | W3C SHACL test suite |
| 5 | geosparql | partial | 2026-10-09 | GeoSPARQL compliance benchmark (206 queries); Geographica 2 for performance |
| 6 | clients | ready | - | ResearchSpace on the store; the Datamodel Workflow's export protocol |

## Coverage

Per workload and system: standard tiers with an `ok` (or `ran*`) newest outcome / standard tiers. A `!` marks a newest outcome that is `FAIL`, `T/O`, `WRONG` or `part` on some tier.

| Workload | State | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | nemo | nemo-schemafirst | nemo-sparq | souffle | vlog | glog | owlrl | hermit | openllet | konclude | elk | graphdb | rdfox | allegrograph | stardog | anzograph | qdrant | milvus | pgvector | lancedb | topbraid | pyshacl | maplib | corese |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| lubm | ready | 4/4 | · | 3/4 | – | – | – | 1/4 ! | – | – | 3/4 | 3/4 | 3/4 ! | – | – | – | 1/4 | – | – | – | – | 4/4 | · | – | n/a | n/a | – | – | – | – | – | – | – | – |
| lubm-materialised | ready | 3/3 | · | · | 3/3 | 3/3 | 3/3 | 3/3 | · | 3/3 | – | – | – | – | – | – | – | – | – | – | – | 3/3 | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| owl2bench | ready | 2/2 | · | 1/2 | – | – | – | 1/2 | – | – | 2/2 | 2/2 | · | – | – | – | 1/2 ! | – | – | – | – | 2/2 | · | – | n/a | n/a | – | – | – | – | – | – | – | – |
| ldbc-spb | partial | · | n/a | n/a | – | – | – | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| integration-rg-gs-gnd | partial | · | · | · | – | – | – | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | – | n/a | n/a | – | – | – | – | – | – | – | – |
| real-ontologies | planned | · | · | · | – | – | – | · | – | · | · | · | · | – | – | – | · | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| consistency | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| w3c-owl2-rl | ready | 0/1 ! | n/a | n/a | – | – | – | – | – | – | n/a | n/a | n/a | – | – | – | n/a | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| ore-2015 | partial | 0/2 ! | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | 1/2 | 0/2 ! | 0/2 ! | 1/2 | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| w3c-owl2-dl | ready | 0/1 ! | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | 0/1 ! | 0/1 ! | 0/1 ! | 0/1 ! | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| owl2bench-classify | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| basics-mix | ready | 5/5 | · | 3/5 ! | 4/5 ! | 4/5 ! | 2/5 ! | 5/5 | · | 2/5 ! | – | – | – | – | – | – | – | – | – | – | – | 5/5 | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| sparqloscope | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| bsbm | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| watdiv | partial | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| sp2bench | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| gcare | planned | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| wdbench | planned | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| write-scaling | ready | 2/2 | n/a | n/a | n/a | n/a | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| w3c-sparql11 | ready | 1/1 | n/a | n/a | n/a | n/a | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| w3c-shacl | ready | 1/1 | n/a | n/a | – | – | – | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | n/a | n/a | n/a | n/a |
| era-shacl | partial | · | · | · | – | – | – | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| fulltext-search | planned | · | · | · | · | – | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | – | – | – | – | – | – | – | – | – |
| geosparql | partial | 0/1 ! | n/a | n/a | n/a | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – | – |
| federation | planned | · | · | · | · | · | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | · | – | – | – | – | – | – | – | – |
| starbench | partial | · | · | – | – | · | – | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | – | – | n/a | – | – | – | – | – | – | – | – | – |
| geographica2 | planned | · | · | · | · | – | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | – | – | – | – | – | – | – | – | – |
| fedshop | planned | · | · | · | · | · | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | · | – | – | – | – | – | – | – | – |
| vibe | planned | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | – | – | – |
| vector-dbbench | planned | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | – | – | – |
| larger-than-memory | planned | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| fast | partial | 1/3 | · | · | 2/3 | 1/3 ! | 1/3 | 0/3 ! | · | · | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| clients | ready | · | n/a | n/a | n/a | n/a | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| claros | partial | · | n/a | n/a | – | – | – | n/a | – | – | · | n/a | n/a | · | · | · | n/a | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| sspe | planned | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – |
| datalog-graphs | partial | · | n/a | – | – | – | – | n/a | – | – | · | n/a | n/a | · | · | · | – | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| commit-latency | partial | · | n/a | n/a | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | – | – | – | – | – | – | – | – | – | – |
| elk-ontologies | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| pagoda-suite | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| vector-recall | partial | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| ann-benchmarks | partial | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |

`ok` every repetition loaded, every step and query execution ok · `part` a load, step or query execution failed or timed out in some repetition · `WRONG` an answer count differs from the expected one · `FAIL`/`T/O` the load failed or timed out · `skip` the suite skipped it (reason in the record) · `≠?` its answers differ from another system's, not yet adjudicated · `hang` a comparator gave no answer in a past run · `ran*` ran; a licensed system whose outcome stays local · `·` can run, never run · `–` lacks a capability · `n/a` no adapter or kit for it

## Workloads, tier by tier

### lubm: LUBM: universities, 14 queries

ready, performance; tasks RT1, RT3, RT2, RT5; kit `benches/reasoning` (suite driver); checked by: inferred set against owlrl or Nemo; the published answers for LUBM(1).

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | ok 10-09 | · | ok 10-02 | ok 10-04 | ok 10-04 | ok 10-04 | ok 10-04 | ok 10-04 | ran* 10-04 | skip 10-02 |
| 10 | ok 10-04 | · | ok 10-02 | part 10-04 | ok 10-04 | ok 10-04 | ok 10-04 | skip 10-04 | ran* 10-04 | skip 10-02 |
| 100 | ok 10-04 | · | ok 10-02 | part 10-04 | ok 10-04 | ok 10-04 | ok 10-04 | skip 10-02 | ran* 10-04 | skip 10-02 |
| 1000 | ok 10-04 | · | · | · | · | · | FAIL 10-04 | · | ran* 10-04 | · |
| diagnostic-1-Office-PC-client-sweep (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · |
| diagnostic-1-Office-PC-queries (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · |
| diagnostic-1-Phuoc-Yu-client-sweep (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · |
| diagnostic-1-Phuoc-Yu-queries (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · |

### lubm-materialised: LUBM with the closure precomputed: the 14 queries on every SPARQL engine

ready, performance; tasks RT5; kit `benches/reasoning` (suite driver); checked by: row counts across systems; the published answers for LUBM(1).

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | ok 10-03 | · | · | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | · | ok 10-03 | ran* 10-03 | · | · |
| 10 | ok 10-03 | · | · | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | · | ok 10-03 | ran* 10-03 | · | · |
| 100 | ok 10-03 | · | · | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | · | ok 10-03 | ran* 10-03 | · | · |

### owl2bench: OWL2Bench: one TBox per OWL 2 profile, 22 queries

ready, performance; tasks RT1, RT3; kit `benches/reasoning` (suite driver); checked by: inferred set against owlrl or Nemo; answer counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|---|
| rl-1 | ok 10-12 | · | ok 10-02 | ok 10-04 | ok 10-04 | ok 10-04 | skip 10-04 | T/O 10-04 | ran* 10-04 | skip 10-02 |
| ql-1 | ok 10-12 | · | · | skip 10-04 | ok 10-04 | ok 10-04 | skip 10-04 | ok 10-04 | ran* 10-04 | · |

### ldbc-spb: LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates

partial, performance; tasks RT3, RT5; kit `benches/reasoning/spb` (suite driver); checked by: the benchmark's own validation of query results and of its OWL 2 RL rules.

| Tier | nrese | graphdb |
|---|---|---|
| 50m | · | · |
| 256m | · | · |

### integration-rg-gs-gnd: RG x GS x GND integration: identity reasoning over real links, 10 competency questions

partial, performance; tasks RT1, RT3; kit `benches/integration` (suite driver); checked by: answer counts across systems within one entailment regime.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | graphdb | rdfox |
|---|---|---|---|---|---|---|
| example | · | · | · | · | · | · |
| cohort | · | · | · | · | · | · |
| full | · | · | · | · | · | · |

### real-ontologies: DBpedia and YAGO with their ontologies: materialisation over real hierarchies

planned, performance; tasks RT1, RT3; kit `-` (outside the driver); checked by: inferred set against Nemo.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | rdf4j | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| dbpedia-core | · | · | · | · | · | · | · | · | · | · | · |
| yago-tiny | · | · | · | · | · | · | · | · | · | · | · |

### consistency: Injected contradictions: detection and explanation

planned, performance; tasks RT4, RT7; kit `-` (outside the driver); checked by: every injected contradiction found, none on clean data.

| Tier | nrese | nrese-fsst | nrese-oxigraph | graphdb |
|---|---|---|---|---|
| lubm-1 | · | · | · | · |
| owl2bench-rl-1 | · | · | · | · |

### w3c-owl2-rl: W3C OWL 2 test cases, RL profile: entailment and consistency

ready, conformance; tasks RT1, RT4; kit `crates/nrese-store/tests/w3c_owl2_rl` (suite driver); checked by: the test suite's expected entailments.

| Tier | nrese |
|---|---|
| - | part 10-09 |

### ore-2015: ORE 2015: classification of 1,920 real ontologies

partial, performance; tasks RT6; kit `benches/reasoning/dl` (outside the driver); checked by: canonical taxonomies compared by hash across the reference reasoners; differences adjudicated (disputed.tsv).

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk |
|---|---|---|---|---|---|---|---|
| dev | part 10-03 | · | · | ok 10-03 | part 10-03 | part 10-03 | ok 10-03 |
| full | · | · | · | · | · | · | · |

### w3c-owl2-dl: W3C OWL 2 test cases, species DL, direct semantics: consistency, entailment, non-entailment

ready, conformance; tasks RT4, RT6; kit `benches/reasoning/dl` (outside the driver); checked by: the test types; the working group's published results of 2009 (published-2009.tsv); disputed.tsv.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk |
|---|---|---|---|---|---|---|---|
| - | part 10-09 | · | · | part 10-03 | WRONG 10-03 | WRONG 10-03 | part 10-03 |

### owl2bench-classify: OWL2Bench EL and DL: classification and consistency of the generated ontologies

planned, performance; tasks RT4, RT6; kit `benches/reasoning/dl` (outside the driver); checked by: canonical taxonomies across the reference reasoners.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk |
|---|---|---|---|---|---|---|---|
| el-1 | · | · | · | · | · | · | · |
| dl-1 | · | · | · | · | · | · | · |
| dl-10 | · | · | · | · | · | · | · |

### basics-mix: Load, store size, restart and a query mix on real datasets

ready, performance; tasks basics; kit `benches/competitors` (suite driver); checked by: row counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| olympics | ok 10-03 | · | ok 10-02 | ok 10-03 | part 10-03 | WRONG 10-03 | ok 10-03 | · | part 10-03 | ran* 10-03 | skip 10-02 | · |
| yago-tiny | ok 10-03 | · | skip 10-02 | ok 10-03 | ok 10-03 | part 10-03 | ok 10-03 | · | ok 10-03 | ran* 10-03 | skip 10-02 | · |
| entities-10000000 | ok 10-03 | · | ok 10-02 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | · | ok 10-03 | ran* 10-03 | skip 10-02 | · |
| dbpedia-core | ok 10-03 | · | ≠? 10-02 | part 10-03 | ok 10-03 | WRONG 10-03 | ok 10-03 | · | FAIL 10-03 | ran* 10-03 | skip 10-02 | · |
| wikidata-lexemes-60m | ok 10-03 | · | ok 10-02 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | · | FAIL 10-03 | ran* 10-03 | skip 10-02 | · |

### sparqloscope: Sparqloscope: 105 generated queries covering the SPARQL 1.1 features one by one

ready, performance; tasks basics; kit `benches/sparql` (suite driver); checked by: the answers every engine of the paper published alike (66 of 105), the others against the reference systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| sparqloscope-dblp | · | · | · | · | · | · | · | · | · | · | · | · |

### bsbm: Berlin SPARQL Benchmark V3.1: e-commerce explore and business intelligence mixes, 1 to 64 clients

ready, performance; tasks basics; kit `benches/sparql/bsbm` (suite driver); checked by: the driver's qualification: each system's answers against a reference system's.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| bsbm-explore-100m | · | · | · | · | · | · | · | · | · | · | · | · |
| bsbm-bi-100m | · | · | · | · | · | · | · | · | · | · | · | · |

### watdiv: WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test

partial, performance; tasks basics; kit `benches/sparql` (suite driver); checked by: answers against the reference systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| watdiv-10m | · | · | · | · | · | · | · | · | · | · | · | · |
| watdiv-100m | · | · | · | · | · | · | · | · | · | · | · | · |

### sp2bench: SP2Bench: DBLP-shaped documents, 17 queries (OPTIONAL, negation, long chains, ASK)

ready, performance; tasks basics; kit `benches/sparql` (suite driver); checked by: the result sizes of the SP2Bench paper (Table V).

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| sp2bench-1m | · | · | · | · | · | · | · | · | · | · | · | · |
| sp2bench-25m | · | · | · | · | · | · | · | · | · | · | · | · |

### gcare: G-CARE: cardinality estimation for subgraph queries (SIGMOD 2020)

planned, performance; tasks basics; kit `benches/sparql/gcare` (outside the driver); checked by: the published true cardinalities; q-errors of the planner's estimates (explain=plan) against G-CARE's estimators.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| gcare-aids | · | · | · | · | · | · | · | · | · | · | · |
| gcare-human | · | · | · | · | · | · | · | · | · | · | · |
| gcare-lubm80 | · | · | · | · | · | · | · | · | · | · | · |
| gcare-yago | · | · | · | · | · | · | · | · | · | · | · |

### wdbench: WDBench: about 2,650 real queries from Wikidata's logs (BGPs, OPTIONAL, paths, C2RPQs)

planned, performance; tasks basics; kit `benches/sparql` (outside the driver); checked by: answers against the reference systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| wdbench | · | · | · | · | · | · | · | · | · | · | · |

### write-scaling: Single-statement and batch writes at 1 M, 10 M, 100 M; writes under read load

ready, performance; tasks basics, RT5; kit `benches/nrese-bench-harness` (suite driver); checked by: the written statements are read back.

| Tier | nrese | graphdb |
|---|---|---|
| 1m | ok 10-03 | · |
| 10m | ok 10-03 | · |
| diagnostic-http-10m (extra) | ok 10-09 | · |
| diagnostic-http-1m (extra) | ok 10-09 | · |

### w3c-sparql11: W3C SPARQL 1.1 test suite: query, update, syntax, result formats

ready, conformance; tasks basics; kit `crates/nrese-sparql/tests/w3c_sparql11` (suite driver); checked by: the suite's expected results.

| Tier | nrese |
|---|---|
| - | ok 10-09 |

### w3c-shacl: W3C SHACL test suite

ready, conformance; tasks validation; kit `crates/nrese-shacl/tests` (suite driver); checked by: the suite's expected reports.

| Tier | nrese |
|---|---|
| - | ok 10-09 |

### era-shacl: ERA-SHACL-Benchmark: validation time and memory on a real railway knowledge graph

partial, performance; tasks validation; kit `benches/breadth/era-shacl` (outside the driver); checked by: every report against TopBraid's (the reference implementation of the spec's editor): results as (focus, path, value, component, severity) bags.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | rdf4j | graphdb |
|---|---|---|---|---|---|---|
| ES | · | · | · | · | · | · |
| FR | · | · | · | · | · | · |
| ERA | · | · | · | · | · | · |

### fulltext-search: Keyword search: ResearchSpace's search queries; LUBMft

planned, performance; tasks search; kit `-` (outside the driver); checked by: result sets across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | virtuoso | jena | rdf4j | graphdb |
|---|---|---|---|---|---|---|---|---|
| - | · | · | · | · | · | · | · | · |

### geosparql: GeoSPARQL compliance benchmark (206 queries); Geographica 2 for performance

partial, conformance; tasks space; kit `crates/nrese-sparql/tests/geosparql_compliance` (suite driver); checked by: the benchmark's expected answers.

| Tier | nrese |
|---|---|
| compliance | part 10-09 |

### federation: FedBench / W3C federation tests

planned, performance; tasks federation; kit `-` (outside the driver); checked by: answers against the single-store run.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|
| - | · | · | · | · | · | · | · | · | · | · |

### starbench: StarBench: 56 SPARQL-star queries over BKR (61 M statements as RDF-star)

partial, performance; tasks basics; kit `benches/breadth/starbench` (suite driver); checked by: answer counts across systems; GraphDB (the paper's fastest) as the reference.

| Tier | nrese | nrese-fsst | oxigraph | jena | rdf4j | graphdb |
|---|---|---|---|---|---|---|
| bkr | · | · | · | · | · | · |

### geographica2: Geographica 2: GeoSPARQL micro and macro workloads on real data, synthetic and scalability sets

planned, performance; tasks space; kit `benches/breadth/geographica2` (suite driver); checked by: answer counts across systems; a dispute settled by a minimised witness.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | virtuoso | jena | rdf4j | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|
| real | · | · | · | · | · | · | · | · | · |
| census | · | · | · | · | · | · | · | · | · |
| synthetic | · | · | · | · | · | · | · | · | · |
| scalability | · | · | · | · | · | · | · | · | · |

### fedshop: FedShop200: federated BSBM-like shops, 20 to 200 members, 120 queries

planned, performance; tasks federation; kit `benches/breadth/fedshop` (outside the driver); checked by: answers against the reference source assignment's.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|
| 20 | · | · | · | · | · | · | · | · | · | · |
| 100 | · | · | · | · | · | · | · | · | · | · |
| 200 | · | · | · | · | · | · | · | · | · | · |

### vibe: VIBE: recall against throughput on modern embeddings, in and out of distribution

planned, performance; tasks search; kit `benches/breadth/vectors` (outside the driver); checked by: recall@10 against the dataset's exact neighbours.

| Tier | nrese | nrese-fsst | qdrant | milvus | pgvector | lancedb |
|---|---|---|---|---|---|---|
| agnews-mxbai-1024-euclidean | · | · | · | · | · | · |
| arxiv-nomic-768-normalized | · | · | · | · | · | · |

### vector-dbbench: VectorDBBench: load, recall, QPS and p99 of vector databases, with and without filters

planned, performance; tasks search; kit `benches/breadth/vectors` (outside the driver); checked by: recall against the case's ground truth.

| Tier | nrese | nrese-fsst | qdrant | milvus | pgvector | lancedb |
|---|---|---|---|---|---|---|
| performance768d1m | · | · | · | · | · | · |
| performance768d1m1p | · | · | · | · | · | · |
| performance768d1m99p | · | · | · | · | · | · |

### larger-than-memory: LUBM 1000 under memory caps: load, OWL 2 RL closure and the 14 queries at 16, 12, 8, 6, 4 and 2 GB

planned, performance; tasks RT1, RT5; kit `benches/breadth/memory-budget` (outside the driver); checked by: the same closure size and answers as uncapped.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 16g | · | · | · | · | · | · | · | · | · | · | · |
| 12g | · | · | · | · | · | · | · | · | · | · | · |
| 8g | · | · | · | · | · | · | · | · | · | · | · |
| 6g | · | · | · | · | · | · | · | · | · | · | · |
| 4g | · | · | · | · | · | · | · | · | · | · | · |
| 2g | · | · | · | · | · | · | · | · | · | · | · |

### fast: The fast suite's cases on their comparators: each case under the semantics a system supports

partial, performance; tasks basics, RT1, RT3; kit `benches/fast` (suite driver); checked by: the generator's own answers and pinned counts (fast.py run); answer counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| q-cycles~plain | ok 10-10 | · | · | ok 10-10 | part 10-10 | ok 10-10 | part 10-10 | · | · | · | · | · |
| rl-hierarchy~closure | · | · | · | ok 10-06 | ok 10-06 | · | · | · | · | · | · | · |
| rl-clique | · | · | · | · | · | · | · | · | · | · | · | · |
| 6 plain cases (extra) | · | · | · | ok 10-06 | part 10-06 | ok 10-06 | part 10-06 | · | · | · | · | · |
| 84 cases (extra) | ok 10-06 | · | · | · | · | · | · | · | · | · | · | · |
| clients-lookups~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| clients-sweep~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | part 10-10 | ok 10-10 | · | · | · | · | · |
| custom-tc~custom (extra) | · | · | · | · | · | · | · | · | · | · | · | · |
| dl cases (extra) | · | · | · | · | · | · | · | · | · | · | · | · |
| dl-roles, el-classify (extra) | · | · | · | · | · | · | · | · | · | · | · | · |
| el-classify (extra) | · | · | · | · | · | · | · | · | · | · | · | · |
| horst-lubm10~owl-horst (extra) | · | · | · | · | · | · | FAIL 10-06 | · | · | · | · | · |
| index-join-keys~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | · | · | · | · | · | · | · |
| kernel-intersect~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | · | ok 10-10 | · | · | · | · | · |
| native-cli-cache-replay (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · | · |
| native-cli-classify-store-horn (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · | · |
| native-cli-clients-sweep (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · | · |
| native-cli-commits-readers (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · | · |
| native-cli-dlmode-lubm1 (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · | · |
| native-cli-writes-under-readers (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · | · |
| op-dates~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| op-group~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| op-negation~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| op-optional~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| op-strings~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| q-chain~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| q-paths~plain (extra) | ok 10-10 | · | · | part 10-10 | part 10-10 | part 10-10 | part 10-10 | · | · | · | · | · |
| q-sideways~plain (extra) | ok 10-10 | · | · | ok 10-10 | part 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| q-snowflake~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| q-star~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| q-topk~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · | · |
| rdfs-hierarchy~rdfs (extra) | · | · | · | · | · | · | ok 10-06 | · | · | · | · | · |
| rl-clique, rl-chain, rl-owl2bench~owl-horst (extra) | · | · | · | · | · | · | hang 10-06 | · | · | · | · | · |
| rl-clique~owl-horst (extra) | · | · | · | · | · | · | part 10-06 | · | · | · | · | · |
| rule cases~owl2-rl (extra) | · | · | · | · | · | · | · | · | · | · | · | · |

### clients: ResearchSpace on the store; the Datamodel Workflow's export protocol

ready, conformance; tasks integration; kit `scripts` (suite driver); checked by: the scripts' assertions.

| Tier | nrese |
|---|---|
| - | · |

### claros: Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques

partial, performance; tasks RT1, RT2; kit `benches/reasoning/datalog` (suite driver); checked by: facts per relation (each relation is one triple pattern) across the engines; GLog's published totals.

| Tier | nrese | nemo | souffle | vlog | glog |
|---|---|---|---|---|---|
| l | · | · | · | · | · |
| le | · | · | · | · | · |

### sspe: SSPE: single-source path graphs where one deletion invalidates long derivations

planned, performance; tasks RT2; kit `-` (outside the driver); checked by: the incremental closure equals the rematerialised one.

No system can run it yet.

### datalog-graphs: Datalog graph programs: transitive closure, same generation, CSPA

partial, performance; tasks RT1; kit `benches/reasoning/datalog` (suite driver); checked by: derived facts per relation against Soufflé's published counts (FlowLog's reproduction logs); counts across the engines.

| Tier | nrese | nemo | souffle | vlog | glog |
|---|---|---|---|---|---|
| tc-G10K | · | · | · | · | · |
| sg-G10K | · | · | · | · | · |
| cspa-httpd | · | · | · | · | · |
| cspa-linux | · | · | · | · | · |
| cspa-postgresql | · | · | · | · | · |

### commit-latency: Commit latency under 0, 1, 8 and 64 readers, the closure complete at every commit

partial, performance; tasks RT2; kit `benches/reasoning/maintenance` (suite driver); checked by: every change is undone, so the closure must hash back to the one the load materialised.

| Tier | nrese | graphdb |
|---|---|---|
| lubm-10 | · | · |

### elk-ontologies: Classification of large EL and near-EL ontologies: GO, ChEBI, NCIt, GALEN

planned, performance; tasks RT6; kit `benches/reasoning/el-classification` (outside the driver); checked by: taxonomies against ELK's and Konclude's, by canonical hash.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk |
|---|---|---|---|---|---|---|---|
| go | · | · | · | · | · | · | · |
| chebi | · | · | · | · | · | · | · |
| ncit | · | · | · | · | · | · | · |
| galen | · | · | · | · | · | · | · |

### pagoda-suite: PAGOdA's test suite: query answering beyond OWL 2 RL

planned, performance; tasks RT3; kit `-` (outside the driver); checked by: answers against a complete reasoner (HermiT, Konclude).

| Tier | nrese | nrese-fsst | nrese-oxigraph | graphdb |
|---|---|---|---|---|
| lubm | · | · | · | · |
| uobm | · | · | · | · |
| fly | · | · | · | · |
| npd | · | · | · | · |
| dbpedia | · | · | · | · |

### vector-recall: Filtered k-NN: recall and time of vector search under a filter

partial, performance; tasks vector; kit `benches/fast` (outside the driver); checked by: recall@10 against the exact search.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| generated-200k | · | · | · | · | · | · | · | · | · | · | · |
| yfcc-10m | · | · | · | · | · | · | · | · | · | · | · |

### ann-benchmarks: ann-benchmarks: recall and throughput curves of approximate k-NN

partial, performance; tasks vector; kit `benches/breadth/vectors` (outside the driver); checked by: recall@10 against the sets' ground truth.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| sift-128-euclidean | · | · | · | · | · | · | · | · | · | · | · |
| glove-100-angular | · | · | · | · | · | · | · | · | · | · | · |
| fashion-mnist-784-euclidean | · | · | · | · | · | · | · | · | · | · | · |

## Gaps

### Pairs that can run and have no outcome yet

- **lubm:** jena (1000); nemo (1000); owlrl (10, 100, 1000); rdfox (1, 10, 100, 1000)
- **lubm-materialised:** millenniumdb (1, 10, 100); rdfox (1, 10, 100); anzograph (1, 10, 100)
- **owl2bench:** jena (ql-1); rdfox (rl-1, ql-1)
- **ldbc-spb:** nrese (50m, 256m); graphdb (50m, 256m)
- **integration-rg-gs-gnd:** nrese (example, cohort, full); jena (example, cohort, full); graphdb (example, cohort, full); rdfox (example, cohort, full)
- **real-ontologies:** nrese (dbpedia-core, yago-tiny); jena (dbpedia-core, yago-tiny); rdf4j (dbpedia-core, yago-tiny); nemo (dbpedia-core, yago-tiny); owlrl (dbpedia-core, yago-tiny); graphdb (dbpedia-core, yago-tiny); anzograph (dbpedia-core, yago-tiny)
- **consistency:** nrese (lubm-1, owl2bench-rl-1); graphdb (lubm-1, owl2bench-rl-1)
- **ore-2015:** nrese (full); hermit (full); openllet (full); konclude (full); elk (full)
- **owl2bench-classify:** nrese (el-1, dl-1, dl-10); hermit (el-1, dl-1, dl-10); openllet (el-1, dl-1, dl-10); konclude (el-1, dl-1, dl-10); elk (el-1, dl-1, dl-10)
- **basics-mix:** millenniumdb (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); rdfox (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); anzograph (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m)
- **sparqloscope:** nrese (sparqloscope-dblp); qlever (sparqloscope-dblp); oxigraph (sparqloscope-dblp); virtuoso (sparqloscope-dblp); jena (sparqloscope-dblp); millenniumdb (sparqloscope-dblp); rdf4j (sparqloscope-dblp); graphdb (sparqloscope-dblp); rdfox (sparqloscope-dblp); anzograph (sparqloscope-dblp)
- **bsbm:** nrese (bsbm-explore-100m, bsbm-bi-100m); qlever (bsbm-explore-100m, bsbm-bi-100m); oxigraph (bsbm-explore-100m, bsbm-bi-100m); virtuoso (bsbm-explore-100m, bsbm-bi-100m); jena (bsbm-explore-100m, bsbm-bi-100m); millenniumdb (bsbm-explore-100m, bsbm-bi-100m); rdf4j (bsbm-explore-100m, bsbm-bi-100m); graphdb (bsbm-explore-100m, bsbm-bi-100m); rdfox (bsbm-explore-100m, bsbm-bi-100m); anzograph (bsbm-explore-100m, bsbm-bi-100m)
- **watdiv:** nrese (watdiv-10m, watdiv-100m); qlever (watdiv-10m, watdiv-100m); oxigraph (watdiv-10m, watdiv-100m); virtuoso (watdiv-10m, watdiv-100m); jena (watdiv-10m, watdiv-100m); millenniumdb (watdiv-10m, watdiv-100m); rdf4j (watdiv-10m, watdiv-100m); graphdb (watdiv-10m, watdiv-100m); rdfox (watdiv-10m, watdiv-100m); anzograph (watdiv-10m, watdiv-100m)
- **sp2bench:** nrese (sp2bench-1m, sp2bench-25m); qlever (sp2bench-1m, sp2bench-25m); oxigraph (sp2bench-1m, sp2bench-25m); virtuoso (sp2bench-1m, sp2bench-25m); jena (sp2bench-1m, sp2bench-25m); millenniumdb (sp2bench-1m, sp2bench-25m); rdf4j (sp2bench-1m, sp2bench-25m); graphdb (sp2bench-1m, sp2bench-25m); rdfox (sp2bench-1m, sp2bench-25m); anzograph (sp2bench-1m, sp2bench-25m)
- **gcare:** nrese (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); qlever (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); oxigraph (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); virtuoso (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); jena (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); millenniumdb (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); rdf4j (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); graphdb (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); anzograph (gcare-aids, gcare-human, gcare-lubm80, gcare-yago)
- **wdbench:** nrese (wdbench); qlever (wdbench); oxigraph (wdbench); virtuoso (wdbench); jena (wdbench); millenniumdb (wdbench); rdf4j (wdbench); graphdb (wdbench); anzograph (wdbench)
- **write-scaling:** graphdb (1m, 10m)
- **era-shacl:** nrese (ES, FR, ERA); jena (ES, FR, ERA); rdf4j (ES, FR, ERA); graphdb (ES, FR, ERA)
- **fulltext-search:** nrese (-); qlever (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-)
- **federation:** nrese (-); qlever (-); oxigraph (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-); anzograph (-)
- **starbench:** nrese (bkr); oxigraph (bkr); jena (bkr); rdf4j (bkr); graphdb (bkr)
- **geographica2:** nrese (real, census, synthetic, scalability); qlever (real, census, synthetic, scalability); virtuoso (real, census, synthetic, scalability); jena (real, census, synthetic, scalability); rdf4j (real, census, synthetic, scalability); graphdb (real, census, synthetic, scalability); rdfox (real, census, synthetic, scalability)
- **fedshop:** nrese (20, 100, 200); qlever (20, 100, 200); oxigraph (20, 100, 200); virtuoso (20, 100, 200); jena (20, 100, 200); rdf4j (20, 100, 200); graphdb (20, 100, 200); anzograph (20, 100, 200)
- **vibe:** nrese (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); qdrant (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); milvus (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); pgvector (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); lancedb (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized)
- **vector-dbbench:** nrese (performance768d1m, performance768d1m1p, performance768d1m99p); qdrant (performance768d1m, performance768d1m1p, performance768d1m99p); milvus (performance768d1m, performance768d1m1p, performance768d1m99p); pgvector (performance768d1m, performance768d1m1p, performance768d1m99p); lancedb (performance768d1m, performance768d1m1p, performance768d1m99p)
- **larger-than-memory:** nrese (16g, 12g, 8g, 6g, 4g, 2g); qlever (16g, 12g, 8g, 6g, 4g, 2g); oxigraph (16g, 12g, 8g, 6g, 4g, 2g); virtuoso (16g, 12g, 8g, 6g, 4g, 2g); jena (16g, 12g, 8g, 6g, 4g, 2g); millenniumdb (16g, 12g, 8g, 6g, 4g, 2g); rdf4j (16g, 12g, 8g, 6g, 4g, 2g); graphdb (16g, 12g, 8g, 6g, 4g, 2g); anzograph (16g, 12g, 8g, 6g, 4g, 2g)
- **fast:** nrese (rl-hierarchy~closure, rl-clique); qlever (rl-clique); oxigraph (rl-clique); virtuoso (rl-hierarchy~closure, rl-clique); jena (rl-hierarchy~closure, rl-clique); millenniumdb (q-cycles~plain, rl-hierarchy~closure, rl-clique); rdf4j (q-cycles~plain, rl-hierarchy~closure, rl-clique); graphdb (q-cycles~plain, rl-hierarchy~closure, rl-clique); rdfox (q-cycles~plain, rl-hierarchy~closure, rl-clique); anzograph (q-cycles~plain, rl-hierarchy~closure, rl-clique)
- **clients:** nrese (-)
- **claros:** nrese (l, le); nemo (l, le); souffle (l, le); vlog (l, le); glog (l, le)
- **datalog-graphs:** nrese (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); nemo (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); souffle (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); vlog (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); glog (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql)
- **commit-latency:** nrese (lubm-10); graphdb (lubm-10)
- **elk-ontologies:** nrese (go, chebi, ncit, galen); hermit (go, chebi, ncit, galen); openllet (go, chebi, ncit, galen); konclude (go, chebi, ncit, galen); elk (go, chebi, ncit, galen)
- **pagoda-suite:** nrese (lubm, uobm, fly, npd, dbpedia); graphdb (lubm, uobm, fly, npd, dbpedia)
- **vector-recall:** nrese (generated-200k, yfcc-10m); qlever (generated-200k, yfcc-10m); oxigraph (generated-200k, yfcc-10m); virtuoso (generated-200k, yfcc-10m); jena (generated-200k, yfcc-10m); millenniumdb (generated-200k, yfcc-10m); rdf4j (generated-200k, yfcc-10m); graphdb (generated-200k, yfcc-10m); anzograph (generated-200k, yfcc-10m)
- **ann-benchmarks:** nrese (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); qlever (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); oxigraph (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); virtuoso (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); jena (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); millenniumdb (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); rdf4j (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); graphdb (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); anzograph (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean)

### Build variants without an outcome (nrese-fsst, nrese-oxigraph, nemo-schemafirst, nemo-sparq; optional)

- **lubm:** nrese-fsst (1, 10, 100, 1000); nrese-oxigraph (1000); nemo-schemafirst (1000)
- **lubm-materialised:** nrese-fsst (1, 10, 100); nrese-oxigraph (1, 10, 100)
- **owl2bench:** nrese-fsst (rl-1, ql-1); nrese-oxigraph (ql-1); nemo-sparq (rl-1, ql-1)
- **integration-rg-gs-gnd:** nrese-fsst (example, cohort, full); nrese-oxigraph (example, cohort, full)
- **real-ontologies:** nrese-fsst (dbpedia-core, yago-tiny); nrese-oxigraph (dbpedia-core, yago-tiny); nemo-schemafirst (dbpedia-core, yago-tiny); nemo-sparq (dbpedia-core, yago-tiny)
- **consistency:** nrese-fsst (lubm-1, owl2bench-rl-1); nrese-oxigraph (lubm-1, owl2bench-rl-1)
- **ore-2015:** nrese-fsst (dev, full); nrese-oxigraph (dev, full)
- **w3c-owl2-dl:** nrese-fsst (-); nrese-oxigraph (-)
- **owl2bench-classify:** nrese-fsst (el-1, dl-1, dl-10); nrese-oxigraph (el-1, dl-1, dl-10)
- **basics-mix:** nrese-fsst (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); nrese-oxigraph (yago-tiny)
- **sparqloscope:** nrese-fsst (sparqloscope-dblp); nrese-oxigraph (sparqloscope-dblp)
- **bsbm:** nrese-fsst (bsbm-explore-100m, bsbm-bi-100m); nrese-oxigraph (bsbm-explore-100m, bsbm-bi-100m)
- **watdiv:** nrese-fsst (watdiv-10m, watdiv-100m); nrese-oxigraph (watdiv-10m, watdiv-100m)
- **sp2bench:** nrese-fsst (sp2bench-1m, sp2bench-25m); nrese-oxigraph (sp2bench-1m, sp2bench-25m)
- **gcare:** nrese-fsst (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); nrese-oxigraph (gcare-aids, gcare-human, gcare-lubm80, gcare-yago)
- **wdbench:** nrese-fsst (wdbench); nrese-oxigraph (wdbench)
- **era-shacl:** nrese-fsst (ES, FR, ERA); nrese-oxigraph (ES, FR, ERA)
- **fulltext-search:** nrese-fsst (-); nrese-oxigraph (-)
- **federation:** nrese-fsst (-); nrese-oxigraph (-)
- **starbench:** nrese-fsst (bkr)
- **geographica2:** nrese-fsst (real, census, synthetic, scalability); nrese-oxigraph (real, census, synthetic, scalability)
- **fedshop:** nrese-fsst (20, 100, 200); nrese-oxigraph (20, 100, 200)
- **vibe:** nrese-fsst (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized)
- **vector-dbbench:** nrese-fsst (performance768d1m, performance768d1m1p, performance768d1m99p)
- **larger-than-memory:** nrese-fsst (16g, 12g, 8g, 6g, 4g, 2g); nrese-oxigraph (16g, 12g, 8g, 6g, 4g, 2g)
- **fast:** nrese-fsst (q-cycles~plain, rl-hierarchy~closure, rl-clique); nrese-oxigraph (q-cycles~plain, rl-hierarchy~closure, rl-clique)
- **elk-ontologies:** nrese-fsst (go, chebi, ncit, galen); nrese-oxigraph (go, chebi, ncit, galen)
- **pagoda-suite:** nrese-fsst (lubm, uobm, fly, npd, dbpedia); nrese-oxigraph (lubm, uobm, fly, npd, dbpedia)
- **vector-recall:** nrese-fsst (generated-200k, yfcc-10m); nrese-oxigraph (generated-200k, yfcc-10m)
- **ann-benchmarks:** nrese-fsst (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); nrese-oxigraph (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean)

### Problems on the newest run

- basics-mix dbpedia-core on nrese-oxigraph: disputed (2026-10-02): answers differ from other systems': q07-subdivision-path
- basics-mix dbpedia-core on qlever: partial (2026-10-03-office-a): incomplete: q08-label-text (q08-label-text: 3 timeout of 3; 3 timeout of 3)
- basics-mix dbpedia-core on rdf4j: failed (2026-10-03-office-a): server start-up excluded; SPARQL LOAD per file
- basics-mix dbpedia-core on virtuoso: wrong (2026-10-03-office-a): wrong answer counts: q07-subdivision-path
- basics-mix olympics on oxigraph: partial (2026-10-03-office-a): incomplete: q09-no-medal (q09-no-medal: 3 timeout of 3)
- basics-mix olympics on rdf4j: partial (2026-10-03-office-a): incomplete: q09-no-medal (q09-no-medal: 3 timeout of 3)
- basics-mix olympics on virtuoso: wrong (2026-10-03-office-a): wrong answer counts: q12-all-labels; incomplete: q08-event-hierarchy (q08-event-hierarchy: 3 failed of 3)
- basics-mix wikidata-lexemes-60m on rdf4j: failed (2026-10-03-office-a): server start-up excluded; SPARQL LOAD per file
- basics-mix yago-tiny on virtuoso: partial (2026-10-03-office-a): incomplete: q01-label-lookup, q03-taxon-path, q08-subclass-path (q01-label-lookup: 3 failed of 3)
- fast 6 plain cases on jena: partial (2026-10-06-fast-compete): 4-cycles timed out
- fast 6 plain cases on oxigraph: partial (2026-10-06-fast-compete): 4-cycles timed out
- fast clients-sweep~plain on virtuoso: partial (2026-10-10-competitors-dc2114a): incomplete: clients-64 (clients-64: 5 failed of 6)
- fast dl cases on hermit: partial (2026-10-06-fast-suite): timeouts at 120 s on most generated ontologies; parse and classify ok on the others
- fast dl cases on konclude: partial (2026-10-06-fast-suite): an error on dl-number; timeouts on nominals and Horn classification
- fast dl cases on openllet: partial (2026-10-06-fast-suite): timeouts on most
- fast dl-roles, el-classify on hermit: partial (2026-10-06-fast-compete): el-classify timed out
- fast dl-roles, el-classify on openllet: partial (2026-10-06-fast-compete): el-classify timed out
- fast horst-lubm10~owl-horst on jena: failed (2026-10-06-fast-compete): count out of heap; rerun without it tonight
- fast q-cycles~plain on jena: partial (2026-10-10-competitors-dc2114a): incomplete: four-cycles (four-cycles: 3 timeout of 3)
- fast q-cycles~plain on oxigraph: partial (2026-10-10-competitors-dc2114a): incomplete: four-cycles (four-cycles: 3 timeout of 3)
- fast q-paths~plain on jena: partial (2026-10-10-competitors-dc2114a): incomplete: follows-closure-count, follows-reach (follows-closure-count: 3 failed of 3)
- fast q-paths~plain on oxigraph: partial (2026-10-10-competitors-dc2114a): incomplete: follows-closure-count (follows-closure-count: 3 timeout of 3)
- fast q-paths~plain on qlever: partial (2026-10-10-competitors-dc2114a): incomplete: follows-closure-count (follows-closure-count: 3 timeout of 3; 3 timeout of 3)
- fast q-paths~plain on virtuoso: partial (2026-10-10-competitors-dc2114a): incomplete: follows-closure-count, follows-reach (follows-closure-count: 3 failed of 3)
- fast q-sideways~plain on oxigraph: partial (2026-10-10-competitors-dc2114a): incomplete: exists (exists: 3 timeout of 3)
- fast rl-clique~owl-horst on jena: partial (2026-10-06-fast-suite)
- fast rule cases~owl2-rl on nemo: partial (2026-10-06-fast-compete): answers equal NRESE's where it finished
- geosparql compliance on nrese: partial (2026-10-09-office-conformance-a633): 18 disputed reference differences
- lubm 10 on jena: partial (2026-10-03-office-b): incomplete: q01, q02, q03, q04, q05, q06, q07, q08, q09, q10, q11, q12, q13, q14 (q01: 3 timeout of 3); steps: count failed
- lubm 100 on jena: partial (2026-10-03-office-b): incomplete: q01, q02, q03, q04, q05, q06, q07, q08, q09, q10, q11, q12, q13, q14 (q01: 1 timeout of 1); steps: count failed
- lubm 1000 on nemo-sparq: failed (2026-10-03-office-b): encoding sparq/owl-rl.rls sha256 9012609fe35f25b7; load time includes concatenating the inputs
- ore-2015 dev on konclude: partial (2026-10-03-dl-reference): 1 error
- ore-2015 dev on nrese: partial (2026-10-03-dl-reference): EL track only (20/20 agree); the DL tasks wait for phase 3
- ore-2015 dev on openllet: partial (2026-10-03-dl-reference): 3 timeouts; differs on ore_ont_1340 (open)
- owl2bench rl-1 on owlrl: timeout (2026-10-03-office-b)
- w3c-owl2-dl - on elk: partial (2026-10-03-dl-reference): EL tasks only; the rest use constructs outside its fragment
- w3c-owl2-dl - on hermit: partial (2026-10-03-dl-reference): 27 timeouts, 1 crash (WebOnt-Thing-003), no wrong result
- w3c-owl2-dl - on konclude: wrong (2026-10-03-dl-reference): 30 wrong results, 100 unsupported (datatypes, keys)
- w3c-owl2-dl - on nrese: partial (2026-10-09-office-conformance-a633): DL-909 disputed
- w3c-owl2-dl - on openllet: wrong (2026-10-03-dl-reference): 6 wrong results, 10 timeouts
- w3c-owl2-rl - on nrese: partial (2026-10-09-office-conformance-a633): two import skips, one functional-only premise skip

### Workloads not ready

- **ldbc-spb** (partial): LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates. The industry's audited reasoning benchmark (GraphDB publishes audited results). Its advanced query mix also needs fulltext and geosparql; the kit runs the basic mix with the benchmark's own ruleset on both systems. Office-PC's GraphDB licence is Free (1 CPU core, two concurrent queries; checked 12 Oct 2026): the comparison is the matched one-CPU scenario, and the audited Enterprise results can't be reproduced with it
- **integration-rg-gs-gnd** (partial): RG x GS x GND integration: identity reasoning over real links, 10 competency questions. NRESE and Fuseki run; cohort and full tiers and the other systems are open
- **real-ontologies** (planned): DBpedia and YAGO with their ontologies: materialisation over real hierarchies.
- **consistency** (planned): Injected contradictions: detection and explanation.
- **ore-2015** (partial): ORE 2015: classification of 1,920 real ontologies. dev: the development subset (80 tasks: 40 DL and 20 EL classifications, 20 DL consistency checks, up to 5 MB; ore.py subset); full: all 1,920 ontologies x 3 tasks (Draco). NRESE classifies the EL track (benches/reasoning/dl/nrese.py el); its DL engines come with phase 3
- **owl2bench-classify** (planned): OWL2Bench EL and DL: classification and consistency of the generated ontologies. Konclude times out on the DL variants at 1 and 10 (SemREC 2021): opening 4 of docs/design/owl2-dl-performance.md
- **watdiv** (partial): WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test. Oxigraph's and Jena's home turf (owner, 12 Oct 2026), with BSBM and SP2Bench; the queries are instantiated from the generator's saved state of each dataset
- **gcare** (planned): G-CARE: cardinality estimation for subgraph queries (SIGMOD 2020). The planner's estimates on the cardinality-estimation benchmark; G-CARE's estimators (characteristic sets, SumRDF, BoundSketch, WanderJoin, JSUB, ...) are the comparators
- **wdbench** (planned): WDBench: about 2,650 real queries from Wikidata's logs (BGPs, OPTIONAL, paths, C2RPQs). MillenniumDB's home turf; larger than office-pc and phuoc-yu hold for the competitors: khaleesi S: or Draco
- **era-shacl** (partial): ERA-SHACL-Benchmark: validation time and memory on a real railway knowledge graph. Engines in memory, each in its own container (upstream's CLI contract); the published numbers per engine are in the repository's results/. The era shape set uses SHACL-SPARQL
- **fulltext-search** (planned): Keyword search: ResearchSpace's search queries; LUBMft.
- **geosparql** (partial): GeoSPARQL compliance benchmark (206 queries); Geographica 2 for performance. The compliance part runs on NRESE in CI: 175 of 212; the 37 others are reference answers that contradict GeoSPARQL's definitions or each other (expected-failures.txt). Geographica 2 (performance) is to add
- **federation** (planned): FedBench / W3C federation tests.
- **starbench** (partial): StarBench: 56 SPARQL-star queries over BKR (61 M statements as RDF-star). GraphDB and Stardog load the RDF-star file; the RDF 1.2 systems one reifier per distinct quoted triple (rdfstar_to_rdf12.py), which keeps the answers. Published: Jena 4.7.0, Oxigraph 0.3.16, GraphDB 10.0.2 and a commercial 'Engine X' (Stardog 8.1, by the repository)
- **geographica2** (planned): Geographica 2: GeoSPARQL micro and macro workloads on real data, synthetic and scalability sets. Published: Strabon, uSeekM, Parliament, GraphDB 8.6.1, RDF4J 2.4.3 and a proprietary System X; Jena GeoSPARQL and QLever added here
- **fedshop** (planned): FedShop200: federated BSBM-like shops, 20 to 200 members, 120 queries. NRESE runs only the SERVICE-annotated reference source assignments: it has no source selection over a federation (a finding). FedX, CostFed, Semagrow, HeFQUIN, FedUP published
- **vibe** (planned): VIBE: recall against throughput on modern embeddings, in and out of distribution. Algorithm level, one core, under Apptainer; the database systems aren't VIBE methods (their own ground is vector-dbbench)
- **vector-dbbench** (planned): VectorDBBench: load, recall, QPS and p99 of vector databases, with and without filters. The database vendors' own ground: Qdrant, Milvus, pgvector and LanceDB have clients there; NRESE needs one (SPARQL with nrv:search)
- **larger-than-memory** (planned): LUBM 1000 under memory caps: load, OWL 2 RL closure and the 14 queries at 16, 12, 8, 6, 4 and 2 GB. Systems with a memory budget of their own degrade (NRESE's budgets, QLever's sort memory, JVM heaps); the others are killed at the cap: failed, never a time
- **fast** (partial): The fast suite's cases on their comparators: each case under the semantics a system supports. Tier = CASE~SEMANTICS (benches/fast/compete.py prepares the data in the volume nrese-fast-data). NRESE runs every case in benches/fast/fast.py; this workload is the comparison
- **claros** (partial): Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques. The worst case for proof-based deletion; GLog's single-thread result (Claros-L 119 s against RDFox's 2,512 s, reported). The kit runs NRESE (the program as user rules), Soufflé, Nemo, VLog and GLog; RDFox waits for a licence. LE's closure may not fit Office-PC's 31 GiB on every engine
- **sspe** (planned): SSPE: single-source path graphs where one deletion invalidates long derivations. Counting beat B/F 10.5 s to 253 s for 1,000 deletions (reported); the fast case deletes-layered is its small cousin. Blocked as published on both sides: its rules add path lengths (`z := z1 + z2`) over a ternary relation, and NRESE's user rules have neither arithmetic nor relations beyond triples; RDFox has no licence here
- **datalog-graphs** (partial): Datalog graph programs: transitive closure, same generation, CSPA. The batch executor beyond OWL: plain Datalog as N3 rules; Nemo, VLog and Soufflé on their home turf. Extra tiers: tc-G5K (the qualifier), tc-G20K (400 M facts), CSDA, reachability on LiveJournal, Galen (ternary relations: not expressible on NRESE)
- **commit-latency** (partial): Commit latency under 0, 1, 8 and 64 readers, the closure complete at every commit. Over the SPARQL protocol on every system (the suite's adapters): batches of 1, 100 and 10,000 inserts and deletes, a schema axiom removed and restored, single-triple commits under 0, 1, 8 and 64 readers. GraphDB Free serves two queries at once, so more readers measure its queueing; RDFox waits for a licence. The fast cases commits-readers and writes-under-readers cover NRESE in process
- **elk-ontologies** (planned): Classification of large EL and near-EL ontologies: GO, ChEBI, NCIt, GALEN. ELK's home turf: EL classification speed
- **pagoda-suite** (planned): PAGOdA's test suite: query answering beyond OWL 2 RL. Query answering by lower and upper bounds, as NRESE's DL mode answers
- **vector-recall** (partial): Filtered k-NN: recall and time of vector search under a filter. The fast cases vector-knn-exact and vector-knn-filtered; why the filtered recall failed: runs/2026-10-12-vector-recall-phuoc.toml, and the probe-first route's beam not widened for the filter (fixed 12 Oct: recall 0.98, runs/2026-10-12-vector-filtered-fix-phuoc.toml)
- **ann-benchmarks** (partial): ann-benchmarks: recall and throughput curves of approximate k-NN. Comparability with every published index. NRESE as a module in upstream's harness (its server in the container, SPARQL per query); Qdrant 1.19.2 and pgvector v0.8.7 pinned (upstream pins Qdrant 1.5.1 and builds pgvector's master); hnswlib as the library reference

### Capabilities NRESE lacks

- rules-nary: not planned: user rules are over triples (Galen, Doop, SSPE need ternary and wider relations); owner's scope decision
- rules-negation: not planned: no negation in user rules (Doop's stratified negation); owner's scope decision
- rules-arithmetic: not planned: math builtins are B2's rest (SSPE adds path lengths); owner's scope decision

## Runs

Newest first; the records are in [runs/](runs/).

| Run | Started | Kit | Host | Commit | Pairs | Purpose |
|---|---|---|---|---|---|---|
| [2026-10-12-vector-filtered-fix-phuoc](runs/2026-10-12-vector-filtered-fix-phuoc.toml) | 2026-10-12T15:02 | benches/breadth/vectors/knn_filtered.py through queue.py (vector-knn-filtered's queries in variants) | phuoc-yu | bfc2b2f990 | 0 | vector-knn-filtered after the probe-first route widens its beam by the joined pattern's share (99026108): recall against the 0.9 gate, and its time against the exact scan |
| [2026-10-12-vector-recall-phuoc](runs/2026-10-12-vector-recall-phuoc.toml) | 2026-10-12T14:00 | fast (vector-knn-filtered's queries in variants; tmp diagnosis script through queue.py) | phuoc-yu | aabd47a732 | 0 | Why vector-knn-filtered's approximate recall fails its 0.9 gate: the graph's own recall, the filter's share, the search beam |
| [2026-10-12-owl2bench-ql1-recheck](runs/2026-10-12-owl2bench-ql1-recheck.toml) | 2026-10-12T13:48 | suite | office-pc | 16c3bfb2 ( | 2 | Whether OWL2Bench QL-1, WRONG in office-b (4 Oct: q01 1,427 against the adjudicated 5,104), is still wrong: a possible v1.5 blocker. Counts only (one run, one query run) |
| [2026-10-12-pr23-acceptance-ab-phuoc](runs/2026-10-12-pr23-acceptance-ab-phuoc.toml) | 2026-10-12T10:51 | fast | phuoc-yu | aabd47a732 | 0 | PR #23's acceptance: the whole fast suite on base 157931a and the consolidated takeover/pr23 (aabd47a), no regression in time or space allowed |
| [2026-10-11-fast-inputs-office](runs/2026-10-11-fast-inputs-office.toml) | 2026-10-11T23:40 | fast | office-pc | 133ac6d | 0 | Why nine fast cases failed their checks on the base and the PR alike in the 11 October A/B on office-pc, and the harness fix |
| [2026-10-11-answer-replay-dc2114a](runs/2026-10-11-answer-replay-dc2114a.toml) | 2026-10-11 | suite (answers.py) | office-pc | 133ac6d | 0 | The 10 October competitor answer captures reclassified with the committed recorder: each system against one reference, strict and value matches apart |
| [2026-10-11-horn-recovery](runs/2026-10-11-horn-recovery.toml) | 2026-10-11 | reasoning/dl and fast classify-store-horn | phuoc-yu | dc2114a0f0 | 0 | Horn context accounting by sampled local capacity instead of per-message shared reservations (candidate K), against the original base |
| [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) | 2026-10-10T18:25 | suite | office-pc | dc2114a0f0 | 77 | The free SPARQL stores beside NRESE on 16 fast cases (the plain query and client cases), each answer captured and checked before its time: the first data of the standings, on dc2114a (before PR #23's last changes) |
| [2026-10-10-cache-replay-profile-77478d1](runs/2026-10-10-cache-replay-profile-77478d1.toml) | 2026-10-10 | perf lab | office-pc | 77478d195c | 0 | Where the cache-on replay spends its CPU, to attribute its regression |
| [2026-10-10-checkpoint-bebada2](runs/2026-10-10-checkpoint-bebada2.toml) | 2026-10-10 | fast | office-pc, phuoc-yu | bebada2e66 | 0 | The frozen v2 checkpoint against the preceding revision: all fast cases on both hosts, then a ten-pair follow-up of the regressed cases |
| [2026-10-10-default-execution-recovery](runs/2026-10-10-default-execution-recovery.toml) | 2026-10-10 | fast | phuoc-yu, office-pc | 62ae2e0fae | 0 | Default execution back on the caller (no physical pool unless configured): recovery against the original baseline |
| [2026-10-10-vector-adjacency-reuse](runs/2026-10-10-vector-adjacency-reuse.toml) | 2026-10-10 | perf lab | phuoc-yu | 62ae2e0fae | 0 | HNSW traversal reuses one adjacency buffer per search routine instead of cloning each node's list |
| [2026-10-10-vector-collector-repair](runs/2026-10-10-vector-collector-repair.toml) | 2026-10-10 | perf lab | phuoc-yu | 62ae2e0fae | 0 | The SPARQL vector collector merges sorted rounds, keeps only the winners' probe rows and releases the displaced rows' charges |
| [2026-10-09-dl-reuse-be5c3d7](runs/2026-10-09-dl-reuse-be5c3d7.toml) | 2026-10-09T21:28 | reasoning/dl | office-pc | be5c3d7ea9 | 0 | Bounded Base reuse in DL classification and realisation, with one deadline and cancellation owner |
| [2026-10-09-office-conformance-a633](runs/2026-10-09-office-conformance-a633.toml) | 2026-10-09T17:25 | conformance | office-pc | 6f22e85da5 | 5 | The corpus-backed conformance tests on the repaired sources |
| [2026-10-09-phuoc-partial-77478d1](runs/2026-10-09-phuoc-partial-77478d1.toml) | 2026-10-09T15:58 | fast, suite and perf lab | phuoc-yu | 77478d195c | 10 | The runtime and reuse revision against the preceding one on selected fast cases, client and cache workloads, and native suite cycles |
| [2026-10-09-initial-runtime-c888a20](runs/2026-10-09-initial-runtime-c888a20.toml) | 2026-10-09T14:40 | perf lab | office-pc, phuoc-yu | c888a203ea | 4 | The shared physical worker pool and plan inspection against the preceding revision: LUBM 1 queries, a short client sweep, Horn classification |
| [2026-10-09-graphdb-coverage](runs/2026-10-09-graphdb-coverage.toml) | 2026-10-09 | competitors | office-pc | ? | 0 | Which registered workloads GraphDB Free can be compared on, and what each still lacks |
| [2026-10-10-cache-contracts-d31010c](runs/2026-10-10-cache-contracts-d31010c.toml) | 2026-10-09 | probe | office-pc | d31010c95e | 0 | QL compiler-input identity by the exact schema; complete-only caching of explicit DL classification and realisation |
| [2026-10-10-dl-alias-da4465d](runs/2026-10-10-dl-alias-da4465d.toml) | 2026-10-09 | reasoning/dl | office-pc | da4465d245 | 0 | Retained DL probes apply assumptions to an individual's current representative: the cost of the correctness fix |
| [2026-10-06-fast-compete](runs/2026-10-06-fast-compete.toml) | 2026-10-06 | fast | Kraris-GPTler | 96a679d (N | 13 | The fast suite's comparisons after Phase 2: Nemo and Jena's rules rerun, the plain SPARQL cases, the printer's rewritten queries (counts), the QL and DL-mode cases, dl-roles' reference, el-classify regenerated |
| [2026-10-06-fast-suite](runs/2026-10-06-fast-suite.toml) | 2026-10-06 | fast | Kraris-GPTler | 8233ea5 (f | 9 | Phase 2 of the benchmark work: the fast suite's first baselines (NRESE on every case) and its first comparisons with the systems each case declares |
| [2026-10-03-office-b](runs/2026-10-03-office-b.toml) | 2026-10-04T02:28 | suite | sus-ai | fb8c0cbb76 | 33 | The reasoning batch on the office PC: OWL 2 RL / QL / OWL-Horst materialisation on LUBM 1-1000 and OWL2Bench RL-1 and QL-1, against Nemo (three encodings), owlrl, Jena and a licensed system whose outcomes stay local. |
| [2026-10-03-office-a](runs/2026-10-03-office-a.toml) | 2026-10-03T17:19 | suite | sus-ai | 5ccf8dbb2e | 62 | The first comparison batch on the office PC, the benchmark machine since 3 October: query engines without reasoning (basics-mix on five datasets, materialised LUBM 1-100), and NRESE's W3C conformance, write scaling and GeoSPARQL. |
| [2026-10-03-full](runs/2026-10-03-full.toml) | 2026-10-03T15:34 | suite | Kraris-GPTler | 5ccf8dbb2e | 12 | Full comparison on the main PC at 5ccf8db; interrupted after 35 min (olympics complete, YAGO tiny begun) and moved to the office PC (2026-10-03-office-a and -b), which runs independently while the main PC is used for development |
| [2026-10-03-nrese-current](runs/2026-10-03-nrese-current.toml) | 2026-10-03T12:20 | suite | Kraris-GPTler | b963708 +  | 17 | Regression check before the milestone: NRESE at the head of 3 October on the standard tiers, plus OWL2Bench QL-1 and write scaling 1m |
| [2026-10-03-dl-reference](runs/2026-10-03-dl-reference.toml) | 2026-10-03 | reasoning/dl | Kraris-GPTler | 00f9b19 (kit); c0b5e5c (NRESE EL rerun) | 9 | Work package 2.3: the reference reasoners on the W3C OWL 2 DL suite and the ORE 2015 development subset; NRESE's EL classifier on the EL track |
| [2026-10-02-yago-head](runs/2026-10-02-yago-head.toml) | 2026-10-02T08:44 | suite | Kraris-GPTler | unknown (before run manifests) | 1 | NRESE on YAGO tiny after the fixes of 2 October |
| [2026-10-02-yago](runs/2026-10-02-yago.toml) | 2026-10-02T06:58 | suite | Kraris-GPTler | unknown (before run manifests) | 6 | YAGO tiny on every system (its data was missing in 2026-10-02) |
| [2026-10-02-lubm-materialised](runs/2026-10-02-lubm-materialised.toml) | 2026-10-02T06:11 | suite | Kraris-GPTler | unknown (before run manifests) | 18 | LUBM with the closure precomputed: the query engines without a reasoner on their own turf |
| [2026-10-02-nrese-current](runs/2026-10-02-nrese-current.toml) | 2026-10-02T06:03 | suite | Kraris-GPTler | unknown (before run manifests) | 9 | NRESE at the head of 2 October on the standard tiers (one run): the baseline for the regression checks |
| [2026-10-02](runs/2026-10-02.toml) | 2026-10-02T00:06 | suite | Kraris-GPTler | unknown (before run manifests) | 48 | The comparison batch: the basics mix on five datasets, LUBM 1/10/100 and OWL2Bench RL-1 on every system with an adapter (YAGO tiny's data was missing: rerun in 2026-10-02-yago) |
| [2026-10-01](runs/2026-10-01.toml) | 2026-10-01T19:44 | suite | Kraris-GPTler | unknown (before run manifests) | 15 | First full suite run: LUBM, OWL2Bench RL and the olympics mix on every system with an adapter; NRESE on Oxigraph as the pre-migration baseline |
| [2026-10-12-horn-takeover-dl](runs/2026-10-12-horn-takeover-dl.toml) |  | reasoning/dl (dl_classify, context_classify) | Phuoc-Yu | 8b2dee9 | 0 | Horn saturation after consolidating the context engine's budget checks and capacity observations (review tmp/pr23-review/dl-reasoner.md section 2) |
| [2026-10-12-sparql-consolidation](runs/2026-10-12-sparql-consolidation.toml) |  | fast | Office-PC | e9eb4ee | 0 | PR #23 SPARQL/exec consolidation: the base comparison of the cases the 11 Oct A/B flagged (open-first-query +12 %, kernel-intersect and op-negation memory), and whether the generic evaluate path through SolutionTable caused them |
