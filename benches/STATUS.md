# Benchmark status

Generated 2026-10-12 by `python benches/suite/suite.py status --write` from [suite/systems.toml](suite/systems.toml), [suite/workloads.toml](suite/workloads.toml) and the run records in [runs/](runs/README.md). Don't edit it by hand: change those and regenerate. How to read and extend it: [README.md](README.md).

## At a glance

- **Workloads:** 34 (6 partial, 18 planned, 10 ready)
- **Systems:** 21 (16 free to publish, 5 only with the vendor's permission)
- **Pairs** (standard tier × system that can run it; build variants apart): 441; run at least once: 103; ok on their newest run: 79
- **NRESE:** 18 of its 72 standard tiers ok on their newest run
- **Run records:** 33, newest 2026-10-12

## Schedule

The order the families are measured in ([suite/schedule.toml](suite/schedule.toml), the owner's of 12 Oct 2026): the measurement queue starts jobs in it. A family without a workload entry yet is a benchmark of [CATALOG.md](CATALOG.md), named by its section.

### This week, in this order

| # | Family | State | Newest run | What |
|---|---|---|---|---|
| 1 | sparqloscope | planned | - | Sparqloscope: 105 generated queries covering the SPARQL 1.1 features one by one |
| 2 | bsbm | planned | - | Berlin SPARQL Benchmark: e-commerce explore, business intelligence, explore and update |
| 3 | watdiv | planned | - | WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test |
| 4 | gcare | planned | - | G-CARE: subgraph queries for cardinality estimation on real and generated graphs |
| 5 | owl2bench | ready | 2026-10-09 | OWL2Bench: one TBox per OWL 2 profile, 22 queries |
| 6 | lubm | ready | 2026-10-09 | LUBM: universities, 14 queries |
| 7 | claros | planned | - | Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques |
| 8 | sspe | planned | - | SSPE: single-source path graphs where one deletion invalidates long derivations |
| 9 | ldbc-spb | planned | - | LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates |
| 10 | datalog-graphs | planned | - | Datalog graph programs: transitive closure, same generation, CSPA |
| 11 | commit-latency | partial | - | Commit latency under 0, 1, 8 and 64 readers, the closure complete at every commit |
| 12 | ore-2015 | partial | 2026-10-03 | ORE 2015: classification of 1,920 real ontologies |
| 13 | elk-ontologies | planned | - | Classification of large EL and near-EL ontologies: GO, ChEBI, NCIt, GALEN |
| 14 | pagoda-suite | planned | - | PAGOdA's test suite: query answering beyond OWL 2 RL |
| 15 | vector-recall | partial | - | Filtered k-NN: recall and time of vector search under a filter |
| 16 | ann-benchmarks | planned | - | ann-benchmarks: recall and throughput curves of approximate k-NN |
| 17 | geographica2 | planned | - | Geographica 2: spatial selections, joins and aggregates on real and synthetic data |
| 18 | era-shacl | planned | - | ERA-SHACL-Benchmark: validation time and memory on a real railway knowledge graph |
| 19 | fast | partial | 2026-10-10 | The fast suite's cases on their comparators: each case under the semantics a system supports |

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
| 10 | wdbench | catalogue only | - | CATALOG.md §3.2: WDBench, Wikidata's query log (MillenniumDB's home turf) |
| 11 | sp2bench | catalogue only | - | CATALOG.md §3.2: SP2Bench, DBLP-shaped data and 17 queries |
| 12 | npd | catalogue only | - | CATALOG.md §3.6: NPD, OBDA over the Norwegian Petroleum Directorate (Ontop's home turf) |
| 13 | lubm-exists | catalogue only | - | CATALOG.md §3.6: LUBM∃, LUBM with existential rules (Ontop, Stardog, PAGOdA) |
| 14 | starbench | catalogue only | - | CATALOG.md §3.10: StarBench, RDF-star queries |
| 15 | fedshop | catalogue only | - | CATALOG.md §3.11: FedShop, federated queries over many shops |
| 16 | beir | catalogue only | - | CATALOG.md §3.9: BEIR, retrieval quality and time |
| 17 | vibe | catalogue only | - | CATALOG.md §3.13: VIBE, vector index benchmark |
| 18 | soak | catalogue only | - | CATALOG.md §3.16: soak, hours of mixed load |
| 19 | larger-than-memory | catalogue only | - | CATALOG.md §3.17: larger than memory, store sizes past the RAM |

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

| Workload | State | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | nemo | nemo-schemafirst | nemo-sparq | owlrl | hermit | openllet | konclude | elk | graphdb | rdfox | allegrograph | stardog | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| lubm | ready | 4/4 | · | 3/4 | – | – | – | 1/4 ! | – | 3/4 | 3/4 | 3/4 ! | 1/4 | – | – | – | – | 4/4 | · | – | n/a | n/a |
| lubm-materialised | ready | 3/3 | · | · | 3/3 | 3/3 | 3/3 | 3/3 | 3/3 | – | – | – | – | – | – | – | – | 3/3 | · | n/a | n/a | · |
| owl2bench | ready | 1/2 ! | · | 1/2 | – | – | – | 1/2 | – | 2/2 | 2/2 | · | 1/2 ! | – | – | – | – | 2/2 | · | – | n/a | n/a |
| ldbc-spb | planned | · | · | · | – | – | – | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · |
| integration-rg-gs-gnd | partial | · | · | · | – | – | – | · | – | – | – | – | – | – | – | – | – | · | · | – | n/a | n/a |
| real-ontologies | planned | · | · | · | – | – | – | · | · | · | · | · | · | – | – | – | – | · | · | n/a | n/a | · |
| consistency | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | – | n/a | – |
| w3c-owl2-rl | ready | 0/1 ! | n/a | n/a | – | – | – | – | – | n/a | n/a | n/a | n/a | – | – | – | – | n/a | n/a | – | n/a | – |
| ore-2015 | partial | 0/2 ! | · | · | – | – | – | – | – | – | – | – | – | 1/2 | 0/2 ! | 0/2 ! | 1/2 | – | · | – | – | – |
| w3c-owl2-dl | ready | 0/1 ! | · | · | – | – | – | – | – | – | – | – | – | 0/1 ! | 0/1 ! | 0/1 ! | 0/1 ! | – | · | – | – | – |
| owl2bench-classify | planned | · | · | · | – | – | – | – | – | – | – | – | – | · | · | · | · | – | · | – | – | – |
| basics-mix | ready | 5/5 | · | 3/5 ! | 4/5 ! | 4/5 ! | 2/5 ! | 5/5 | 2/5 ! | – | – | – | – | – | – | – | – | 5/5 | · | n/a | n/a | · |
| sparqloscope | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · |
| bsbm | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · |
| watdiv | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · |
| write-scaling | ready | 2/2 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – | · | n/a | n/a | n/a | n/a |
| w3c-sparql11 | ready | 1/1 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | n/a |
| w3c-shacl | ready | 1/1 | n/a | n/a | – | – | – | n/a | n/a | – | – | – | – | – | – | – | – | n/a | n/a | – | n/a | – |
| era-shacl | planned | · | · | · | – | – | – | · | · | – | – | – | – | – | – | – | – | · | · | – | n/a | – |
| fulltext-search | planned | · | · | · | · | – | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | – |
| geosparql | partial | 0/1 ! | n/a | n/a | n/a | – | n/a | n/a | n/a | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | – |
| federation | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | · | · | – | n/a | · |
| fast | partial | 1/3 | · | · | 2/3 | 1/3 ! | 1/3 | 0/3 ! | · | n/a | n/a | n/a | n/a | – | – | – | – | · | · | n/a | n/a | · |
| clients | ready | · | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | – | – | n/a | n/a | n/a | n/a | n/a |
| gcare | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · |
| claros | planned | · | · | · | – | – | – | · | – | · | · | · | · | – | – | – | – | · | · | – | n/a | – |
| sspe | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | – | – | – |
| datalog-graphs | planned | · | · | – | – | – | – | · | – | · | · | · | – | – | – | – | – | · | · | – | n/a | – |
| commit-latency | partial | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | – | – | – |
| elk-ontologies | planned | · | · | · | – | – | – | – | – | – | – | – | – | · | · | · | · | – | · | – | – | – |
| pagoda-suite | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | – | – | · | · | – | n/a | – |
| vector-recall | partial | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · |
| ann-benchmarks | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | · |
| geographica2 | planned | · | · | · | · | – | · | · | · | – | – | – | – | – | – | – | – | · | · | n/a | n/a | – |

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

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | ok 10-03 | · | · | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ran* 10-03 | · | · |
| 10 | ok 10-03 | · | · | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ran* 10-03 | · | · |
| 100 | ok 10-03 | · | · | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ran* 10-03 | · | · |

### owl2bench: OWL2Bench: one TBox per OWL 2 profile, 22 queries

ready, performance; tasks RT1, RT3; kit `benches/reasoning` (suite driver); checked by: inferred set against owlrl or Nemo; answer counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|---|
| rl-1 | ok 10-09 | · | ok 10-02 | ok 10-04 | ok 10-04 | ok 10-04 | skip 10-04 | T/O 10-04 | ran* 10-04 | skip 10-02 |
| ql-1 | WRONG 10-04 | · | · | skip 10-04 | ok 10-04 | ok 10-04 | skip 10-04 | ok 10-04 | ran* 10-04 | · |

### ldbc-spb: LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates

planned, performance; tasks RT3, RT5; kit `-` (outside the driver); checked by: the benchmark's own validation of query results and of its OWL 2 RL rules.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|
| 50m | · | · | · | · | · | · | · | · |
| 256m | · | · | · | · | · | · | · | · |

### integration-rg-gs-gnd: RG x GS x GND integration: identity reasoning over real links, 10 competency questions

partial, performance; tasks RT1, RT3; kit `benches/integration` (suite driver); checked by: answer counts across systems within one entailment regime.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | graphdb | rdfox |
|---|---|---|---|---|---|---|
| example | · | · | · | · | · | · |
| cohort | · | · | · | · | · | · |
| full | · | · | · | · | · | · |

### real-ontologies: DBpedia and YAGO with their ontologies: materialisation over real hierarchies

planned, performance; tasks RT1, RT3; kit `-` (outside the driver); checked by: inferred set against Nemo.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | rdf4j | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| dbpedia-core | · | · | · | · | · | · | · | · | · | · | · | · |
| yago-tiny | · | · | · | · | · | · | · | · | · | · | · | · |

### consistency: Injected contradictions: detection and explanation

planned, performance; tasks RT4, RT7; kit `-` (outside the driver); checked by: every injected contradiction found, none on clean data.

| Tier | nrese | nrese-fsst | nrese-oxigraph | graphdb | rdfox |
|---|---|---|---|---|---|
| lubm-1 | · | · | · | · | · |
| owl2bench-rl-1 | · | · | · | · | · |

### w3c-owl2-rl: W3C OWL 2 test cases, RL profile: entailment and consistency

ready, conformance; tasks RT1, RT4; kit `crates/nrese-store/tests/w3c_owl2_rl` (suite driver); checked by: the test suite's expected entailments.

| Tier | nrese |
|---|---|
| - | part 10-09 |

### ore-2015: ORE 2015: classification of 1,920 real ontologies

partial, performance; tasks RT6; kit `benches/reasoning/dl` (outside the driver); checked by: canonical taxonomies compared by hash across the reference reasoners; differences adjudicated (disputed.tsv).

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk | rdfox |
|---|---|---|---|---|---|---|---|---|
| dev | part 10-03 | · | · | ok 10-03 | part 10-03 | part 10-03 | ok 10-03 | · |
| full | · | · | · | · | · | · | · | · |

### w3c-owl2-dl: W3C OWL 2 test cases, species DL, direct semantics: consistency, entailment, non-entailment

ready, conformance; tasks RT4, RT6; kit `benches/reasoning/dl` (outside the driver); checked by: the test types; the working group's published results of 2009 (published-2009.tsv); disputed.tsv.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk | rdfox |
|---|---|---|---|---|---|---|---|---|
| - | part 10-09 | · | · | part 10-03 | WRONG 10-03 | WRONG 10-03 | part 10-03 | · |

### owl2bench-classify: OWL2Bench EL and DL: classification and consistency of the generated ontologies

planned, performance; tasks RT4, RT6; kit `benches/reasoning/dl` (outside the driver); checked by: canonical taxonomies across the reference reasoners.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk | rdfox |
|---|---|---|---|---|---|---|---|---|
| el-1 | · | · | · | · | · | · | · | · |
| dl-1 | · | · | · | · | · | · | · | · |
| dl-10 | · | · | · | · | · | · | · | · |

### basics-mix: Load, store size, restart and a query mix on real datasets

ready, performance; tasks basics; kit `benches/competitors` (suite driver); checked by: row counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| olympics | ok 10-03 | · | ok 10-02 | ok 10-03 | part 10-03 | WRONG 10-03 | ok 10-03 | part 10-03 | ran* 10-03 | skip 10-02 | · |
| yago-tiny | ok 10-03 | · | skip 10-02 | ok 10-03 | ok 10-03 | part 10-03 | ok 10-03 | ok 10-03 | ran* 10-03 | skip 10-02 | · |
| entities-10000000 | ok 10-03 | · | ok 10-02 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | ran* 10-03 | skip 10-02 | · |
| dbpedia-core | ok 10-03 | · | ≠? 10-02 | part 10-03 | ok 10-03 | WRONG 10-03 | ok 10-03 | FAIL 10-03 | ran* 10-03 | skip 10-02 | · |
| wikidata-lexemes-60m | ok 10-03 | · | ok 10-02 | ok 10-03 | ok 10-03 | ok 10-03 | ok 10-03 | FAIL 10-03 | ran* 10-03 | skip 10-02 | · |

### sparqloscope: Sparqloscope: 105 generated queries covering the SPARQL 1.1 features one by one

planned, performance; tasks basics; kit `-` (outside the driver); checked by: result sizes across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| dblp | · | · | · | · | · | · | · | · | · | · | · |

### bsbm: Berlin SPARQL Benchmark: e-commerce explore, business intelligence, explore and update

planned, performance; tasks basics; kit `-` (outside the driver); checked by: the driver's qualification mode.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 10m | · | · | · | · | · | · | · | · | · | · | · |
| 100m | · | · | · | · | · | · | · | · | · | · | · |

### watdiv: WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test

planned, performance; tasks basics; kit `-` (outside the driver); checked by: row counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 10m | · | · | · | · | · | · | · | · | · | · | · |
| 100m | · | · | · | · | · | · | · | · | · | · | · |

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

planned, performance; tasks validation; kit `-` (outside the driver); checked by: the validation reports across engines.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | rdf4j | graphdb | rdfox |
|---|---|---|---|---|---|---|---|
| - | · | · | · | · | · | · | · |

### fulltext-search: Keyword search: ResearchSpace's search queries; LUBMft

planned, performance; tasks search; kit `-` (outside the driver); checked by: result sets across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | virtuoso | jena | rdf4j | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|
| - | · | · | · | · | · | · | · | · | · |

### geosparql: GeoSPARQL compliance benchmark (206 queries); Geographica 2 for performance

partial, conformance; tasks space; kit `crates/nrese-sparql/tests/geosparql_compliance` (suite driver); checked by: the benchmark's expected answers.

| Tier | nrese |
|---|---|
| compliance | part 10-09 |

### federation: FedBench / W3C federation tests

planned, performance; tasks federation; kit `-` (outside the driver); checked by: answers against the single-store run.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| - | · | · | · | · | · | · | · | · | · | · | · |

### fast: The fast suite's cases on their comparators: each case under the semantics a system supports

partial, performance; tasks basics, RT1, RT3; kit `benches/fast` (suite driver); checked by: the generator's own answers and pinned counts (fast.py run); answer counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| q-cycles~plain | ok 10-10 | · | · | ok 10-10 | part 10-10 | ok 10-10 | part 10-10 | · | · | · | · |
| rl-hierarchy~closure | · | · | · | ok 10-06 | ok 10-06 | · | · | · | · | · | · |
| rl-clique | · | · | · | · | · | · | · | · | · | · | · |
| 6 plain cases (extra) | · | · | · | ok 10-06 | part 10-06 | ok 10-06 | part 10-06 | · | · | · | · |
| 84 cases (extra) | ok 10-06 | · | · | · | · | · | · | · | · | · | · |
| clients-lookups~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| clients-sweep~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | part 10-10 | ok 10-10 | · | · | · | · |
| custom-tc~custom (extra) | · | · | · | · | · | · | · | · | · | · | · |
| dl cases (extra) | · | · | · | · | · | · | · | · | · | · | · |
| dl-roles, el-classify (extra) | · | · | · | · | · | · | · | · | · | · | · |
| el-classify (extra) | · | · | · | · | · | · | · | · | · | · | · |
| horst-lubm10~owl-horst (extra) | · | · | · | · | · | · | FAIL 10-06 | · | · | · | · |
| index-join-keys~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | · | · | · | · | · | · |
| kernel-intersect~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | · | ok 10-10 | · | · | · | · |
| native-cli-cache-replay (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · |
| native-cli-classify-store-horn (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · |
| native-cli-clients-sweep (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · |
| native-cli-commits-readers (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · |
| native-cli-dlmode-lubm1 (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · |
| native-cli-writes-under-readers (extra) | ok 10-09 | · | · | · | · | · | · | · | · | · | · |
| op-dates~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| op-group~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| op-negation~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| op-optional~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| op-strings~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| q-chain~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| q-paths~plain (extra) | ok 10-10 | · | · | part 10-10 | part 10-10 | part 10-10 | part 10-10 | · | · | · | · |
| q-sideways~plain (extra) | ok 10-10 | · | · | ok 10-10 | part 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| q-snowflake~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| q-star~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| q-topk~plain (extra) | ok 10-10 | · | · | ok 10-10 | ok 10-10 | ok 10-10 | ok 10-10 | · | · | · | · |
| rdfs-hierarchy~rdfs (extra) | · | · | · | · | · | · | ok 10-06 | · | · | · | · |
| rl-clique, rl-chain, rl-owl2bench~owl-horst (extra) | · | · | · | · | · | · | hang 10-06 | · | · | · | · |
| rl-clique~owl-horst (extra) | · | · | · | · | · | · | part 10-06 | · | · | · | · |
| rule cases~owl2-rl (extra) | · | · | · | · | · | · | · | · | · | · | · |

### clients: ResearchSpace on the store; the Datamodel Workflow's export protocol

ready, conformance; tasks integration; kit `scripts` (suite driver); checked by: the scripts' assertions.

| Tier | nrese |
|---|---|
| - | · |

### gcare: G-CARE: subgraph queries for cardinality estimation on real and generated graphs

planned, performance; tasks basics; kit `-` (outside the driver); checked by: the exact counts the benchmark ships; result counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| yago | · | · | · | · | · | · | · | · | · | · | · |
| lubm80 | · | · | · | · | · | · | · | · | · | · | · |

### claros: Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques

planned, performance; tasks RT1, RT2; kit `-` (outside the driver); checked by: inferred counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | nemo-schemafirst | nemo-sparq | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|---|
| l | · | · | · | · | · | · | · | · | · | · |
| le | · | · | · | · | · | · | · | · | · | · |

### sspe: SSPE: single-source path graphs where one deletion invalidates long derivations

planned, performance; tasks RT2; kit `-` (outside the driver); checked by: the incremental closure equals the rematerialised one.

| Tier | nrese | nrese-fsst | nrese-oxigraph | graphdb | rdfox |
|---|---|---|---|---|---|
| 1000 | · | · | · | · | · |

### datalog-graphs: Datalog graph programs: transitive closure, same generation, CSPA

planned, performance; tasks RT1; kit `-` (outside the driver); checked by: counts against Soufflé or Nemo.

| Tier | nrese | nrese-fsst | jena | nemo | nemo-schemafirst | nemo-sparq | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|
| tc | · | · | · | · | · | · | · | · |
| sg | · | · | · | · | · | · | · | · |
| cspa | · | · | · | · | · | · | · | · |

### commit-latency: Commit latency under 0, 1, 8 and 64 readers, the closure complete at every commit

partial, performance; tasks RT2; kit `benches/fast` (outside the driver); checked by: the committed state equals the rematerialisation.

| Tier | nrese | nrese-fsst | nrese-oxigraph | graphdb | rdfox |
|---|---|---|---|---|---|
| lubm-10 | · | · | · | · | · |

### elk-ontologies: Classification of large EL and near-EL ontologies: GO, ChEBI, NCIt, GALEN

planned, performance; tasks RT6; kit `benches/reasoning/el-classification` (outside the driver); checked by: taxonomies against ELK's and Konclude's, by canonical hash.

| Tier | nrese | nrese-fsst | nrese-oxigraph | hermit | openllet | konclude | elk | rdfox |
|---|---|---|---|---|---|---|---|---|
| go | · | · | · | · | · | · | · | · |
| chebi | · | · | · | · | · | · | · | · |
| ncit | · | · | · | · | · | · | · | · |
| galen | · | · | · | · | · | · | · | · |

### pagoda-suite: PAGOdA's test suite: query answering beyond OWL 2 RL

planned, performance; tasks RT3; kit `-` (outside the driver); checked by: answers against a complete reasoner (HermiT, Konclude).

| Tier | nrese | nrese-fsst | nrese-oxigraph | graphdb | rdfox |
|---|---|---|---|---|---|
| lubm | · | · | · | · | · |
| uobm | · | · | · | · | · |
| fly | · | · | · | · | · |
| npd | · | · | · | · | · |
| dbpedia | · | · | · | · | · |

### vector-recall: Filtered k-NN: recall and time of vector search under a filter

partial, performance; tasks vector; kit `benches/fast` (outside the driver); checked by: recall@10 against the exact search.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| generated-200k | · | · | · | · | · | · | · | · | · | · | · |
| yfcc-10m | · | · | · | · | · | · | · | · | · | · | · |

### ann-benchmarks: ann-benchmarks: recall and throughput curves of approximate k-NN

planned, performance; tasks vector; kit `-` (outside the driver); checked by: recall against the sets' ground truth.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| sift-1m | · | · | · | · | · | · | · | · | · | · | · |
| glove-100 | · | · | · | · | · | · | · | · | · | · | · |
| fashion-mnist | · | · | · | · | · | · | · | · | · | · | · |

### geographica2: Geographica 2: spatial selections, joins and aggregates on real and synthetic data

planned, performance; tasks space; kit `-` (outside the driver); checked by: answers across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | virtuoso | jena | rdf4j | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|---|
| real | · | · | · | · | · | · | · | · | · |
| synthetic | · | · | · | · | · | · | · | · | · |

## Gaps

### Pairs that can run and have no outcome yet

- **lubm:** jena (1000); nemo (1000); owlrl (10, 100, 1000); rdfox (1, 10, 100, 1000)
- **lubm-materialised:** rdfox (1, 10, 100); anzograph (1, 10, 100)
- **owl2bench:** jena (ql-1); rdfox (rl-1, ql-1)
- **ldbc-spb:** nrese (50m, 256m); jena (50m, 256m); rdf4j (50m, 256m); graphdb (50m, 256m); rdfox (50m, 256m); anzograph (50m, 256m)
- **integration-rg-gs-gnd:** nrese (example, cohort, full); jena (example, cohort, full); graphdb (example, cohort, full); rdfox (example, cohort, full)
- **real-ontologies:** nrese (dbpedia-core, yago-tiny); jena (dbpedia-core, yago-tiny); rdf4j (dbpedia-core, yago-tiny); nemo (dbpedia-core, yago-tiny); owlrl (dbpedia-core, yago-tiny); graphdb (dbpedia-core, yago-tiny); rdfox (dbpedia-core, yago-tiny); anzograph (dbpedia-core, yago-tiny)
- **consistency:** nrese (lubm-1, owl2bench-rl-1); graphdb (lubm-1, owl2bench-rl-1); rdfox (lubm-1, owl2bench-rl-1)
- **ore-2015:** nrese (full); hermit (full); openllet (full); konclude (full); elk (full); rdfox (dev, full)
- **w3c-owl2-dl:** rdfox (-)
- **owl2bench-classify:** nrese (el-1, dl-1, dl-10); hermit (el-1, dl-1, dl-10); openllet (el-1, dl-1, dl-10); konclude (el-1, dl-1, dl-10); elk (el-1, dl-1, dl-10); rdfox (el-1, dl-1, dl-10)
- **basics-mix:** rdfox (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); anzograph (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m)
- **sparqloscope:** nrese (dblp); qlever (dblp); oxigraph (dblp); virtuoso (dblp); jena (dblp); rdf4j (dblp); graphdb (dblp); rdfox (dblp); anzograph (dblp)
- **bsbm:** nrese (10m, 100m); qlever (10m, 100m); oxigraph (10m, 100m); virtuoso (10m, 100m); jena (10m, 100m); rdf4j (10m, 100m); graphdb (10m, 100m); rdfox (10m, 100m); anzograph (10m, 100m)
- **watdiv:** nrese (10m, 100m); qlever (10m, 100m); oxigraph (10m, 100m); virtuoso (10m, 100m); jena (10m, 100m); rdf4j (10m, 100m); graphdb (10m, 100m); rdfox (10m, 100m); anzograph (10m, 100m)
- **write-scaling:** graphdb (1m, 10m)
- **era-shacl:** nrese (-); jena (-); rdf4j (-); graphdb (-); rdfox (-)
- **fulltext-search:** nrese (-); qlever (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-); rdfox (-)
- **federation:** nrese (-); qlever (-); oxigraph (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-); rdfox (-); anzograph (-)
- **fast:** nrese (rl-hierarchy~closure, rl-clique); qlever (rl-clique); oxigraph (rl-clique); virtuoso (rl-hierarchy~closure, rl-clique); jena (rl-hierarchy~closure, rl-clique); rdf4j (q-cycles~plain, rl-hierarchy~closure, rl-clique); graphdb (q-cycles~plain, rl-hierarchy~closure, rl-clique); rdfox (q-cycles~plain, rl-hierarchy~closure, rl-clique); anzograph (q-cycles~plain, rl-hierarchy~closure, rl-clique)
- **clients:** nrese (-)
- **gcare:** nrese (yago, lubm80); qlever (yago, lubm80); oxigraph (yago, lubm80); virtuoso (yago, lubm80); jena (yago, lubm80); rdf4j (yago, lubm80); graphdb (yago, lubm80); rdfox (yago, lubm80); anzograph (yago, lubm80)
- **claros:** nrese (l, le); jena (l, le); nemo (l, le); owlrl (l, le); graphdb (l, le); rdfox (l, le)
- **sspe:** nrese (1000); graphdb (1000); rdfox (1000)
- **datalog-graphs:** nrese (tc, sg, cspa); jena (tc, sg, cspa); nemo (tc, sg, cspa); graphdb (tc, sg, cspa); rdfox (tc, sg, cspa)
- **commit-latency:** nrese (lubm-10); graphdb (lubm-10); rdfox (lubm-10)
- **elk-ontologies:** nrese (go, chebi, ncit, galen); hermit (go, chebi, ncit, galen); openllet (go, chebi, ncit, galen); konclude (go, chebi, ncit, galen); elk (go, chebi, ncit, galen); rdfox (go, chebi, ncit, galen)
- **pagoda-suite:** nrese (lubm, uobm, fly, npd, dbpedia); graphdb (lubm, uobm, fly, npd, dbpedia); rdfox (lubm, uobm, fly, npd, dbpedia)
- **vector-recall:** nrese (generated-200k, yfcc-10m); qlever (generated-200k, yfcc-10m); oxigraph (generated-200k, yfcc-10m); virtuoso (generated-200k, yfcc-10m); jena (generated-200k, yfcc-10m); rdf4j (generated-200k, yfcc-10m); graphdb (generated-200k, yfcc-10m); rdfox (generated-200k, yfcc-10m); anzograph (generated-200k, yfcc-10m)
- **ann-benchmarks:** nrese (sift-1m, glove-100, fashion-mnist); qlever (sift-1m, glove-100, fashion-mnist); oxigraph (sift-1m, glove-100, fashion-mnist); virtuoso (sift-1m, glove-100, fashion-mnist); jena (sift-1m, glove-100, fashion-mnist); rdf4j (sift-1m, glove-100, fashion-mnist); graphdb (sift-1m, glove-100, fashion-mnist); rdfox (sift-1m, glove-100, fashion-mnist); anzograph (sift-1m, glove-100, fashion-mnist)
- **geographica2:** nrese (real, synthetic); qlever (real, synthetic); virtuoso (real, synthetic); jena (real, synthetic); rdf4j (real, synthetic); graphdb (real, synthetic); rdfox (real, synthetic)

### Build variants without an outcome (nrese-fsst, nrese-oxigraph, nemo-schemafirst, nemo-sparq; optional)

- **lubm:** nrese-fsst (1, 10, 100, 1000); nrese-oxigraph (1000); nemo-schemafirst (1000)
- **lubm-materialised:** nrese-fsst (1, 10, 100); nrese-oxigraph (1, 10, 100)
- **owl2bench:** nrese-fsst (rl-1, ql-1); nrese-oxigraph (ql-1); nemo-sparq (rl-1, ql-1)
- **ldbc-spb:** nrese-fsst (50m, 256m); nrese-oxigraph (50m, 256m)
- **integration-rg-gs-gnd:** nrese-fsst (example, cohort, full); nrese-oxigraph (example, cohort, full)
- **real-ontologies:** nrese-fsst (dbpedia-core, yago-tiny); nrese-oxigraph (dbpedia-core, yago-tiny); nemo-schemafirst (dbpedia-core, yago-tiny); nemo-sparq (dbpedia-core, yago-tiny)
- **consistency:** nrese-fsst (lubm-1, owl2bench-rl-1); nrese-oxigraph (lubm-1, owl2bench-rl-1)
- **ore-2015:** nrese-fsst (dev, full); nrese-oxigraph (dev, full)
- **w3c-owl2-dl:** nrese-fsst (-); nrese-oxigraph (-)
- **owl2bench-classify:** nrese-fsst (el-1, dl-1, dl-10); nrese-oxigraph (el-1, dl-1, dl-10)
- **basics-mix:** nrese-fsst (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); nrese-oxigraph (yago-tiny)
- **sparqloscope:** nrese-fsst (dblp); nrese-oxigraph (dblp)
- **bsbm:** nrese-fsst (10m, 100m); nrese-oxigraph (10m, 100m)
- **watdiv:** nrese-fsst (10m, 100m); nrese-oxigraph (10m, 100m)
- **era-shacl:** nrese-fsst (-); nrese-oxigraph (-)
- **fulltext-search:** nrese-fsst (-); nrese-oxigraph (-)
- **federation:** nrese-fsst (-); nrese-oxigraph (-)
- **fast:** nrese-fsst (q-cycles~plain, rl-hierarchy~closure, rl-clique); nrese-oxigraph (q-cycles~plain, rl-hierarchy~closure, rl-clique)
- **gcare:** nrese-fsst (yago, lubm80); nrese-oxigraph (yago, lubm80)
- **claros:** nrese-fsst (l, le); nrese-oxigraph (l, le); nemo-schemafirst (l, le); nemo-sparq (l, le)
- **sspe:** nrese-fsst (1000); nrese-oxigraph (1000)
- **datalog-graphs:** nrese-fsst (tc, sg, cspa); nemo-schemafirst (tc, sg, cspa); nemo-sparq (tc, sg, cspa)
- **commit-latency:** nrese-fsst (lubm-10); nrese-oxigraph (lubm-10)
- **elk-ontologies:** nrese-fsst (go, chebi, ncit, galen); nrese-oxigraph (go, chebi, ncit, galen)
- **pagoda-suite:** nrese-fsst (lubm, uobm, fly, npd, dbpedia); nrese-oxigraph (lubm, uobm, fly, npd, dbpedia)
- **vector-recall:** nrese-fsst (generated-200k, yfcc-10m); nrese-oxigraph (generated-200k, yfcc-10m)
- **ann-benchmarks:** nrese-fsst (sift-1m, glove-100, fashion-mnist); nrese-oxigraph (sift-1m, glove-100, fashion-mnist)
- **geographica2:** nrese-fsst (real, synthetic); nrese-oxigraph (real, synthetic)

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
- owl2bench ql-1 on nrese: wrong (2026-10-03-office-b): wrong answer counts: q01
- owl2bench rl-1 on owlrl: timeout (2026-10-03-office-b)
- w3c-owl2-dl - on elk: partial (2026-10-03-dl-reference): EL tasks only; the rest use constructs outside its fragment
- w3c-owl2-dl - on hermit: partial (2026-10-03-dl-reference): 27 timeouts, 1 crash (WebOnt-Thing-003), no wrong result
- w3c-owl2-dl - on konclude: wrong (2026-10-03-dl-reference): 30 wrong results, 100 unsupported (datatypes, keys)
- w3c-owl2-dl - on nrese: partial (2026-10-09-office-conformance-a633): DL-909 disputed
- w3c-owl2-dl - on openllet: wrong (2026-10-03-dl-reference): 6 wrong results, 10 timeouts
- w3c-owl2-rl - on nrese: partial (2026-10-09-office-conformance-a633): two import skips, one functional-only premise skip

### Workloads not ready

- **ldbc-spb** (planned): LDBC Semantic Publishing Benchmark 2.0 (BBC): queries under inference with continuous updates. The industry's audited reasoning benchmark (GraphDB publishes audited results). Its advanced query mix also needs fulltext and geosparql
- **integration-rg-gs-gnd** (partial): RG x GS x GND integration: identity reasoning over real links, 10 competency questions. NRESE and Fuseki run; cohort and full tiers and the other systems are open
- **real-ontologies** (planned): DBpedia and YAGO with their ontologies: materialisation over real hierarchies.
- **consistency** (planned): Injected contradictions: detection and explanation.
- **ore-2015** (partial): ORE 2015: classification of 1,920 real ontologies. dev: the development subset (80 tasks: 40 DL and 20 EL classifications, 20 DL consistency checks, up to 5 MB; ore.py subset); full: all 1,920 ontologies x 3 tasks (Draco). NRESE classifies the EL track (benches/reasoning/dl/nrese.py el); its DL engines come with phase 3
- **owl2bench-classify** (planned): OWL2Bench EL and DL: classification and consistency of the generated ontologies. Konclude times out on the DL variants at 1 and 10 (SemREC 2021): opening 4 of docs/design/owl2-dl-performance.md
- **sparqloscope** (planned): Sparqloscope: 105 generated queries covering the SPARQL 1.1 features one by one. The current cross-engine query benchmark; its authors publish QLever, Virtuoso, MillenniumDB, GraphDB, Blazegraph and Jena on it
- **bsbm** (planned): Berlin SPARQL Benchmark: e-commerce explore, business intelligence, explore and update. The classic mixed query and update workload; most vendors publish numbers on it
- **watdiv** (planned): WatDiv: diverse query shapes (linear, star, snowflake, complex) as a stress test.
- **era-shacl** (planned): ERA-SHACL-Benchmark: validation time and memory on a real railway knowledge graph. Parts of its shapes use SHACL-SPARQL
- **fulltext-search** (planned): Keyword search: ResearchSpace's search queries; LUBMft.
- **geosparql** (partial): GeoSPARQL compliance benchmark (206 queries); Geographica 2 for performance. The compliance part runs on NRESE in CI: 175 of 212; the 37 others are reference answers that contradict GeoSPARQL's definitions or each other (expected-failures.txt). Geographica 2 (performance) is to add
- **federation** (planned): FedBench / W3C federation tests.
- **fast** (partial): The fast suite's cases on their comparators: each case under the semantics a system supports. Tier = CASE~SEMANTICS (benches/fast/compete.py prepares the data in the volume nrese-fast-data). NRESE runs every case in benches/fast/fast.py; this workload is the comparison
- **gcare** (planned): G-CARE: subgraph queries for cardinality estimation on real and generated graphs. Measures the planner's estimates (q-error per query) besides the queries' times; the fast case plan-estimates measures q-error on LUBM 10 today
- **claros** (planned): Claros (L, LE): RDFox's and GLog's datasets, rules that build large cliques. The worst case for proof-based deletion; GLog's single-thread result (Claros-L 119 s against RDFox's 2,512 s, reported). Size LE on Draco first
- **sspe** (planned): SSPE: single-source path graphs where one deletion invalidates long derivations. Counting beat B/F 10.5 s to 253 s for 1,000 deletions (reported); the fast case deletes-layered is its small cousin
- **datalog-graphs** (planned): Datalog graph programs: transitive closure, same generation, CSPA. The batch executor beyond OWL: plain Datalog as N3 rules; Nemo, VLog and Soufflé on their home turf
- **commit-latency** (partial): Commit latency under 0, 1, 8 and 64 readers, the closure complete at every commit. The fast cases commits-readers and writes-under-readers cover it on NRESE; GraphDB and RDFox are the comparators
- **elk-ontologies** (planned): Classification of large EL and near-EL ontologies: GO, ChEBI, NCIt, GALEN. ELK's home turf: EL classification speed
- **pagoda-suite** (planned): PAGOdA's test suite: query answering beyond OWL 2 RL. Query answering by lower and upper bounds, as NRESE's DL mode answers
- **vector-recall** (partial): Filtered k-NN: recall and time of vector search under a filter. The fast cases vector-knn-exact and vector-knn-filtered; why the filtered recall fails: runs/2026-10-12-vector-recall-phuoc.toml
- **ann-benchmarks** (planned): ann-benchmarks: recall and throughput curves of approximate k-NN. Comparability with every published index
- **geographica2** (planned): Geographica 2: spatial selections, joins and aggregates on real and synthetic data. Strabon, uSeekM, Parliament published on it; Jena and GraphDB's GeoSPARQL are the comparators here

## Runs

Newest first; the records are in [runs/](runs/).

| Run | Started | Kit | Host | Commit | Pairs | Purpose |
|---|---|---|---|---|---|---|
| [2026-10-12-vector-recall-phuoc](runs/2026-10-12-vector-recall-phuoc.toml) | 2026-10-12T14:00 | fast (vector-knn-filtered's queries in variants; tmp diagnosis script through queue.py) | phuoc-yu | aabd47a732 | 0 | Why vector-knn-filtered's approximate recall fails its 0.9 gate: the graph's own recall, the filter's share, the search beam |
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
