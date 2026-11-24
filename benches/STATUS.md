# Benchmark status

Generated 2026-11-21 by `python benches/suite/suite.py status --write` from [suite/systems.toml](suite/systems.toml), [suite/workloads.toml](suite/workloads.toml) and the run records in [runs/](runs/README.md). Don't edit it by hand: change those and regenerate. How to read and extend it: [README.md](README.md).

## At a glance

- **Workloads:** 42 (13 partial, 15 planned, 14 ready)
- **Systems:** 33 (28 free to publish, 5 only with the vendor's permission)
- **Pairs** (standard tier × system that can run it; build variants apart): 619; run at least once: 0; ok on their newest run: 0; run, outcome kept locally (`ran*`): 0
- **NRESE:** 0 of its 108 standard tiers ok on their newest run
- **Run records:** 0, newest -
- **Before the reset:** 0 of the 0 records, and 0 of the 0 newest outcomes: history, not the current engine's (marked †; the v1.6 baseline measures the current engine)

## Schedule

The order the families are measured in ([suite/schedule.toml](suite/schedule.toml), the owner's of 12 Oct 2026). A family without a workload entry yet is a benchmark of [CATALOG.md](CATALOG.md), named by its section.

### This week, in this order

| # | Family | State | Newest run | What |
|---|---|---|---|---|
| 1 | sparqloscope | ready | - | Sparqloscope: 105 generated queries covering the SPARQL 1.1 features one by one |
| 2 | bsbm | ready | - | Berlin SPARQL Benchmark V3.1: e-commerce explore and business intelligence mixes, 1 to 64 clients |
| 3 | watdiv | partial | - | WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test |
| 4 | gcare | planned | - | G-CARE: cardinality estimation for subgraph queries (SIGMOD 2020) |
| 5 | owl2bench | ready | - | OWL2Bench: one TBox per OWL 2 profile, 22 queries |
| 6 | lubm | ready | - | LUBM: universities, 14 queries |
| 7 | claros | partial | - | Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques |
| 8 | ldbc-spb | partial | - | LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates |
| 9 | datalog-graphs | partial | - | Datalog graph programs: transitive closure, same generation, CSPA |
| 10 | commit-latency | partial | - | Commit latency under 0, 1, 8 and 64 readers, the closure complete at every commit |
| 11 | ore-2015 | partial | - | ORE 2015: classification of 1,920 real ontologies |
| 12 | elk-ontologies | planned | - | Classification of large EL and near-EL ontologies: GO, ChEBI, NCIt, GALEN |
| 13 | pagoda-suite | planned | - | PAGOdA's test suite: query answering beyond OWL 2 RL |
| 14 | vector-recall | partial | - | Filtered k-NN: recall and time of vector search under a filter |
| 15 | ann-benchmarks | partial | - | ann-benchmarks: recall and throughput curves of approximate k-NN |
| 16 | geographica2 | planned | - | Geographica 2: GeoSPARQL micro and macro workloads on real data, synthetic and scalability sets |
| 17 | era-shacl | partial | - | ERA-SHACL-Benchmark: validation time and memory on a real railway knowledge graph |
| 18 | fast | partial | - | The fast suite's cases on their comparators: each case under the semantics a system supports |

### Backlog

| # | Family | State | Newest run | What |
|---|---|---|---|---|
| 1 | lubm-materialised | ready | - | LUBM with the closure precomputed: the 14 queries on every SPARQL engine |
| 2 | owl2bench-classify | planned | - | OWL2Bench EL and DL: classification and consistency of the generated ontologies |
| 3 | real-ontologies | planned | - | DBpedia and YAGO with their ontologies: materialisation over real hierarchies |
| 4 | consistency | planned | - | Injected contradictions: detection and explanation |
| 5 | integration-rg-gs-gnd | partial | - | RG x GS x GND integration: identity reasoning over real links, 10 competency questions |
| 6 | fulltext-search | planned | - | Keyword search: ResearchSpace's search queries; LUBMft |
| 7 | federation | planned | - | FedBench / W3C federation tests |
| 8 | write-scaling | ready | - | Single-statement and batch writes at 1 M, 10 M, 100 M; writes under read load |
| 9 | basics-mix | ready | - | Load, store size, restart and a query mix on real datasets |
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
| 27 | support-stress | ready | - | Restricted readers' inferred visibility under many alternative supports |

### Last: not expected to give data for polishing or optimisation

| # | Family | State | Newest run | What |
|---|---|---|---|---|
| 1 | w3c-owl2-rl | ready | - | W3C OWL 2 test cases, RL profile: entailment and consistency |
| 2 | w3c-owl2-dl | ready | - | W3C OWL 2 test cases, species DL, direct semantics: consistency, entailment, non-entailment |
| 3 | w3c-sparql11 | ready | - | W3C SPARQL 1.1 test suite: query, update, syntax, result formats |
| 4 | w3c-shacl | ready | - | W3C SHACL test suite |
| 5 | geosparql | partial | - | GeoSPARQL compliance benchmark (206 queries); Geographica 2 for performance |
| 6 | clients | ready | - | ResearchSpace on the store; a data-modelling application's export protocol |

## Coverage

Per workload and system: standard tiers with an `ok` (or `ran*`) newest outcome / standard tiers. A `!` marks a newest outcome that is `FAIL`, `T/O`, `WRONG` or `part` on some tier.

| Workload | State | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | nemo | nemo-schemafirst | nemo-sparq | souffle | vlog | glog | owlrl | hermit | openllet | konclude | elk | graphdb | rdfox | allegrograph | stardog | anzograph | qdrant | milvus | pgvector | lancedb | topbraid | pyshacl | maplib | corese |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| lubm | ready | · | · | · | – | – | – | · | – | – | · | · | · | – | – | – | · | – | – | – | – | · | · | – | n/a | n/a | – | – | – | – | – | – | – | – |
| lubm-materialised | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| owl2bench | ready | · | · | · | – | – | – | · | – | – | · | · | · | – | – | – | · | – | – | – | – | · | · | – | n/a | n/a | – | – | – | – | – | – | – | – |
| ldbc-spb | partial | · | n/a | n/a | – | – | – | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| integration-rg-gs-gnd | partial | · | · | · | – | – | – | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | – | n/a | n/a | – | – | – | – | – | – | – | – |
| real-ontologies | planned | · | · | · | – | – | – | · | – | · | · | · | · | – | – | – | · | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| consistency | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| w3c-owl2-rl | ready | · | n/a | n/a | – | – | – | – | – | – | n/a | n/a | n/a | – | – | – | n/a | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| ore-2015 | partial | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| w3c-owl2-dl | ready | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| owl2bench-classify | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| basics-mix | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| sparqloscope | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| bsbm | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| watdiv | partial | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| sp2bench | ready | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| gcare | planned | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| wdbench | planned | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| write-scaling | ready | · | n/a | n/a | n/a | n/a | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| w3c-sparql11 | ready | · | n/a | n/a | n/a | n/a | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| w3c-shacl | ready | · | n/a | n/a | – | – | – | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | n/a | n/a | n/a | n/a |
| era-shacl | partial | · | · | · | – | – | – | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| fulltext-search | planned | · | · | · | · | – | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | – | – | – | – | – | – | – | – | – |
| geosparql | partial | · | n/a | n/a | n/a | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – | – |
| federation | planned | · | · | · | · | · | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | · | – | – | – | – | – | – | – | – |
| starbench | partial | · | · | – | – | · | – | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | – | – | n/a | – | – | – | – | – | – | – | – | – |
| geographica2 | planned | · | · | · | · | – | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | n/a | – | – | – | – | – | – | – | – | – |
| fedshop | planned | · | · | · | · | · | · | · | – | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | · | – | – | – | – | – | – | – | – |
| vibe | planned | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | – | – | – |
| vector-dbbench | planned | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | – | – | – |
| larger-than-memory | planned | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| fast | partial | · | · | · | · | · | · | · | · | · | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | · | · | n/a | n/a | · | – | – | – | – | – | – | – | – |
| clients | ready | · | n/a | n/a | n/a | n/a | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – |
| claros | partial | · | n/a | n/a | – | – | – | n/a | – | – | · | n/a | n/a | · | · | · | n/a | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| sspe | planned | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – |
| datalog-graphs | partial | · | n/a | – | – | – | – | n/a | – | – | · | n/a | n/a | · | · | · | – | – | – | – | – | n/a | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| commit-latency | partial | · | n/a | n/a | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | – | – | – | – | – | – | – | – | – | – |
| support-stress | ready | · | n/a | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – |
| elk-ontologies | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | · | · | – | n/a | – | – | – | – | – | – | – | – | – | – | – |
| pagoda-suite | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | – | · | n/a | – | n/a | – | – | – | – | – | – | – | – | – |
| vector-recall | partial | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |
| ann-benchmarks | partial | · | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | · | – | – | – | – | – | – | – | – |

`ok` every repetition loaded, every step and query execution ok · `part` a load, step or query execution failed or timed out in some repetition · `WRONG` an answer count differs from the expected one · `FAIL`/`T/O` the load failed or timed out · `skip` the suite skipped it (reason in the record) · `≠?` its answers differ from another system's, not yet adjudicated · `hang` a comparator gave no answer in a past run · `ran*` ran; its outcome stays local (a licensed system's, or results kept locally) · `·` can run, never run · `–` lacks a capability · `n/a` no adapter or kit for it · † before the reset of 25 Oct 2026 (engine-v2 re-based on 59e5e24c): history, not the current engine's result (every outcome of the cell)

## Workloads, tier by tier

### lubm: LUBM: universities, 14 queries

ready, performance; tasks RT1, RT3, RT2, RT5; kit `benches/reasoning` (suite driver); checked by: inferred set against owlrl or Nemo; the published answers for LUBM(1).

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | · | · | · | · | · | · | · | · | · | · |
| 10 | · | · | · | · | · | · | · | · | · | · |
| 100 | · | · | · | · | · | · | · | · | · | · |
| 1000 | · | · | · | · | · | · | · | · | · | · |

### lubm-materialised: LUBM with the closure precomputed: the 14 queries on every SPARQL engine

ready, performance; tasks RT5; kit `benches/reasoning` (suite driver); checked by: row counts across systems; the published answers for LUBM(1).

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | millenniumdb | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | · | · | · | · | · | · | · | · | · | · | · | · |
| 10 | · | · | · | · | · | · | · | · | · | · | · | · |
| 100 | · | · | · | · | · | · | · | · | · | · | · | · |

### owl2bench: OWL2Bench: one TBox per OWL 2 profile, 22 queries

ready, performance; tasks RT1, RT3; kit `benches/reasoning` (suite driver); checked by: inferred set against owlrl or Nemo; answer counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|---|
| rl-1 | · | · | · | · | · | · | · | · | · | · |
| ql-1 | · | · | · | · | · | · | · | · | · | · |

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
| - | · |

### ore-2015: ORE 2015: classification of 1,920 real ontologies

partial, performance; tasks RT6; kit `benches/reasoning/dl` (outside the driver); checked by: canonical taxonomies compared by hash across the reference reasoners; differences settled by hand.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk |
|---|---|---|---|---|---|---|---|
| dev | · | · | · | · | · | · | · |
| full | · | · | · | · | · | · | · |

### w3c-owl2-dl: W3C OWL 2 test cases, species DL, direct semantics: consistency, entailment, non-entailment

ready, conformance; tasks RT4, RT6; kit `benches/reasoning/dl` (outside the driver); checked by: the test types; the working group's published results of 2009 (published-2009.tsv); differences settled by hand.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk |
|---|---|---|---|---|---|---|---|
| - | · | · | · | · | · | · | · |

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
| olympics | · | · | · | · | · | · | · | · | · | · | · | · |
| yago-tiny | · | · | · | · | · | · | · | · | · | · | · | · |
| entities-10000000 | · | · | · | · | · | · | · | · | · | · | · | · |
| dbpedia-core | · | · | · | · | · | · | · | · | · | · | · | · |
| wikidata-lexemes-60m | · | · | · | · | · | · | · | · | · | · | · | · |

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
| 1m | · | · |
| 10m | · | · |

### w3c-sparql11: W3C SPARQL 1.1 test suite: query, update, syntax, result formats

ready, conformance; tasks basics; kit `crates/nrese-sparql/tests/w3c_sparql11` (suite driver); checked by: the suite's expected results.

| Tier | nrese |
|---|---|
| - | · |

### w3c-shacl: W3C SHACL test suite

ready, conformance; tasks validation; kit `crates/nrese-shacl/tests` (suite driver); checked by: the suite's expected reports.

| Tier | nrese |
|---|---|
| - | · |

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
| compliance | · |

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

planned, performance; tasks federation; kit `none yet` (outside the driver); checked by: answers against the reference source assignment's.

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

planned, performance; tasks RT1, RT5; kit `none yet` (outside the driver); checked by: the same closure size and answers as uncapped.

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
| q-cycles~plain | · | · | · | · | · | · | · | · | · | · | · | · |
| rl-hierarchy~closure | · | · | · | · | · | · | · | · | · | · | · | · |
| rl-clique | · | · | · | · | · | · | · | · | · | · | · | · |

### clients: ResearchSpace on the store; a data-modelling application's export protocol

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

### support-stress: Restricted readers' inferred visibility under many alternative supports

ready, performance; tasks RT3, RT2; kit `benches/fast` (suite driver); checked by: a reader's answer counts against the generator's oracle: the closure of the statements it may read, computed by brute force (spec: docs/STATUS.md, Inferences and graph access; nrese_store::support).

| Tier | nrese |
|---|---|
| copies-8 | · |
| copies-16 | · |
| copies-17 | · |
| copies-256 | · |
| product-64 | · |
| product-256 | · |
| product-1024 | · |
| product-4096 | · |
| ladder-16 | · |
| ladder-256 | · |
| ladder-4096 | · |

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

- **lubm:** nrese (1, 10, 100, 1000); jena (1, 10, 100, 1000); nemo (1, 10, 100, 1000); owlrl (1, 10, 100, 1000); graphdb (1, 10, 100, 1000); rdfox (1, 10, 100, 1000)
- **lubm-materialised:** nrese (1, 10, 100); qlever (1, 10, 100); oxigraph (1, 10, 100); virtuoso (1, 10, 100); jena (1, 10, 100); millenniumdb (1, 10, 100); rdf4j (1, 10, 100); graphdb (1, 10, 100); rdfox (1, 10, 100); anzograph (1, 10, 100)
- **owl2bench:** nrese (rl-1, ql-1); jena (rl-1, ql-1); nemo (rl-1, ql-1); owlrl (rl-1, ql-1); graphdb (rl-1, ql-1); rdfox (rl-1, ql-1)
- **ldbc-spb:** nrese (50m, 256m); graphdb (50m, 256m)
- **integration-rg-gs-gnd:** nrese (example, cohort, full); jena (example, cohort, full); graphdb (example, cohort, full); rdfox (example, cohort, full)
- **real-ontologies:** nrese (dbpedia-core, yago-tiny); jena (dbpedia-core, yago-tiny); rdf4j (dbpedia-core, yago-tiny); nemo (dbpedia-core, yago-tiny); owlrl (dbpedia-core, yago-tiny); graphdb (dbpedia-core, yago-tiny); anzograph (dbpedia-core, yago-tiny)
- **consistency:** nrese (lubm-1, owl2bench-rl-1); graphdb (lubm-1, owl2bench-rl-1)
- **w3c-owl2-rl:** nrese (-)
- **ore-2015:** nrese (dev, full); hermit (dev, full); openllet (dev, full); konclude (dev, full); elk (dev, full)
- **w3c-owl2-dl:** nrese (-); hermit (-); openllet (-); konclude (-); elk (-)
- **owl2bench-classify:** nrese (el-1, dl-1, dl-10); hermit (el-1, dl-1, dl-10); openllet (el-1, dl-1, dl-10); konclude (el-1, dl-1, dl-10); elk (el-1, dl-1, dl-10)
- **basics-mix:** nrese (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); qlever (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); oxigraph (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); virtuoso (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); jena (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); millenniumdb (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); rdf4j (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); graphdb (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); rdfox (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); anzograph (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m)
- **sparqloscope:** nrese (sparqloscope-dblp); qlever (sparqloscope-dblp); oxigraph (sparqloscope-dblp); virtuoso (sparqloscope-dblp); jena (sparqloscope-dblp); millenniumdb (sparqloscope-dblp); rdf4j (sparqloscope-dblp); graphdb (sparqloscope-dblp); rdfox (sparqloscope-dblp); anzograph (sparqloscope-dblp)
- **bsbm:** nrese (bsbm-explore-100m, bsbm-bi-100m); qlever (bsbm-explore-100m, bsbm-bi-100m); oxigraph (bsbm-explore-100m, bsbm-bi-100m); virtuoso (bsbm-explore-100m, bsbm-bi-100m); jena (bsbm-explore-100m, bsbm-bi-100m); millenniumdb (bsbm-explore-100m, bsbm-bi-100m); rdf4j (bsbm-explore-100m, bsbm-bi-100m); graphdb (bsbm-explore-100m, bsbm-bi-100m); rdfox (bsbm-explore-100m, bsbm-bi-100m); anzograph (bsbm-explore-100m, bsbm-bi-100m)
- **watdiv:** nrese (watdiv-10m, watdiv-100m); qlever (watdiv-10m, watdiv-100m); oxigraph (watdiv-10m, watdiv-100m); virtuoso (watdiv-10m, watdiv-100m); jena (watdiv-10m, watdiv-100m); millenniumdb (watdiv-10m, watdiv-100m); rdf4j (watdiv-10m, watdiv-100m); graphdb (watdiv-10m, watdiv-100m); rdfox (watdiv-10m, watdiv-100m); anzograph (watdiv-10m, watdiv-100m)
- **sp2bench:** nrese (sp2bench-1m, sp2bench-25m); qlever (sp2bench-1m, sp2bench-25m); oxigraph (sp2bench-1m, sp2bench-25m); virtuoso (sp2bench-1m, sp2bench-25m); jena (sp2bench-1m, sp2bench-25m); millenniumdb (sp2bench-1m, sp2bench-25m); rdf4j (sp2bench-1m, sp2bench-25m); graphdb (sp2bench-1m, sp2bench-25m); rdfox (sp2bench-1m, sp2bench-25m); anzograph (sp2bench-1m, sp2bench-25m)
- **gcare:** nrese (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); qlever (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); oxigraph (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); virtuoso (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); jena (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); millenniumdb (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); rdf4j (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); graphdb (gcare-aids, gcare-human, gcare-lubm80, gcare-yago); anzograph (gcare-aids, gcare-human, gcare-lubm80, gcare-yago)
- **wdbench:** nrese (wdbench); qlever (wdbench); oxigraph (wdbench); virtuoso (wdbench); jena (wdbench); millenniumdb (wdbench); rdf4j (wdbench); graphdb (wdbench); anzograph (wdbench)
- **write-scaling:** nrese (1m, 10m); graphdb (1m, 10m)
- **w3c-sparql11:** nrese (-)
- **w3c-shacl:** nrese (-)
- **era-shacl:** nrese (ES, FR, ERA); jena (ES, FR, ERA); rdf4j (ES, FR, ERA); graphdb (ES, FR, ERA)
- **fulltext-search:** nrese (-); qlever (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-)
- **geosparql:** nrese (compliance)
- **federation:** nrese (-); qlever (-); oxigraph (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-); anzograph (-)
- **starbench:** nrese (bkr); oxigraph (bkr); jena (bkr); rdf4j (bkr); graphdb (bkr)
- **geographica2:** nrese (real, census, synthetic, scalability); qlever (real, census, synthetic, scalability); virtuoso (real, census, synthetic, scalability); jena (real, census, synthetic, scalability); rdf4j (real, census, synthetic, scalability); graphdb (real, census, synthetic, scalability); rdfox (real, census, synthetic, scalability)
- **fedshop:** nrese (20, 100, 200); qlever (20, 100, 200); oxigraph (20, 100, 200); virtuoso (20, 100, 200); jena (20, 100, 200); rdf4j (20, 100, 200); graphdb (20, 100, 200); anzograph (20, 100, 200)
- **vibe:** nrese (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); qdrant (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); milvus (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); pgvector (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized); lancedb (agnews-mxbai-1024-euclidean, arxiv-nomic-768-normalized)
- **vector-dbbench:** nrese (performance768d1m, performance768d1m1p, performance768d1m99p); qdrant (performance768d1m, performance768d1m1p, performance768d1m99p); milvus (performance768d1m, performance768d1m1p, performance768d1m99p); pgvector (performance768d1m, performance768d1m1p, performance768d1m99p); lancedb (performance768d1m, performance768d1m1p, performance768d1m99p)
- **larger-than-memory:** nrese (16g, 12g, 8g, 6g, 4g, 2g); qlever (16g, 12g, 8g, 6g, 4g, 2g); oxigraph (16g, 12g, 8g, 6g, 4g, 2g); virtuoso (16g, 12g, 8g, 6g, 4g, 2g); jena (16g, 12g, 8g, 6g, 4g, 2g); millenniumdb (16g, 12g, 8g, 6g, 4g, 2g); rdf4j (16g, 12g, 8g, 6g, 4g, 2g); graphdb (16g, 12g, 8g, 6g, 4g, 2g); anzograph (16g, 12g, 8g, 6g, 4g, 2g)
- **fast:** nrese (q-cycles~plain, rl-hierarchy~closure, rl-clique); qlever (q-cycles~plain, rl-hierarchy~closure, rl-clique); oxigraph (q-cycles~plain, rl-hierarchy~closure, rl-clique); virtuoso (q-cycles~plain, rl-hierarchy~closure, rl-clique); jena (q-cycles~plain, rl-hierarchy~closure, rl-clique); millenniumdb (q-cycles~plain, rl-hierarchy~closure, rl-clique); rdf4j (q-cycles~plain, rl-hierarchy~closure, rl-clique); graphdb (q-cycles~plain, rl-hierarchy~closure, rl-clique); rdfox (q-cycles~plain, rl-hierarchy~closure, rl-clique); anzograph (q-cycles~plain, rl-hierarchy~closure, rl-clique)
- **clients:** nrese (-)
- **claros:** nrese (l, le); nemo (l, le); souffle (l, le); vlog (l, le); glog (l, le)
- **datalog-graphs:** nrese (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); nemo (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); souffle (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); vlog (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql); glog (tc-G10K, sg-G10K, cspa-httpd, cspa-linux, cspa-postgresql)
- **commit-latency:** nrese (lubm-10); graphdb (lubm-10)
- **support-stress:** nrese (copies-8, copies-16, copies-17, copies-256, product-64, product-256, product-1024, product-4096, ladder-16, ladder-256, ladder-4096)
- **elk-ontologies:** nrese (go, chebi, ncit, galen); hermit (go, chebi, ncit, galen); openllet (go, chebi, ncit, galen); konclude (go, chebi, ncit, galen); elk (go, chebi, ncit, galen)
- **pagoda-suite:** nrese (lubm, uobm, fly, npd, dbpedia); graphdb (lubm, uobm, fly, npd, dbpedia)
- **vector-recall:** nrese (generated-200k, yfcc-10m); qlever (generated-200k, yfcc-10m); oxigraph (generated-200k, yfcc-10m); virtuoso (generated-200k, yfcc-10m); jena (generated-200k, yfcc-10m); millenniumdb (generated-200k, yfcc-10m); rdf4j (generated-200k, yfcc-10m); graphdb (generated-200k, yfcc-10m); anzograph (generated-200k, yfcc-10m)
- **ann-benchmarks:** nrese (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); qlever (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); oxigraph (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); virtuoso (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); jena (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); millenniumdb (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); rdf4j (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); graphdb (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean); anzograph (sift-128-euclidean, glove-100-angular, fashion-mnist-784-euclidean)

### Build variants without an outcome (nrese-fsst, nrese-oxigraph, nemo-schemafirst, nemo-sparq; optional)

- **lubm:** nrese-fsst (1, 10, 100, 1000); nrese-oxigraph (1, 10, 100, 1000); nemo-schemafirst (1, 10, 100, 1000); nemo-sparq (1, 10, 100, 1000)
- **lubm-materialised:** nrese-fsst (1, 10, 100); nrese-oxigraph (1, 10, 100)
- **owl2bench:** nrese-fsst (rl-1, ql-1); nrese-oxigraph (rl-1, ql-1); nemo-schemafirst (rl-1, ql-1); nemo-sparq (rl-1, ql-1)
- **integration-rg-gs-gnd:** nrese-fsst (example, cohort, full); nrese-oxigraph (example, cohort, full)
- **real-ontologies:** nrese-fsst (dbpedia-core, yago-tiny); nrese-oxigraph (dbpedia-core, yago-tiny); nemo-schemafirst (dbpedia-core, yago-tiny); nemo-sparq (dbpedia-core, yago-tiny)
- **consistency:** nrese-fsst (lubm-1, owl2bench-rl-1); nrese-oxigraph (lubm-1, owl2bench-rl-1)
- **ore-2015:** nrese-fsst (dev, full); nrese-oxigraph (dev, full)
- **w3c-owl2-dl:** nrese-fsst (-); nrese-oxigraph (-)
- **owl2bench-classify:** nrese-fsst (el-1, dl-1, dl-10); nrese-oxigraph (el-1, dl-1, dl-10)
- **basics-mix:** nrese-fsst (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); nrese-oxigraph (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m)
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


### Workloads not ready

- **ldbc-spb** (partial): LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates. The industry's audited reasoning benchmark (GraphDB publishes audited results). Its advanced query mix also needs fulltext and geosparql; the kit runs the basic mix with the benchmark's own ruleset on both systems. The build host's GraphDB licence is Free (1 CPU core, two concurrent queries; checked 12 Oct 2026): the comparison is the matched one-CPU scenario, and the audited Enterprise results can't be reproduced with it
- **integration-rg-gs-gnd** (partial): RG x GS x GND integration: identity reasoning over real links, 10 competency questions. NRESE and Fuseki run; cohort and full tiers and the other systems are open
- **real-ontologies** (planned): DBpedia and YAGO with their ontologies: materialisation over real hierarchies.
- **consistency** (planned): Injected contradictions: detection and explanation.
- **ore-2015** (partial): ORE 2015: classification of 1,920 real ontologies. dev: the development subset (80 tasks: 40 DL and 20 EL classifications, 20 DL consistency checks, up to 5 MB; ore.py subset); full: all 1,920 ontologies x 3 tasks (a cluster host). NRESE classifies the EL track (benches/reasoning/dl/nrese.py el); its DL engines come with phase 3
- **owl2bench-classify** (planned): OWL2Bench EL and DL: classification and consistency of the generated ontologies. Konclude times out on the DL variants at 1 and 10 (SemREC 2021): opening 4 of docs/design/owl2-dl-performance.md
- **watdiv** (partial): WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test. Oxigraph's and Jena's home turf (owner, 12 Oct 2026), with BSBM and SP2Bench; the queries are instantiated from the generator's saved state of each dataset
- **gcare** (planned): G-CARE: cardinality estimation for subgraph queries (SIGMOD 2020). The planner's estimates on the cardinality-estimation benchmark; G-CARE's estimators (characteristic sets, SumRDF, BoundSketch, WanderJoin, JSUB, ...) are the comparators
- **wdbench** (planned): WDBench: about 2,650 real queries from Wikidata's logs (BGPs, OPTIONAL, paths, C2RPQs). MillenniumDB's home turf; larger than the build and timed hosts hold for the competitors: the big-memory host or a cluster host
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
- **claros** (partial): Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques. The worst case for proof-based deletion; GLog's single-thread result (Claros-L 119 s against RDFox's 2,512 s, reported). The kit runs NRESE (the program as user rules), Soufflé, Nemo, VLog and GLog; RDFox waits for a licence. LE's closure may not fit the build host's 31 GiB on every engine
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

