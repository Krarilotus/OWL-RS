# Benchmark status

Generated 2026-10-03 by `python benches/suite/suite.py status --write` from [suite/systems.toml](suite/systems.toml), [suite/workloads.toml](suite/workloads.toml) and the run records in [runs/](runs/README.md). Don't edit it by hand: change those and regenerate. How to read and extend it: [README.md](README.md).

## At a glance

- **Workloads:** 23 (3 partial, 10 planned, 10 ready)
- **Systems:** 18 (14 free to publish, 4 only with the vendor's permission)
- **Pairs** (standard tier × system that can run it; build variants apart): 263; run at least once: 79; ok on their newest run: 66
- **NRESE:** 17 of its 44 standard tiers ok on their newest run
- **Run records:** 8, newest 2026-10-03

## Coverage

Per workload and system: standard tiers with an `ok` (or `ran*`) newest outcome / standard tiers. A `!` marks a newest outcome that is `FAIL`, `T/O`, `WRONG` or `part` on some tier.

| Workload | State | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | nemo | owlrl | hermit | openllet | konclude | elk | graphdb | rdfox | stardog | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| lubm | ready | 3/4 | · | 3/4 | – | – | – | 1/4 ! | – | 3/4 | 1/4 | – | – | – | – | 3/4 | · | n/a | n/a |
| lubm-materialised | ready | 3/3 | · | · | 3/3 | 3/3 | 3/3 | 3/3 | · | – | – | – | – | – | – | 3/3 | · | n/a | · |
| owl2bench | ready | 2/2 | · | 1/2 | – | – | – | · | – | 1/2 | 0/2 ! | – | – | – | – | 1/2 | · | n/a | n/a |
| ldbc-spb | planned | · | · | · | – | – | – | · | · | – | – | – | – | – | – | · | · | n/a | · |
| integration-rg-gs-gnd | partial | · | · | · | – | – | – | · | – | – | – | – | – | – | – | · | · | n/a | n/a |
| real-ontologies | planned | · | · | · | – | – | – | · | · | · | · | – | – | – | – | · | · | n/a | · |
| consistency | planned | · | · | · | – | – | – | – | – | – | – | – | – | – | – | · | · | n/a | – |
| w3c-owl2-rl | ready | 1/1 | n/a | n/a | – | – | – | – | – | n/a | n/a | – | – | – | – | n/a | n/a | n/a | – |
| ore-2015 | partial | 0/2 ! | · | · | – | – | – | – | – | – | – | 1/2 | 0/2 ! | 0/2 ! | 1/2 | – | · | – | – |
| w3c-owl2-dl | ready | · | · | · | – | – | – | – | – | – | – | 0/1 ! | 0/1 ! | 0/1 ! | 0/1 ! | – | · | – | – |
| owl2bench-classify | planned | · | · | · | – | – | – | – | – | – | – | · | · | · | · | – | · | – | – |
| basics-mix | ready | 5/5 | · | 4/5 | 5/5 | 4/5 ! | 3/5 ! | 5/5 | · | – | – | – | – | – | – | 5/5 | · | n/a | · |
| sparqloscope | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | · | · | n/a | · |
| bsbm | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | · | · | n/a | · |
| watdiv | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | · | · | n/a | · |
| write-scaling | ready | 0/2 ! | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | n/a | n/a | n/a | n/a |
| w3c-sparql11 | ready | 1/1 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | n/a | n/a | n/a | n/a |
| w3c-shacl | ready | 1/1 | n/a | n/a | – | – | – | n/a | n/a | – | – | – | – | – | – | n/a | n/a | n/a | – |
| era-shacl | planned | · | · | · | – | – | – | · | · | – | – | – | – | – | – | · | · | n/a | – |
| fulltext-search | planned | · | · | · | · | – | · | · | · | – | – | – | – | – | – | · | · | n/a | – |
| geosparql | partial | 1/1 | n/a | n/a | n/a | – | n/a | n/a | n/a | – | – | – | – | – | – | n/a | n/a | n/a | – |
| federation | planned | · | · | · | · | · | · | · | · | – | – | – | – | – | – | · | · | n/a | · |
| clients | ready | · | n/a | n/a | n/a | n/a | n/a | n/a | n/a | – | – | – | – | – | – | n/a | n/a | n/a | n/a |

`ok` every step and query ok · `part` some queries not ok · `WRONG` an answer count differs from the expected one · `FAIL`/`T/O` the load failed or timed out · `skip` the suite skipped it (reason in the record) · `ran*` ran; a licensed system whose outcome stays local · `·` can run, never run · `–` lacks a capability · `n/a` no adapter or kit for it

## Workloads, tier by tier

### lubm: LUBM: universities, 14 queries

ready, performance; tasks RT1, RT3, RT2, RT5; kit `benches/reasoning` (suite driver); checked by: inferred set against owlrl or Nemo; the published answers for LUBM(1).

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|
| 1 | ok 10-03 | · | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | ran* 10-02 | skip 10-02 |
| 10 | ok 10-03 | · | ok 10-02 | part 10-02 | ok 10-02 | skip 10-02 | ran* 10-02 | skip 10-02 |
| 100 | ok 10-03 | · | ok 10-02 | · | ok 10-02 | skip 10-02 | ran* 10-02 | skip 10-02 |
| 1000 | · | · | · | · | · | · | · | · |

### lubm-materialised: LUBM with the closure precomputed: the 14 queries on every SPARQL engine

ready, performance; tasks RT5; kit `benches/reasoning` (suite driver); checked by: row counts across systems; the published answers for LUBM(1).

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | ok 10-03 | · | · | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | skip 10-02 | ran* 10-02 | · | · |
| 10 | ok 10-03 | · | · | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | skip 10-02 | ran* 10-02 | · | · |
| 100 | ok 10-03 | · | · | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | skip 10-02 | ran* 10-02 | · | · |

### owl2bench: OWL2Bench: one TBox per OWL 2 profile, 22 queries

ready, performance; tasks RT1, RT3; kit `benches/reasoning` (suite driver); checked by: inferred set against owlrl or Nemo; answer counts across systems.

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | nemo | owlrl | graphdb | rdfox |
|---|---|---|---|---|---|---|---|---|
| rl-1 | ok 10-03 | · | ok 10-02 | · | ok 10-02 | T/O 10-02 | ran* 10-02 | skip 10-02 |
| ql-1 | ok 10-03 | · | · | · | · | · | · | · |

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

| Tier | nrese | nrese-fsst | nrese-oxigraph | jena | rdf4j | nemo | owlrl | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|
| dbpedia-core | · | · | · | · | · | · | · | · | · | · |
| yago-tiny | · | · | · | · | · | · | · | · | · | · |

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
| - | ok 10-03 |

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
| - | · | · | · | part 10-03 | WRONG 10-03 | WRONG 10-03 | part 10-03 | · |

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
| olympics | ok 10-03 | · | ok 10-02 | ok 10-02 | part 10-02 | part 10-02 | ok 10-02 | · | ran* 10-02 | skip 10-02 | · |
| yago-tiny | ok 10-03 | · | skip 10-02 | ok 10-02 | ok 10-02 | part 10-02 | ok 10-02 | · | ran* 10-02 | skip 10-02 | · |
| entities-10000000 | ok 10-02 | · | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | · | ran* 10-02 | skip 10-02 | · |
| dbpedia-core | ok 10-03 | · | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | · | ran* 10-02 | skip 10-02 | · |
| wikidata-lexemes-60m | ok 10-03 | · | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | ok 10-02 | · | ran* 10-02 | skip 10-02 | · |

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

| Tier | nrese |
|---|---|
| 1m | FAIL 10-03 |
| 10m | · |

### w3c-sparql11: W3C SPARQL 1.1 test suite: query, update, syntax, result formats

ready, conformance; tasks basics; kit `crates/nrese-sparql/tests/w3c_sparql11` (suite driver); checked by: the suite's expected results.

| Tier | nrese |
|---|---|
| - | ok 10-03 |

### w3c-shacl: W3C SHACL test suite

ready, conformance; tasks validation; kit `crates/nrese-shacl/tests` (suite driver); checked by: the suite's expected reports.

| Tier | nrese |
|---|---|
| - | ok 10-03 |

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
| compliance | ok 10-03 |

### federation: FedBench / W3C federation tests

planned, performance; tasks federation; kit `-` (outside the driver); checked by: answers against the single-store run.

| Tier | nrese | nrese-fsst | nrese-oxigraph | qlever | oxigraph | virtuoso | jena | rdf4j | graphdb | rdfox | anzograph |
|---|---|---|---|---|---|---|---|---|---|---|---|
| - | · | · | · | · | · | · | · | · | · | · | · |

### clients: ResearchSpace on the store; the Datamodel Workflow's export protocol

ready, conformance; tasks integration; kit `scripts` (suite driver); checked by: the scripts' assertions.

| Tier | nrese |
|---|---|
| - | · |

## Gaps

### Pairs that can run and have no outcome yet

- **lubm:** nrese (1000); jena (100, 1000); nemo (1000); owlrl (10, 100, 1000); graphdb (1000); rdfox (1, 10, 100, 1000)
- **lubm-materialised:** rdf4j (1, 10, 100); rdfox (1, 10, 100); anzograph (1, 10, 100)
- **owl2bench:** jena (rl-1, ql-1); nemo (ql-1); owlrl (ql-1); graphdb (ql-1); rdfox (rl-1, ql-1)
- **ldbc-spb:** nrese (50m, 256m); jena (50m, 256m); rdf4j (50m, 256m); graphdb (50m, 256m); rdfox (50m, 256m); anzograph (50m, 256m)
- **integration-rg-gs-gnd:** nrese (example, cohort, full); jena (example, cohort, full); graphdb (example, cohort, full); rdfox (example, cohort, full)
- **real-ontologies:** nrese (dbpedia-core, yago-tiny); jena (dbpedia-core, yago-tiny); rdf4j (dbpedia-core, yago-tiny); nemo (dbpedia-core, yago-tiny); owlrl (dbpedia-core, yago-tiny); graphdb (dbpedia-core, yago-tiny); rdfox (dbpedia-core, yago-tiny); anzograph (dbpedia-core, yago-tiny)
- **consistency:** nrese (lubm-1, owl2bench-rl-1); graphdb (lubm-1, owl2bench-rl-1); rdfox (lubm-1, owl2bench-rl-1)
- **ore-2015:** nrese (full); hermit (full); openllet (full); konclude (full); elk (full); rdfox (dev, full)
- **w3c-owl2-dl:** nrese (-); rdfox (-)
- **owl2bench-classify:** nrese (el-1, dl-1, dl-10); hermit (el-1, dl-1, dl-10); openllet (el-1, dl-1, dl-10); konclude (el-1, dl-1, dl-10); elk (el-1, dl-1, dl-10); rdfox (el-1, dl-1, dl-10)
- **basics-mix:** rdf4j (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); rdfox (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m); anzograph (olympics, yago-tiny, entities-10000000, dbpedia-core, wikidata-lexemes-60m)
- **sparqloscope:** nrese (dblp); qlever (dblp); oxigraph (dblp); virtuoso (dblp); jena (dblp); rdf4j (dblp); graphdb (dblp); rdfox (dblp); anzograph (dblp)
- **bsbm:** nrese (10m, 100m); qlever (10m, 100m); oxigraph (10m, 100m); virtuoso (10m, 100m); jena (10m, 100m); rdf4j (10m, 100m); graphdb (10m, 100m); rdfox (10m, 100m); anzograph (10m, 100m)
- **watdiv:** nrese (10m, 100m); qlever (10m, 100m); oxigraph (10m, 100m); virtuoso (10m, 100m); jena (10m, 100m); rdf4j (10m, 100m); graphdb (10m, 100m); rdfox (10m, 100m); anzograph (10m, 100m)
- **write-scaling:** nrese (10m)
- **era-shacl:** nrese (-); jena (-); rdf4j (-); graphdb (-); rdfox (-)
- **fulltext-search:** nrese (-); qlever (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-); rdfox (-)
- **federation:** nrese (-); qlever (-); oxigraph (-); virtuoso (-); jena (-); rdf4j (-); graphdb (-); rdfox (-); anzograph (-)
- **clients:** nrese (-)

### Build variants without an outcome (nrese-fsst, nrese-oxigraph; optional)

- **lubm:** nrese-fsst (1, 10, 100, 1000); nrese-oxigraph (1000)
- **lubm-materialised:** nrese-fsst (1, 10, 100); nrese-oxigraph (1, 10, 100)
- **owl2bench:** nrese-fsst (rl-1, ql-1); nrese-oxigraph (ql-1)
- **ldbc-spb:** nrese-fsst (50m, 256m); nrese-oxigraph (50m, 256m)
- **integration-rg-gs-gnd:** nrese-fsst (example, cohort, full); nrese-oxigraph (example, cohort, full)
- **real-ontologies:** nrese-fsst (dbpedia-core, yago-tiny); nrese-oxigraph (dbpedia-core, yago-tiny)
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

### Problems on the newest run

- basics-mix olympics on oxigraph: partial (2026-10-02): not ok: q09-no-medal
- basics-mix olympics on virtuoso: partial (2026-10-02): not ok: q08-event-hierarchy
- basics-mix yago-tiny on virtuoso: partial (2026-10-02-yago): not ok: q01-label-lookup, q03-taxon-path, q08-subclass-path
- lubm 10 on jena: partial (2026-10-02): not ok: q01, q02, q03, q04, q05, q06, q07, q08, q09, q10, q11, q12, q13, q14
- ore-2015 dev on konclude: partial (2026-10-03-dl-reference): 1 error
- ore-2015 dev on nrese: partial (2026-10-03-dl-reference): EL track only (20/20 agree); the DL tasks wait for phase 3
- ore-2015 dev on openllet: partial (2026-10-03-dl-reference): 3 timeouts; differs on ore_ont_1340 (open)
- owl2bench rl-1 on owlrl: timeout (2026-10-02)
- w3c-owl2-dl - on elk: partial (2026-10-03-dl-reference): EL tasks only; the rest use constructs outside its fragment
- w3c-owl2-dl - on hermit: partial (2026-10-03-dl-reference): 27 timeouts, 1 crash (WebOnt-Thing-003), no wrong result
- w3c-owl2-dl - on konclude: wrong (2026-10-03-dl-reference): 30 wrong results, 100 unsupported (datatypes, keys)
- w3c-owl2-dl - on openllet: wrong (2026-10-03-dl-reference): 6 wrong results, 10 timeouts
- write-scaling 1m on nrese: failed (2026-10-03-nrese-current)

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

## Runs

Newest first; the records are in [runs/](runs/).

| Run | Started | Kit | Host | Commit | Pairs | Purpose |
|---|---|---|---|---|---|---|
| [2026-10-03-nrese-current](runs/2026-10-03-nrese-current.toml) | 2026-10-03T12:20 | suite | Kraris-GPTler | b963708 +  | 17 | Regression check before the milestone: NRESE at the head of 3 October on the standard tiers, plus OWL2Bench QL-1 and write scaling 1m |
| [2026-10-03-dl-reference](runs/2026-10-03-dl-reference.toml) | 2026-10-03 | reasoning/dl | Kraris-GPTler | 00f9b19 (kit); c0b5e5c (NRESE EL rerun) | 9 | Work package 2.3: the reference reasoners on the W3C OWL 2 DL suite and the ORE 2015 development subset; NRESE's EL classifier on the EL track |
| [2026-10-02-yago-head](runs/2026-10-02-yago-head.toml) | 2026-10-02T08:44 | suite | Kraris-GPTler | unknown (before run manifests) | 1 | NRESE on YAGO tiny after the fixes of 2 October |
| [2026-10-02-yago](runs/2026-10-02-yago.toml) | 2026-10-02T06:58 | suite | Kraris-GPTler | unknown (before run manifests) | 6 | YAGO tiny on every system (its data was missing in 2026-10-02) |
| [2026-10-02-lubm-materialised](runs/2026-10-02-lubm-materialised.toml) | 2026-10-02T06:11 | suite | Kraris-GPTler | unknown (before run manifests) | 18 | LUBM with the closure precomputed: the query engines without a reasoner on their own turf |
| [2026-10-02-nrese-current](runs/2026-10-02-nrese-current.toml) | 2026-10-02T06:03 | suite | Kraris-GPTler | unknown (before run manifests) | 9 | NRESE at the head of 2 October on the standard tiers (one run): the baseline for the regression checks |
| [2026-10-02](runs/2026-10-02.toml) | 2026-10-02T00:06 | suite | Kraris-GPTler | unknown (before run manifests) | 48 | The comparison batch: the basics mix on five datasets, LUBM 1/10/100 and OWL2Bench RL-1 on every system with an adapter (YAGO tiny's data was missing: rerun in 2026-10-02-yago) |
| [2026-10-01](runs/2026-10-01.toml) | 2026-10-01T19:44 | suite | Kraris-GPTler | unknown (before run manifests) | 15 | First full suite run: LUBM, OWL2Bench RL and the olympics mix on every system with an adapter; NRESE on Oxigraph as the pre-migration baseline |