## Home turf, per contender

Each contender's home-turf cases, whether they can run here and its newest verdict on them ([STANDINGS.md](STANDINGS.md) has every comparison).

| Contender | Home-turf cases | Runnable here | Newest verdict on its home turf |
|---|---|---|---|
| qlever | q-large-result, text-search, clients-sweep, cache-replay, sparqloscope-dblp | yes; cache-replay: no content check (not timed); sparqloscope-dblp: declared memory-limit; sparqloscope-dblp: no content check (not timed) | q-large-result: not measured; text-search: not measured; clients-sweep: not measured; cache-replay: not measured; sparqloscope-dblp: not measured |
| oxigraph | open-first-query, watdiv-10m | yes; watdiv-10m: no content check (not timed) | open-first-query: not measured; watdiv-10m: not measured |
| virtuoso | clients-lookups, writes-under-readers, bsbm-explore-100m, bsbm-bi-100m | yes; bsbm-explore-100m: no content check (not timed); bsbm-bi-100m: no content check (not timed) | clients-lookups: not measured; writes-under-readers: not measured; bsbm-explore-100m: not measured; bsbm-bi-100m: not measured |
| jena | horst-lubm10, load-entities, watdiv-100m | yes; horst-lubm10: no content check (not timed); watdiv-100m: no content check (not timed) | horst-lubm10: not measured; load-entities: not measured; watdiv-100m: not measured |
| millenniumdb | **gap: none defined** | yes (docker, rootfs) | - |
| rdf4j | commits-plain | yes | commits-plain: not measured |
| nemo | custom-tc | yes | custom-tc: not measured |
| souffle | **gap: none defined** | yes (kit) | - |
| vlog | **gap: none defined** | yes (kit) | - |
| glog | **gap: none defined** | yes (kit) | - |
| owlrl | **gap: none defined** | yes (docker, apptainer) | - |
| hermit | **gap: none defined** | yes (kit) | - |
| openllet | dl-transitive | yes | dl-transitive: not measured |
| konclude | dl-transitive-400, horn-classify | yes; dl-transitive-400: declared gave-up; horn-classify: no content check (not timed) | dl-transitive-400: not measured; horn-classify: not measured |
| elk | el-classify, el-classify-irregular | yes | el-classify: not measured; el-classify-irregular: not measured |
| graphdb | **gap: none defined** | licensed: runs, its results stay local | - |
| rdfox | **gap: none defined** | no: refused (no evaluation licence (12 Oct 2026): its published numbers only, as reported) | - |
| allegrograph | **gap: none defined** | no: refused (no evaluation licence (12 Oct 2026): its published numbers only, as reported) | - |
| stardog | **gap: none defined** | no: nothing here starts it | - |
| anzograph | **gap: none defined** | licensed: runs, its results stay local | - |
| qdrant | **gap: none defined** | yes (kit) | - |
| milvus | **gap: none defined** | yes (kit) | - |
| pgvector | **gap: none defined** | yes (kit) | - |
| lancedb | **gap: none defined** | yes (kit) | - |
| topbraid | **gap: none defined** | yes (kit) | - |
| pyshacl | **gap: none defined** | yes (kit) | - |
| maplib | **gap: none defined** | yes (kit) | - |
| corese | **gap: none defined** | yes (kit) | - |

Gaps, no home-turf case defined (19 of 28): millenniumdb, souffle, vlog, glog, owlrl, hermit, graphdb, rdfox, allegrograph, stardog, anzograph, qdrant, milvus, pgvector, lancedb, topbraid, pyshacl, maplib, corese.
Home turfs of systems not in suite/systems.toml (no runner here): glog-vlog.

## Runs

Newest first; the records are in [runs/](runs/).

| Run | Started | Line | Kit | Host | Commit | Pairs | Purpose |
|---|---|---|---|---|---|---|---|
