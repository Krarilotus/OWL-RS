# The benchmark catalogue: what exists, what it costs, what we run

The field of benchmarks per category of NRESE's goals, each with its cost on our machines,
and the choice of which ones to run. It answers three questions:
- what each benchmark measures, who publishes on it, and what it would show about NRESE;
- what it costs: licence, data on disk and in memory, setup, runtime per system;
- which ones we run: **core** (behind a paper headline), **representative** (the most
  important one of its category; every category has at least one), or **later**.

The registries ([suite/workloads.toml](suite/workloads.toml),
[suite/systems.toml](suite/systems.toml)) hold what is chosen and how it runs; this file
holds the reasons and the costs. The rules every run follows are in
[PROTOCOL.md](PROTOCOL.md). Written 5 October 2026 from the research report of that day
(sections "Benchmarks and systems to add" and "Fair benchmark protocol") and further
search.

**Evidence levels.** Sizes and runtimes marked *measured* come from our runs (the office PC,
3 October: run records office-a and office-b). Everything else is *reported* (the
benchmark's paper or site) or *est.* (estimated from the measured rates below); both are
hypotheses until we measure them (PROTOCOL.md).

## 1. The budget

| Resource | Main PC | Office PC | Draco |
|---|---|---|---|
| Cores, memory | 16 threads; 64 GB, of which Docker's VM has 31 GiB | Ryzen 9 5950X, 31 GiB | cluster nodes (owner's OK per job) |
| Disk for datasets | 277 GB free of 1.9 TB; the volume `nrese-bench-data` holds 63 GB; budget 300 GB (`datasets.toml`), datasets go below 100 GB free | as configured | node-local scratch |
| Use | the fast suite; development tiers; capped runs | queued batches; claim runs on Linux | scale tiers, distribution, claim runs |

**Measured rates** (office PC, Docker, medians of 3, DBpedia core: 67 M statements, 9.2 GB
of N-Triples). Runtime estimates below are statements ÷ load rate, plus the query or
reasoning work where known.

| System | Load | Rate | Store | Per statement | Load peak | Serving peak |
|---|---:|---:|---:|---:|---:|---:|
| NRESE | 20.5 s | 3.3 M/s | 3.7 GB | 56 B | 6.8 GB | 0.5 GB |
| Oxigraph | 41 s | 1.6 M/s | 8.7 GB | 130 B | 17.8 GB | 0.6 GB |
| QLever | 75 s | 0.9 M/s | 2.8 GB | 42 B | 13.5 GB (8 GB sort memory set) | 0.3 GB |
| Virtuoso | 302 s | 0.22 M/s | 6.3 GB | 95 B | 10.6 GB | 6.7 GB (buffers) |
| Jena TDB2 | 737 s | 0.09 M/s | 11.3 GB | 168 B | 15.8 GB | 4.1 GB |
| RDF4J (entities, 10 M) | 125 s | 0.08 M/s | 1.4 GB | 144 B | 7.7 GB | 4.4 GB |

Reasoning (same runs): LUBM 100 load and OWL 2 RL closure, NRESE 6.5 s, Nemo 411 s,
nemo-sparq 127 s; OWL2Bench RL-1, NRESE 0.9 s, Nemo 31 min, owlrl past its limit; LUBM 1000,
NRESE 82 s at a 21.7 GiB peak, nemo-sparq out of memory.

Rules of thumb from these: N-Triples take 100-190 B per statement on disk; NRESE's store
about 56 B, its load about 100 B at peak (less under a bulk-load budget), its OWL 2 RL
materialisation about 100 B per final statement at peak.

## 2. The choice at a glance

| Category | Core | Representative | Covered now |
|---|---|---|---|
| RDF load and storage | basics mix (5 real datasets), entities load curve | | yes: both ready |
| SPARQL, synthetic | Sparqloscope on our datasets | WatDiv | LUBM materialised, basics mix |
| SPARQL, real logs | | WDBench (Wikidata, from logs): later, on Draco | basics mix's hand-written mixes only |
| Updates, writes under readers | LDBC SPB; commit latency under 0-64 readers (ours) | BSBM explore and update | write scaling, concurrency scorecard, soak |
| Rule reasoning | LUBM, OWL2Bench RL | real ontologies (DBpedia, YAGO); Datalog graph programs (TC, SG, CSPA) | LUBM, OWL2Bench (QL wrong today) |
| Incremental maintenance | SSPE, Claros (clique-targeted), LUBM batches | | commit latency on LUBM (`local-bulk.sh`) only |
| OWL 2 DL | ORE 2015 | OWL2Bench EL/DL classification; PAGOdA's test suite (query answering) | W3C DL suite, ORE dev subset, random EL |
| OWL 2 QL / OBDA | | NPD; LUBM∃ | OWL2Bench QL |
| SHACL | | ERA-SHACL | W3C suite |
| GeoSPARQL | | Geographica 2 | compliance suite |
| Full-text | | LUBMft; BEIR (small sets) | string filters in the basics mix |
| RDF 1.2 / RDF-star | | StarBench | W3C RDF 1.2 and SPARQL 1.2 suites |
| Federation | | FedShop | W3C federated query tests |
| Property graphs | | LDBC SNB Interactive v2; openCypher TCK; LDBC Graphalytics | none (G8 not built) |
| Vector search | | VIBE; big-ann filtered track; ann-benchmarks | `vector_search` example only |
| GraphRAG, text-to-SPARQL | | GraphRAG-Bench; data.world chat-with-data; QALD-10 | none |
| KG construction | | Text2KGBench; GTFS-Madrid-Bench / KGCW challenge | none |
| Concurrency and soak | commit latency under readers (as above) | SPB's concurrent mode; BSBM multi-client | scorecard, `soak.py` |
| Larger than memory | the memory-budget series (16 to 2 GB) on LUBM 1000 | WDBench under a cap | `image-cap-check.sh`, bulk-load budget |
| Distribution | | LUBM 8000 and WatDiv 1 B on Draco | none (G6 not built) |

The fast suite (§6) takes one small, generated case per bottleneck from this table; it
doesn't replace these benchmarks.

## 3. Benchmarks by category

Each category has two tables: **what and why** (what it measures, who publishes on it, the
check, what it shows about NRESE, the mark) and **cost** (licence, data, setup, runtime,
systems). "Setup" is the days to a first checked run of NRESE and two free systems, given
the suite's driver.

### 3.1 RDF load and storage

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| Basics mix (olympics, YAGO tiny, DBpedia core, Wikidata lexemes 60 M, entities 10 M) | load, store size, restart, a hand-written query mix on real data; ours | row counts across systems | the store's size and serving memory against QLever's, where QLever leads (performance.md §1) | core | ready, run on 3 Oct |
| Entities load curve (harness `generate`, 1 M to 1 B) | load throughput and memory as data grows; ours | statements read back | where the load stops scaling with cores (P5) and when it spills | core | 10 M ready; curve not run |
| Wikidata truthy, full | load and queries at about 8 B statements; QLever, MillenniumDB, Virtuoso publish | row counts | whether the store holds Wikidata on one node | later (Draco) | – |

| Benchmark | Licence, redistribution | Data: disk / NRESE RAM | Setup | Runtime (est. unless measured) | Systems |
|---|---|---|---|---|---|
| Basics mix | CC-BY-SA (DBpedia), CC-BY (YAGO), CC0 (Wikidata), olympics CC-BY-SA; we don't redistribute, `prepare-datasets.sh` fetches | 22 GB N-Triples in the volume; stores 0.1-3.7 GB; load peaks to 6.8 GB (measured) | 0 | DBpedia core per system above; a full basics run of 6 systems × 5 tiers × 3 runs about 6 h (measured, office batch) | all SPARQL systems |
| Entities curve | ours | 1 B: 110 GB N-Triples (est.), store 56 GB, budgeted load | 0.5 | NRESE 5 min at 1 B, QLever 20 min, Jena 3 h (est.) | all SPARQL systems |
| Wikidata truthy | CC0 | about 1 TB N-Triples (est.), NRESE store 450 GB (est.): beyond the main PC's disk | 2 | NRESE 45 min load (est.), QLever 3 h (reported: hours), Jena days | QLever, Virtuoso, MillenniumDB, NRESE |

### 3.2 SPARQL query: synthetic and from logs

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| Sparqloscope (ISWC 2025) | about 100 queries generated for *any* dataset, one SPARQL feature each (joins, EXISTS, strings, dates, language filters, aggregates); QLever, Virtuoso, MillenniumDB, GraphDB, Blazegraph, Jena published on DBLP and Wikidata | result sizes across systems | feature by feature, where NRESE's planner has no fast path; it runs on the datasets we already hold | core | planned |
| WatDiv (Waterloo) | query shapes: linear, star, snowflake, complex; 20 templates, incremental linear tests; the standard stress test in RDF papers (gStore, RDF-3X successors, TriAD) | row counts across systems | join ordering and WCOJ choice across shapes, at 10 M to 1 B | representative | planned |
| BSBM (Berlin) | e-commerce explore, BI, explore and update; most vendors published | its qualification mode | see 3.3; BI queries stress aggregates | representative (3.3) | probes `bsbm-http.sh`, `bsbm-bi-lab.sh` |
| SP2Bench | DBLP-shaped data, 17 queries (OPTIONAL, negation, long chains) | its reference counts | OPTIONAL and negation at scale | later | – |
| LUBM materialised | the 14 LUBM queries over a closure every engine loads | published answers (LUBM 1), counts | engines on their own turf | covered | ready |
| WDBench (ISWC 2022) | about 2,650 queries (reported) from Wikidata's logs over 1.2 B direct-property statements: BGPs, OPTIONAL, paths, navigational; Jena, Virtuoso, Blazegraph and Neo4j published (MillenniumDB's authors left their own out) | row counts across systems | real query shapes and path queries at scale | representative (real logs); later, a Draco run, not in the fast suite (owner, 5 Oct 2026) | – |
| FEASIBLE | mixes sampled from real logs (DBpedia, SWDF) by feature coverage | counts | the log mix of a given endpoint, e.g. HisQu's | later | – |
| DBpedia SPARQL Benchmark | the 2011 log-derived DBpedia benchmark | counts | superseded by FEASIBLE and WDBench | later | – |
| LSQ, Wikidata SPARQL logs | query logs as data (sources, not benchmarks) | – | input for FEASIBLE-style mixes; Saleem et al. 2019 justify a mix's features | source | – |
| gMark; BeSEPPI | generated property-path workloads; a property-path correctness and speed suite | the generator's counts; BeSEPPI's expected answers | path evaluation, closures by components | later (fast-suite source) | – |

| Benchmark | Licence, redistribution | Data: disk / NRESE RAM | Setup | Runtime (est.) | Systems |
|---|---|---|---|---|---|
| Sparqloscope | Apache-2.0 | none of its own: our datasets (DBpedia core 9.2 GB); its own tier DBLP, about 500 M statements (to confirm at download) | 1 | DBpedia core: 105 queries × 3 runs, under 10 min per system for the fast engines, an hour or more for Jena; DBLP: NRESE load 3 min, QLever 10 min | all SPARQL systems |
| WatDiv | no licence stated (generator source on GitHub); data generated, not redistributed; internal use only: ask the authors before publishing (owner, 5 Oct 2026) | 100 M: 15 GB N-Triples, 5.6 GB store; 1 B on Draco | 1.5 (C++ generator with Boost, in Docker) | 100 M: NRESE load 30 s, QLever 2 min, Jena 20 min; 400 queries: minutes | all SPARQL systems |
| SP2Bench | its site (to check) | generated, 10 M-1 B | 1 | as WatDiv | all |
| WDBench | data CC0 (Wikidata), on Figshare; queries in the repo (no licence stated: to check) | 1.2 B: about 150 GB N-Triples (est.), NRESE store 70 GB; load peak capped by the bulk-load budget | 2 | NRESE load 6-10 min, QLever 25 min, Jena 4 h (est.); queries with a 60 s limit each, about 2 h per system | QLever, Virtuoso, Jena, Oxigraph (slow load), MillenniumDB, NRESE |
| FEASIBLE | AGPL-3.0 (code; we run it, don't ship it) | the log's dataset | 2 | – | all |
| gMark, BeSEPPI | to check | small, generated | 1 each | seconds to minutes | all |

### 3.3 Updates and writes under readers

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| LDBC SPB 2.0 (BBC) | editorial updates under an RDFS-to-OWL 2 RL ontology with concurrent aggregation agents; GraphDB publishes audited results | the official validator (query results and inference) | per-commit maintenance under readers against GraphDB's design; also needs full-text and GeoSPARQL in the advanced mix | core | planned |
| Commit latency under readers (ours, report item 20) | commit p50/p99 under 0, 1, 8, 64 readers with the closure complete at every commit; nobody publishes it | committed state equals rematerialisation | the claim of the ESWC paper | core | partial: `local-bulk.sh` (no readers) |
| BSBM explore and update | the explore mix with updates interleaved, multi-client | qualification mode | write throughput without reasoning against others | representative | probes only |
| Write scaling | single and batch writes at 1 M, 10 M | statements read back | P1 gate (< 5 ms at 10 M) | covered | ready |
| Concurrency scorecard | throughput with concurrent clients, writes under read load | counts | | covered | `competitors/scorecard.sh` |
| BEAR; SPBv | versioned graphs: queries at a revision, deltas | the archives' answers | G12 history | later | – |

| Benchmark | Licence, redistribution | Data: disk / NRESE RAM | Setup | Runtime (est.) | Systems |
|---|---|---|---|---|---|
| LDBC SPB | Apache-2.0; unaudited results must say "not an LDBC benchmark result" (LDBC fair-use policy) | 50 M: 7 GB, store 3 GB; 256 M: 36 GB, store 14 GB | 4 (Java generator and driver, validator, adapters) | 50 M: generation 30 min, load and reasoning minutes, a run 20-60 min per system | NRESE, GraphDB, RDFox, Jena (rules), RDF4J (RDFS), Stardog |
| Commit under readers | ours | LUBM 10 and 100 (in the volume) | 2 (harness mode with readers) | 10 min per point; a curve 1 h per system | systems with incremental reasoning: NRESE, GraphDB, RDFox; without reasoning: all |
| BSBM | Apache-2.0 | 100 M: 25 GB N-Triples, store 6 GB | 1.5 (generator and driver in Docker) | 100 M: load as WatDiv; 30 min per system per mix | all SPARQL systems |
| BEAR | to check | 30 M to 1 B versions | 2 | – | NRESE once revisions are queryable |

### 3.4 Rule reasoning and incremental maintenance

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| LUBM | materialisation and 14 queries; RDFox, GraphDB, VLog, Nemo, RDFox's papers | inferred set against owlrl or Nemo; published answers (LUBM 1) | the headline speed and memory (item 1-6 of the report) | core | ready, 1 to 1000 |
| OWL2Bench RL, QL | one TBox per profile, 22 queries; ISWC 2020; extends UOBM | inferred set; counts across systems | symmetric-transitive cliques (RL-1); QL's existentials (wrong today) | core | ready; QL-1 WRONG |
| UOBM | LUBM with richer OWL (cycles, sameAs) | counts | covered by OWL2Bench | later | – |
| Real ontologies: DBpedia and YAGO with their schemas | materialisation over large real hierarchies | inferred set against Nemo | whether the 96 % schema share of LUBM holds (merge checklist item 1) | representative | planned |
| Claros (L, LE), Reactome, UniProt | RDFox's and GLog's datasets: Claros-LE's rules build huge cliques | inferred counts across systems | the worst case for proof-based deletion; GLog's single-thread result (Claros-L 119 s vs RDFox 2,512 s, reported) | core (Claros) | – |
| SSPE (Hu, Motik, Horrocks) | single-source path graphs where one deletion invalidates long derivations; counting beat B/F (10.5 s vs 253 s for 1,000 deletions, reported) | incremental equals rematerialised | whether proofs or counts should handle a deletion (report item 9) | core | – |
| LUBM insert and delete batches (RT2) | 1, 100, 10 k ABox changes and TBox changes after the closure | incremental equals rematerialised | the common case | core | commit latency only |
| Datalog graph programs: TC, same generation, CSPA | recursive joins on graphs; RecStep, FlowLog, DCDatalog, Soufflé papers | counts against Soufflé or Nemo | the batch executor beyond OWL: plain Datalog in N3 rules | representative | – |
| ChaseBench | existential rules (Doctors, STB-128, ONT-256); Nemo, VLog, RDFox | counts across chase engines | only if existential rules come; NRESE has none | later | – |
| Injected contradictions (RT4) | detection and explanation of each contradiction kind | all found, none on clean data | the consistency gate and explanations | representative | planned |

| Benchmark | Licence, redistribution | Data: disk / NRESE RAM | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| LUBM | generator GPL-2.0; data generated | 1000: 24.7 GB N-Triples, 21.7 GiB peak, 5.3 GB store (measured) | 0 | 100: NRESE 6.5 s, Nemo 411 s; 1000: NRESE 82 s (measured) | NRESE, Nemo, owlrl, Jena, GraphDB, RDFox, RDF4J (RDFS) |
| OWL2Bench | generator Apache-2.0, queries CC-BY-4.0 | RL-1: 7.7 MB, 1.0 GiB serving (measured, an anomaly, P2) | 0 | RL-1: NRESE 0.9 s, Nemo 31 min (measured) | as LUBM |
| Real ontologies | as the basics mix | DBpedia core + ontology: 9.2 GB; closure est. 100-150 M statements, 15 GB peak | 1 | NRESE minutes; Nemo hours (est. from LUBM) | NRESE, Nemo, Jena, GraphDB, RDFox |
| Claros, Reactome, UniProt | Claros: research data from RDFox's test sets, licence unstated (ask Oxford); Reactome CC-BY 4.0; UniProt CC-BY 4.0 | Claros: about 19 M facts in (reported); LE's closure is many times that: size it on Draco first | 2 | Claros-L minutes; LE hours | NRESE, Nemo, VLog, RDFox, GraphDB |
| SSPE | ours to generate, from the paper's description | small: millions of edges | 1 | seconds to minutes per batch | NRESE, RDFox, GraphDB |
| TC, SG, CSPA | ours (generated graphs); SNAP graphs where needed (licences per graph) | 1-100 M edges, closures to billions: capped | 1 | seconds to minutes | NRESE, Nemo, Soufflé (UPL-1.0), VLog |
| ChaseBench | no licence stated; internal use only: ask the authors before publishing (owner, 5 Oct 2026) | to 1 GB | 3 | – | Nemo, VLog, RDFox |

### 3.5 OWL 2 DL: consistency, classification, realisation

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| W3C OWL 2 DL tests | 411 tasks of 311 tests | the test types; 2009 published results; `disputed.tsv` | correctness (403 right, 0 wrong on 5 Oct) | covered | ready |
| ORE 2015 | classification, consistency, realisation of 1,920 real ontologies × 3 tasks; Konclude, HermiT, ELK, FaCT++, Pellet and others published | canonical taxonomies by hash across reference reasoners; adjudication | the DL engine against Konclude and HermiT; the paper's DL section | core | dev subset (80 + 100 tasks) |
| OWL2Bench EL and DL | classification and consistency at 1 and 10 universities (ABox-heavy); Konclude times out on DL (reported) | taxonomies across reasoners | realisation at ABox scale | representative | planned |
| PAGOdA's test suite (LUBM, UOBM, FLY, NPD, DBpedia+, ChEMBL, Reactome, UniProt with queries) | query answering beyond RL by bounds; PAGOdA, HermiT, Konclude published | answers against a complete reasoner | the bounds made store state (report item 7, paper contribution 3) | representative | – |
| Release-pinned real ontologies: GO, ChEBI, NCIt, GALEN; SNOMED CT | classification of large EL and near-EL ontologies; ELK, Snorocket, Konclude | taxonomies against ELK and Konclude | EL speed against ELK | later (SNOMED internal only) | random EL kit |

| Benchmark | Licence, redistribution | Data: disk / RAM | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| W3C DL | W3C | small | 0 | minutes (main PC, 20 s budget per test) | NRESE, HermiT, Openllet, Konclude |
| ORE 2015 | per ontology (Zenodo corpus; we fetch, don't ship) | about 5 GB corpus (reported); up to a few GB RAM per task | 0 (kit), Draco for full | full: 1,920 × 3 tasks × 1,800 s worst case: Draco only; dev: under 1 h per reasoner (measured) | NRESE, HermiT, Openllet, Konclude, ELK |
| OWL2Bench EL/DL | as above | DL-10: tens of MB | 0.5 | Konclude past the limit (reported) | as above |
| PAGOdA suite | per dataset (PAGOdA's site); PAGOdA's own code: to check | to 10 M facts | 2 | minutes to hours | NRESE, PAGOdA, HermiT |
| GO, ChEBI, NCIt, GALEN; SNOMED | CC-BY 4.0 (GO, ChEBI); NCIt CC-BY 4.0; GALEN free; SNOMED CT affiliate licence: never redistributed, results internal | 50-500 MB | 1 | seconds (ELK) to minutes | as above |

### 3.6 OWL 2 QL and OBDA

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| NPD | 30 real user queries over the Norwegian petroleum directorate's data and a QL ontology; Ontop, Stardog | certain answers across QL systems | QL rewriting over the RL closure (report item 7); an RDF variant labelled as such | representative | – |
| LUBM∃ (LUBM with existential axioms) | the tree-witness papers' benchmark | answers against a DL reasoner | that the existential part stays one or two CQs per query | representative | – |
| OWL2Bench QL | as 3.4 | | | covered | WRONG today |
| BSBM-OBDA, GTFS-Madrid-Bench (virtual) | SQL-backed virtual graphs | | G10 virtual graphs | later (3.15) | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| NPD | Apache-2.0 (kit); data NLOD (Norwegian open licence) | NPD ×1 to ×1500 via VIG (reported), 1-100 GB in SQL | 3 (PostgreSQL, mappings; an RDF dump for NRESE) | minutes per scale | Ontop (Apache-2.0), Stardog, GraphDB, NRESE (materialised RDF variant) |
| LUBM∃ | from the papers; data as LUBM | as LUBM | 1 | as LUBM | NRESE, Ontop, Stardog, PAGOdA |

### 3.7 SHACL

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| W3C SHACL tests | Core and SHACL-SPARQL conformance | expected reports | 98/98, 22/22 | covered | ready |
| ERA-SHACL (2025) | validation time and memory on the European railway KG: 55 M statements in three sizes, three shape sets (Core and SPARQL); engines run in memory, each in its own Docker image (the repository's `engines/`) | validation reports across engines | validation at scale against the Java engines | representative | planned |
| LUBM with generated shapes (Trav-SHACL's design) | scalable synthetic validation | reports across engines | shape scheduling, targets by index | later (fast-suite source) | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| ERA-SHACL | none stated in the repository (ERA's data terms); internal use only: ask the authors before publishing (owner, 5 Oct 2026) | 55 M statements: 8 GB N-Triples (est.), NRESE store 3 GB | 1.5 | minutes per engine for NRESE and RDF4J; pySHACL hours (reported scale) | NRESE, Jena, RDF4J, TopBraid SHACL, pySHACL |

### 3.8 GeoSPARQL

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| GeoSPARQL compliance benchmark | 206 queries, conformance | expected answers | 194 of 212 match, 18 disputed (5 Oct) | covered | ready |
| Geographica 2 | spatial selections, joins, aggregates on real data (OSM, CORINE land cover, GADM) and a synthetic scalable set; Strabon, uSeekM, Parliament and a proprietary store published | answers across systems | the spatial index and joins | representative | planned |
| OSM extracts via osm2rdf | QLever's geo queries on country-sized OpenStreetMap | counts across systems | large geometry sets against QLever, which is strong here | later | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| Geographica 2 | not stated (Mercurial repo at strabon.di.uoa.gr); internal use only: ask the authors before publishing (owner, 5 Oct 2026) | real set a few GB; synthetic scalable | 2 (Java runner) | minutes to an hour per system | NRESE, Jena (GeoSPARQL), RDF4J, QLever (subset), GraphDB, Stardog |
| osm2rdf extracts | ODbL (redistribution with attribution, share-alike) | a country: 100 M-1 B statements | 1 | as WatDiv by size | NRESE, QLever, Virtuoso |

### 3.9 Full-text search

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| LUBMft | keyword search combined with structure, on LUBM with generated text | result sets across systems | text index joined with patterns | representative | planned |
| BEIR, small sets (SciFact, NFCorpus, FiQA) | ranking quality (nDCG@10) and speed; every IR paper reports BM25 on it | nDCG against the published BM25 baseline | ranking quality, and hybrid ranking with vectors (3.13) | representative | – |
| ResearchSpace's search queries | the consumer's real keyword queries | result sets | the product case | later | planned |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| LUBMft | none stated (l3s.de); internal use only: ask the authors before publishing (owner, 5 Oct 2026) | as LUBM | 1.5 | minutes | NRESE, Jena (text), Virtuoso, QLever, RDF4J, GraphDB |
| BEIR | Apache-2.0 code; per set (SciFact CC-BY-NC, NFCorpus and FiQA see BEIR's table: internal results until checked) | 5 k-60 k documents: MBs | 1 | seconds per set | NRESE, Lucene/Pyserini BM25 as reference |

### 3.10 RDF 1.2 and RDF-star

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| W3C RDF 1.2 and SPARQL 1.2 tests | conformance | expected results | all pass | covered | ready |
| StarBench (QuWeDa 2023) | 56 SPARQL-star queries on annotated data; Jena, Oxigraph, Stardog and GraphDB published (AnzoGraph and Blazegraph failed to load) | counts across systems | triple-term ids and annotation lookups (report item 11) | representative | – |
| The REF benchmark (Orlandi et al. 2021) | reification, singleton properties and RDF-star compared on BKR (biomedical) | counts | which representation queries fastest in NRESE | later | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| StarBench | data CC-BY 4.0, code Apache-2.0 | about 61 M statements (BKR, as the report has it; to confirm) | 1.5 (RDF-star to RDF 1.2 syntax) | minutes per system | NRESE, Jena, Oxigraph, GraphDB, Stardog |

### 3.11 Federation

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| W3C federated query tests | SERVICE conformance | expected results | | covered | ready |
| FedShop (ISWC 2023) | federation as members grow (20 to 200 BSBM-like shops); FedX, CostFed, Semagrow, HeFQUIN, FedUP | answers against the single-store run | source selection and bind joins over SERVICE | representative | planned |
| FedBench; LargeRDFBench | the classic federation sets (9 and 13 datasets, LargeRDFBench to 1 B) | answers | | later | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| FedShop | GPL-3.0 (run, not shipped) | 200 members: a few GB generated | 3 (many endpoints: one NRESE with many repositories, or many containers under one cap) | hours for the full grid; the 20-member point in minutes | NRESE, Jena, Virtuoso, the federation engines |
| LargeRDFBench | AGPL-3.0 | 1 B statements | 3 | hours | – |

### 3.12 Property graphs (G8)

NRESE has no GQL or openCypher yet; these wait for G8, except that SNB's data loads as RDF
today and its interactive reads can be written in SPARQL (labelled as a SPARQL variant).

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| LDBC SNB Interactive v2 | transactional reads and updates on a social network; audited results (TuGraph, TigerGraph and others) | the reference answers of the validation set | the property-graph view over RDF 1.2 against native engines (report item 15) | representative | – |
| openCypher TCK | conformance of the Cypher subset | the TCK's expected results | the subset we claim | representative | – |
| LDBC Graphalytics | BFS, PageRank, WCC, CDLP, LCC, SSSP; audited results | reference outputs | graph algorithms (G8) | representative | – |
| GAP benchmark suite | the same kernels on generated Kronecker and uniform graphs; the standard kernel reference | its verifiers | a fast source for algorithm cases | later (fast-suite source) | – |
| LDBC SNB BI; FinBench | analytical graph queries; financial transactions | reference answers | | later | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| SNB Interactive v2 | Apache-2.0; the LDBC fair-use rule (unaudited = "not an LDBC benchmark result") | SF1 1 GB CSV (about 30 M edges), SF10 10 GB, SF100 on Draco | 5 (datagen in Spark, driver, a SPARQL or GQL implementation) | SF10: an hour per system | NRESE, Neo4j Community, Memgraph, DuckPGQ, froGQL; Kùzu archived in Oct 2025 |
| openCypher TCK | Apache-2.0 | none | 1 once a parser exists | minutes | as above |
| Graphalytics | Apache-2.0; datasets per graph | XS to L graphs, 0.1-10 GB | 3 | minutes per algorithm and graph | NRESE, Neo4j GDS (licence check), DuckPGQ, GAP reference |
| GAP | BSD-3-Clause | generated, 2^20-2^26 vertices | 0.5 | seconds | as above |

### 3.13 Vector search

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| VIBE (2025) | recall against throughput on modern embeddings (11 in-distribution, 8 out-of-distribution sets), 22 indexes | recall@k against exact k-NN | the index on today's embeddings, not SIFT | representative | – |
| big-ann-benchmarks, filtered track | k-NN with tag filters (YFCC-10M); NeurIPS'23 results | recall@10 | filtered search: SPARQL filters with k-NN, our own advantage to show | representative | – |
| ann-benchmarks | the long-running recall/QPS curves (SIFT-1M, GloVe, Fashion-MNIST) | recall | comparability with every published index | representative | – |
| Hybrid: BEIR with embeddings | keyword plus vector ranking | nDCG | hybrid search in one query | later (3.9) | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| VIBE | MIT; datasets downloadable, some need a GPU to regenerate | 1 M × 768-1024 floats: 3-4 GB per set | 2 | the authors: under 24 h per dataset for all indexes; one index, one set: under 1 h | NRESE, Qdrant, LanceDB, Milvus, pgvector, DuckDB VSS, Faiss |
| big-ann filtered | MIT; YFCC data CC-BY variants | 10 M × 192 bytes: 2 GB | 2 | an hour per system | as above |
| ann-benchmarks | MIT; set licences vary | 0.1-1 GB | 1 | minutes per set and index | as above |

### 3.14 GraphRAG and text-to-SPARQL question answering (G11)

These measure answer quality, not engine speed. Each run costs LLM calls: they need a pinned
model, temperature 0, a token budget, and the API cost recorded per run.

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| GraphRAG-Bench (Xiang et al. 2025) | when graph retrieval helps RAG, by task type; MS GraphRAG, LightRAG, HippoRAG published | answer accuracy against gold | retrieval through the reasoned graph | representative | – |
| data.world chat-with-data (Sequeda et al.) | text-to-SPARQL on an insurance schema, with and without an ontology (54 → 72 % reported) | execution accuracy | whether NRESE's reasoning lifts LLM accuracy (the ontology effect) | representative | – |
| QALD-10 | text-to-SPARQL over Wikidata, GERBIL-scored | F1 against gold answers | the NL-to-SPARQL path on a public KG | representative | – |
| LC-QuAD 2.0; DBLP-QuAD; Spider4SPARQL | larger text-to-SPARQL sets | gold queries' answers | | later | – |
| MultiHop-RAG; STaRK; CRAG; HippoRAG 2's sets | multi-hop and semi-structured retrieval | accuracy | | later | – |
| Transactional semantic verifier (ours, report's paper idea) | invalid commits, repair iterations, latency of LLM-proposed updates checked by OWL and SHACL before commit | the gold KG | a measurement nobody has | later | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| GraphRAG-Bench | MIT | MBs of text | 3 (an embedding model, a pinned LLM) | hours of LLM calls per configuration | NRESE + LLM, MS GraphRAG, LightRAG, HippoRAG 2, Neo4j GraphRAG |
| chat-with-data | Apache-2.0 | tiny (43 questions, a small schema) | 1 | minutes | NRESE + LLM against the published numbers |
| QALD-10 | MIT | Wikidata endpoint (a Wikidata subset or our truthy load) | 2 | an hour | as above |
| CRAG | CC-BY-NC 4.0: research use, results internal until checked | – | – | – | – |
| MultiHop-RAG | ODC-BY | small | 1 | | |

### 3.15 Knowledge graph construction (G10, G11)

| Benchmark | Measures; who publishes | Check | Shows about NRESE | Mark | Now |
|---|---|---|---|---|---|
| Text2KGBench | LLM extraction of triples conforming to an ontology (Wikidata-TekGen, DBpedia-WebNLG) | precision, recall, ontology conformance | SHACL and OWL checks as the conformance oracle | representative | – |
| GTFS-Madrid-Bench; the KGCW challenge (ESWC) | R2RML/RML mapping engines: materialisation from CSV, SQL, JSON, and virtual KGs | the reference KG | mappings and virtual graphs (G10) | representative | – |
| Re-DocRED; BioRED | document-level relation extraction | F1 | extraction feeding the store | later | – |
| Document revisions (ours) | insert and retract facts as documents change | the gold KG per revision | per-commit maintenance driven by extraction | later | – |

| Benchmark | Licence | Data | Setup | Runtime | Systems |
|---|---|---|---|---|---|
| Text2KGBench | Apache-2.0 | small | 2 (LLM) | hours of LLM calls | NRESE + LLM |
| GTFS-Madrid-Bench | Apache-2.0; generated data under the Madrid transport consortium's licence | scales 1-1000 | 2 | minutes per engine | RMLMapper, Morph-KGC, Ontop (virtual), NRESE once G10 lands |
| Re-DocRED | MIT | small | 1 | | |
| BioRED | NCBI release (US government work; check the terms) | small | 1 | | |

### 3.16 Concurrency and soak

| Benchmark | Measures | Check | Mark | Now |
|---|---|---|---|---|
| Commit latency under 0-64 readers (3.3) | p50/p99 per commit, closure complete each time | equals rematerialisation | core | partial |
| SPB's concurrent mode (3.3) | editorial and aggregation agents together | official validator | core | planned |
| BSBM multi-client | query mixes per hour with 1-64 clients | qualification | representative | probes |
| Soak (`probes/soak.py`) | errors, memory slope, handles over hours | the soak graph holds exactly what was committed | covered | ready |

Costs: our own data (LUBM 10 and 100 in the volume); a curve of 5 reader counts × 10 min
per system; the clients on separate cores (`--cpuset-cpus` on Linux).

### 3.17 Larger than memory

| Benchmark | Measures | Check | Mark | Now |
|---|---|---|---|---|
| Memory-budget series (report item 20): LUBM 1000 load, reasoning and queries under 16, 12, 8, 6, 4, 2 GB caps | the slowdown curve, or where it stops | the same closure and answers as uncapped | core | bulk-load budget measured on DBpedia only (5.47 → 4.02 GB peak, +18 % time) |
| WDBench under a cap | real queries on 1.2 B statements with less RAM than the data | counts | representative; later, on Draco | – |
| Wikidata truthy | 8 B statements on one node | counts | later (Draco) | – |

Costs: LUBM 1000 is in the volume (24.7 GB); six points × (82 s to a few times that) per
run: about 1 h per system and repetition. Only systems with a memory limit of their own
(NRESE's budgets, QLever's sort memory, JVM heaps) degrade; the others are killed at the
cap, which is a result too (`failed`, never a time).

### 3.18 Distribution (G6)

No published protocol exists for distributed maintenance with deletions (report item 12).
The candidates are the scale tiers above, run on Draco once tier 1 (leader reasons,
replicas serve) exists: LUBM 8000 (about 1.1 B asserted, 0.7 B inferred, est.), WatDiv 1 B,
WDBench; replica read modes measured as staleness and latency. Mark: representative (LUBM
8000 and WatDiv 1 B), later; setup 3 days on top of the kits; data 200-300 GB on node-local
scratch.

## 4. Systems

Licensed systems (GraphDB, RDFox, Stardog, AnzoGraph, AllegroGraph) run on our machines
only; their results stay local (`publish = permission`) until the vendor agrees in writing
([competitors/README.md](competitors/README.md)). "Setup" is days to a first checked run
through an adapter.

| System | Licence | Publish | Covers | Setup | Resources, notes | Now |
|---|---|---|---|---|---|---|
| QLever | Apache-2.0 | free | SPARQL, text, geo | 0 | index build needs sort memory (8 GB set); smallest stores measured | in suite |
| Oxigraph 0.5 | MIT/Apache-2.0 | free | SPARQL, RDF 1.2 | 0 | RocksDB; 17.8 GB load peak on DBpedia core | in suite |
| Virtuoso 7 OS | GPL-2.0 | free | SPARQL, text, geo | 0 | 16 GB buffers set | in suite |
| Jena 6.2 (TDB2, Fuseki, rules, SHACL, GeoSPARQL, text) | Apache-2.0 | free | SPARQL, rules, SHACL, geo, text | 0 | 16 GB heap; slowest loader | in suite |
| RDF4J native store | EDL-1.0 | free | SPARQL, RDFS, SHACL | 0 | | in suite |
| Nemo | Apache-2.0 | free | Datalog | 0 | single-threaded (both papers) | in suite |
| owlrl | W3C | free | reference closure | 0 | correctness only | in suite |
| HermiT, Openllet, Konclude, ELK | LGPL-3.0 / Apache-2.0; AGPL-3.0 or commercial (dual); LGPL-3.0 (reported); Apache-2.0 | free | DL | 0 | DL kit image | in kit |
| MillenniumDB | GPL-2.0 | free | SPARQL, paths (WDBench's authors) | 1 | | to add (WDBench) |
| Tentris | Apache-2.0 | free | SPARQL (WCOJ, hypertries) | 1 | in-memory | to add (WatDiv) |
| Ontop | Apache-2.0 | free | OWL 2 QL, OBDA | 2 | needs PostgreSQL | to add (NPD) |
| VLog / GLog | Apache-2.0 | free | Datalog | 1.5 | GLog's single-thread lead (reported) | to add (Claros) |
| Soufflé | UPL-1.0 | free | Datalog | 1 | compiled programs | to add (TC, SG, CSPA) |
| PAGOdA | to check | free | QA beyond RL | 1.5 | JVM | to add |
| Rust rivals: maplib, whelk-rs, horned-owl, froGQL; sparq | Apache-2.0; MIT; LGPL-3.0; MIT; MIT (vendored encoding) | free | SPARQL/Datalog; EL; OWL I/O; GQL | 1 each | | sparq's encoding in Nemo |
| Neo4j Community | GPL-3.0 | free | Cypher | 1 | JVM | to add (SNB) |
| Memgraph | BSL 1.1 and Memgraph Enterprise Licence | check the terms | Cypher | 1 | in-memory | to add |
| FalkorDB | SSPL 1 | check the terms | Cypher | 1 | | later |
| DuckPGQ | MIT | free | SQL/PGQ | 1 | | to add |
| Kùzu | MIT | free | Cypher | – | repository archived 10 Oct 2025 | skip |
| Qdrant, LanceDB, Milvus | Apache-2.0 | free | vectors | 1 each | Milvus needs etcd and MinIO | to add (two of them) |
| pgvector / pgvectorscale | PostgreSQL | free | vectors in SQL | 1 | | to add |
| DuckDB VSS, Faiss, cuVS | MIT; MIT; Apache-2.0 | free | vectors (library baselines) | 0.5 each | cuVS needs the GPU | Faiss to add |
| MS GraphRAG, LightRAG, HippoRAG 2, Neo4j GraphRAG, AWS GraphRAG Toolkit | MIT; MIT; MIT; Apache-2.0; Apache-2.0 | free | GraphRAG stacks | 2 each | LLM costs | later |
| Morph-KGC, RMLMapper | Apache-2.0; MIT | free | KG construction | 1 | | later |
| GraphDB 11.5 | Ontotext Free licence | permission | everything SPARQL, rules, SHACL | 0 (adapter exists) | needs the licence file | in suite |
| RDFox 7.6 | evaluation licence | permission; papers reported to OST | rules, maintenance | 0.5 | no licence yet | adapter, never run |
| Stardog | commercial | permission | QL rewriting, rules | 2 | no adapter | – |
| AnzoGraph 3.5 | CSI EULA | permission | SPARQL, RDFS-plus | 0.5 | 8 GB RAM unregistered | scorecard only |
| AllegroGraph | commercial (free tier, 5 M statements) | permission | SPARQL, rules | 2 | the free tier caps size | – |
| Oracle RDF, Amazon Neptune, Spanner Graph | commercial cloud | permission | SPARQL, openCypher, GQL | 3 | cloud cost per hour; never on our machines | later |

## 5. What it costs in total

| Set | Setup | Disk (new) | One full run on the main PC or office PC |
|---|---|---|---|
| Already ready (basics, LUBM, OWL2Bench, write scaling, DL kit) | 0 | 0 (63 GB present) | about 8-10 h for all systems, 3 repetitions |
| Core additions: Sparqloscope, SPB, commit under readers, SSPE, Claros-L, LUBM batches, memory-budget series | 12 days | 60 GB (SPB 256 M, Claros) | about 10 h |
| Representatives without a G8/G10/G11 dependency: WatDiv, WDBench, BSBM, real ontologies, TC/SG/CSPA, OWL2Bench EL/DL, PAGOdA, NPD, LUBM∃, ERA-SHACL, Geographica 2, LUBMft, BEIR, StarBench, FedShop, VIBE, big-ann filtered, ann-benchmarks | 30 days | 230 GB (WDBench 150, WatDiv 15, BSBM 25, rest small): above the 300 GB budget with the present 63 GB, so WDBench lives on Draco or replaces Wikidata lexemes | 20-30 h |
| Representatives waiting on capabilities: SNB, openCypher TCK, Graphalytics (G8); GTFS/KGCW (G10); GraphRAG-Bench, chat-with-data, QALD-10, Text2KGBench (G11) | 20 days | 20 GB | plus LLM costs |

## 6. The fast suite (built; benches/fast)

The benchmarks above take hours and run in bulk. The fast suite is the opposite: many small
cases, each 10-300 s, all of them in under an hour on the main PC, run like unit tests for
performance ([fast/README.md](fast/README.md)). Each case is one bottleneck, on generated
data with a fixed seed or a pinned file, checked before its time counts, with time and
peak memory under a memory cap and, where a plan or engine path should handle it, that
path asserted. Results go to `baselines/fast/` and are compared beyond a confidence
interval.

**Who runs a case** (owner, 5 October 2026):
- **NRESE runs every case:** every reasoning profile (rdfs, rdfs-full, rdfs-plus,
  owl-horst, owl2-ql, owl2-rl, N3 rules, compact equality, EL, DL), every feature,
  operator and route. Each case keeps its own NRESE baseline.
- **Competitors run only what they support, under the same semantics,** and their times
  count only when their answers equal NRESE's. The stores without reasoning (QLever,
  Oxigraph, Virtuoso) run the plain cases, and the reasoning cases' queries on NRESE's
  exported closure (`closure`): the query side on equal terms, and what reasoning buys.
- **Each case declares its comparators** (`system: semantics`) in `fast/cases.toml`; the
  table below is generated from it (`fast.py table --write`), and `fast.py compete` picks
  the comparators from it. A case only NRESE can run has no comparator: its own column.
- **Home turf:** a win counts only where NRESE beats each competitor on what that
  competitor does best. Each open competitor has at least one case on its own turf,
  chosen from what the literature wiki documents (QLever: large read-only data, large
  results and text; Konclude: DL classification; ELK: EL classification; Openllet: DL
  consistency; Nemo: Datalog; HermiT: hypertableau classification) or, where the wiki is
  thin (Jena, Virtuoso, Oxigraph, RDF4J, GLog/VLog), from the owner's examples: Jena's bulk
  load and rules, Virtuoso's many small concurrent requests, Oxigraph's small embedded store
  opened for a first query, RDF4J's small transactions, GLog/VLog's single-threaded
  materialisation (no runner yet: Nemo stands in).

Frontier cases expect a known limit (`outcome`): they show the gap until it closes, and a
fix shows as a changed status.

<!-- fast-cases:start (fast.py table --write) -->
| Case | Area | NRESE runs | Comparators (system: semantics) | Home turf of | Measures |
|---|---|---|---|---|---|
| `rl-hierarchy` | reasoning | owl2-rl | nemo: owl2-rl, graphdb: owl2-rl, rdfox: owl2-rl, qlever: closure, oxigraph: closure |  | a 5,461-class tree and a 1,500-class spine over 1 M typed instances (9.7 M inherited types): 67 s in the rule joins on 6 Oct 2026, against 5 s for LUBM 100's 8.7 M |
| `rdfs-hierarchy` | reasoning | rdfs | jena: rdfs, rdf4j: rdfs, graphdb: rdfs, rdfox: rdfs |  | the same hierarchy under RDFS |
| `rl-clique` | reasoning | owl2-rl | nemo: owl2-rl, jena: owl-horst, graphdb: owl2-rl, rdfox: owl2-rl |  | a symmetric and transitive property closing 40 random trees of 300 to cliques (3.6 M pairs) |
| `rl-sameas` | reasoning | owl2-rl | nemo: owl2-rl, graphdb: owl2-rl, rdfox: owl2-rl |  | owl:sameAs chains: 20,000 groups of 10, a value each, spread to the group (equality by representatives, expanded in the stack) |
| `rl-sameas-compact` | reasoning | owl2-rl | none: NRESE's own column |  | the same sameAs groups with the stack kept over representatives (equality = compact) |
| `rl-chain` | reasoning | owl2-rl | nemo: owl2-rl, jena: owl-horst, graphdb: owl2-rl, rdfox: owl2-rl |  | two chains of 2,500 nodes along a transitive property (6.2 M pairs) |
| `rl-lubm100` | reasoning | owl2-rl | nemo: owl2-rl, jena: owl-horst, graphdb: owl2-rl, rdfox: owl2-rl, qlever: closure, oxigraph: closure, virtuoso: closure |  | LUBM 100 (13.4 M asserted, 8.7 M inferred) and its 14 queries: the headline materialisation |
| `rl-owl2bench` | reasoning | owl2-rl | nemo: owl2-rl, jena: owl-horst, graphdb: owl2-rl, rdfox: owl2-rl |  | OWL2Bench RL-1: a 1.3 M-pair symmetric-transitive clique from a real TBox, 22 queries |
| `rl-single-thread` | reasoning | owl2-rl | nemo: owl2-rl | GLog/VLog (no runner: Nemo stands in) | LUBM 10 on one thread: materialisation without parallelism |
| `rdfs-full-lubm10` | reasoning | rdfs-full | none: NRESE's own column |  | LUBM 10 under every RDFS rule and the axiomatic triples |
| `rdfs-plus-lubm10` | reasoning | rdfs-plus | graphdb: rdfs-plus |  | LUBM 10 under RDFS-Plus |
| `horst-lubm10` | reasoning | owl-horst | jena: owl-horst, graphdb: owl-horst, anzograph: owl-horst | Jena | LUBM 10 under OWL-Horst (pD*) |
| `ql-lubm10` | reasoning | owl2-ql | graphdb: owl2-ql, stardog: owl2-ql |  | LUBM 10 under OWL 2 QL, materialised |
| `custom-tc` | reasoning | custom | nemo: custom, jena: custom | Nemo | transitive closure of a random graph (2,000 nodes, 3,000 edges) as Notation3 user rules |
| `commits-readers` | maintenance | owl2-rl | graphdb: owl2-rl, rdfox: owl2-rl |  | 4,000 one-person commits added then removed under OWL 2 RL, with 8 readers running queries |
| `commits-rdfs` | maintenance | rdfs | graphdb: rdfs, rdfox: rdfs |  | the same commits under RDFS, without readers |
| `commits-plain` | maintenance | none | rdf4j: plain, oxigraph: plain, jena: plain | RDF4J | 20,000 commits without reasoning: the store's own write path, 4 readers alongside |
| `deletes-layered` | maintenance | owl2-rl | graphdb: owl2-rl, rdfox: owl2-rl |  | 10 deletions in a layered graph whose facts have many derivations (after SSPE): about 6 s each |
| `deletes-clique` | maintenance | owl2-rl | graphdb: owl2-rl, rdfox: owl2-rl |  | 4 edge deletions splitting a 300-node symmetric-transitive clique (the worst case for proofs) |
| `dl-transitive` | dl | dl | hermit: dl, openllet: dl, konclude: dl | Openllet | 18 definitions C ≡ ∃hasPart.D over a transitive hasPart in a cycle (ore_ont_10212's shape): time grows exponentially with their number |
| `dl-transitive-400` | dl | dl | konclude: dl, openllet: dl, hermit: dl | Konclude | the same shape with 400 definitions: Konclude decides it in 0.6 s; NRESE gives up (the frontier) |
| `dl-number` | dl | dl | hermit: dl, openllet: dl, konclude: dl |  | qualified number restrictions met exactly: ≥ 40 R.(B1 ⊔ B2) with ≤ 20 of each, on 200 individuals |
| `dl-nominals` | dl | dl | hermit: dl, openllet: dl, konclude: dl |  | nominals: a ring of 10,000 nodes coloured from {colour0, colour1, colour2}, neighbours different |
| `dl-datatypes` | dl | dl | hermit: dl, openllet: dl, konclude: dl |  | at least 256 values of xsd:byte (consistent) and 257 (inconsistent): the datatype theory's clique check |
| `dl-roles` | dl | dl | hermit: dl, openllet: dl, konclude: dl |  | 5,000 universals over the automaton of a transitive role with 200 subroles (ore_ont_1066's shape): about 6 clauses each |
| `el-classify` | dl | el | elk: el, konclude: dl, hermit: dl | ELK | OWL 2 EL classification of 20,000 random classes with existentials, definitions, a role hierarchy, a transitive role and a chain |
| `horn-classify` | dl | dl | hermit: dl, konclude: dl, openllet: dl | Konclude | classification of 1,500 Horn classes beyond EL (inverse roles, universals) by the context core |
| `canon-twins` | dl | canonical | none: NRESE's own column |  | blank-node canonicalisation: 3,000 equal children and 200 rings of 12, under two labellings (time grows with the square of the twins: 2,000 0.6 s, 8,000 12.7 s on 6 Oct 2026) |
| `mem-load-2g` | memory | none | none: NRESE's own column |  | a 10 M-statement bulk load in a 2 GiB container, spilling past a 256 MiB budget |
| `mem-load-20m-2g` | memory | none | none: NRESE's own column |  | a 20 M-statement load in 2 GiB with a 512 MiB budget: the dictionary isn't under the budget (the frontier) |
| `mem-reason-2g` | memory | owl2-rl | none: NRESE's own column |  | LUBM 10 materialised (OWL 2 RL) and queried in a 2 GiB container |
| `mem-reason-lubm100-2g` | memory | owl2-rl | none: NRESE's own column |  | LUBM 100 materialised in a 2 GiB container: the frontier until reasoning spills |
| `load-entities` | load | none | jena: plain, qlever: plain, oxigraph: plain, virtuoso: plain | Jena | bulk load of 20 M statements (the harness's entity shape) into memory |
| `open-first-query` | load | none | oxigraph: plain | Oxigraph | a 1 M-statement store on disk: open it in a fresh process and answer four point lookups (open plus first executions) |
| `q-star` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | 5-way stars: a selective one and a count over all people |
| `q-chain` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | 4-hop chains from one person, and a chain through posts and likes |
| `q-cycles` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | counts of directed triangles and 4-cycles in the follows graph |
| `q-snowflake` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | a snowflake: posts with author, tag and likes, each joined further and filtered |
| `q-paths` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | property paths: a tag's ancestors, the people one reaches, the closure's size, a tag's descendants |
| `q-sideways` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | LIMIT pushed into a join, a VALUES-seeded star, EXISTS as a set |
| `q-large-result` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain | QLever | large results serialised: 2.4 M follows edges, 600 k dated posts |
| `q-topk` | query | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | ORDER BY with LIMIT over 600 k scores and 300 k dates |
| `op-group` | operator | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | COUNT … GROUP BY on one pattern, NOT EXISTS by a group walk, COUNT DISTINCT |
| `op-aggregates` | operator | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | AVG, MAX, SUM and HAVING over a join; a grouped count ordered and cut |
| `op-optional` | operator | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | OPTIONAL-heavy: two optionals over 600 k posts, nested optionals |
| `op-negation` | operator | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | MINUS and NOT EXISTS over all people |
| `op-strings` | operator | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | CONTAINS, STRSTARTS and LANG over 300 k names |
| `op-dates` | operator | none | qlever: plain, oxigraph: plain, jena: plain, virtuoso: plain |  | YEAR ranges, integer and dateTime comparisons as index ranges on inline literals |
| `clients-lookups` | query | none | virtuoso: plain, qlever: plain, oxigraph: plain, jena: plain | Virtuoso | 32 concurrent clients doing point lookups for 20 s: throughput and p99 |
| `shacl-lubm` | service | none | jena: shacl, rdf4j: shacl |  | SHACL Core and SHACL-SPARQL over LUBM 10's asserted data |
| `geo-within` | service | none | jena: geosparql, qlever: geosparql, virtuoso: geosparql |  | GeoSPARQL sfWithin over 1 M points in four squares |
| `text-search` | service | none | qlever: text, jena: text, virtuoso: text | QLever | full-text search over 1 M short documents: four words and a prefix |
| `vector-knn-exact` | service | none | none: NRESE's own column |  | exact k-NN (k = 10, cosine) over 200 k 64-dimensional embeddings, filtered to one kind in ten |
| `vector-knn-filtered` | service | none | none: NRESE's own column |  | approximate k-NN (k = 10) with the same one-in-ten filter: the index's recall under a filter |
| `rdf12-annotations` | service | none | oxigraph: plain, jena: plain |  | RDF 1.2: 1 M reifiers of triple terms with a source and a confidence; a triple term with a constant subject |
| `rdf12-join-2g` | service | none | oxigraph: plain, jena: plain |  | asserted statements joined to the reifiers of their own triple terms, in 2 GiB (the frontier) |
<!-- fast-cases:end -->

## 7. Sources

Benchmarks and code, as checked on 5 October 2026 (licences from the repositories' metadata
or READMEs; "none stated" means neither had one):
- LDBC SPB 2.0: https://github.com/ldbc/ldbc_spb_bm_2.0 (Apache-2.0); LDBC fair-use policy: https://ldbcouncil.org/benchmarks/fair-use-policies/
- Sparqloscope: https://github.com/ad-freiburg/sparqloscope (Apache-2.0); paper: https://ad-publications.cs.uni-freiburg.de/ISWC_sparqloscope_BKTU_2025.pdf
- WatDiv: https://github.com/dsg-uwaterloo/watdiv (none stated)
- WDBench: https://github.com/MillenniumDB/WDBench (data on Figshare, from Wikidata truthy 2021-06-23)
- FEASIBLE: https://github.com/AKSW/FEASIBLE (AGPL-3.0); LSQ: https://github.com/AKSW/LSQ (Apache-2.0)
- OWL2Bench: https://github.com/kracr/owl2bench (Apache-2.0); NPD: https://github.com/ontop/npd-benchmark (Apache-2.0)
- ChaseBench: https://github.com/dbunibas/chasebench (none stated)
- ERA-SHACL: https://github.com/oeg-upm/ERA-SHACL-Benchmark (none stated)
- GeoSPARQL compliance: https://github.com/OpenLinkSoftware/GeoSPARQLBenchmark (GPL-2.0); Geographica 2: https://geographica2.di.uoa.gr/
- StarBench: https://github.com/dkw-aau/SPARQL-star-Benchmark (data CC-BY 4.0, code Apache-2.0)
- FedShop: https://github.com/GDD-Nantes/FedShop (GPL-3.0); LargeRDFBench: https://github.com/AKSW/LargeRDFBench (AGPL-3.0)
- LDBC SNB: https://github.com/ldbc/ldbc_snb_datagen_spark, ldbc_snb_interactive_v2_impls, ldbc_snb_bi; Graphalytics: https://github.com/ldbc/ldbc_graphalytics; FinBench: ldbc_finbench_datagen (all Apache-2.0)
- openCypher TCK: https://github.com/opencypher/openCypher (Apache-2.0); GAP: https://github.com/sbeamer/gapbs
- VIBE: https://github.com/vector-index-bench/vibe (MIT); big-ann-benchmarks: https://github.com/harsha-simhadri/big-ann-benchmarks (MIT); ann-benchmarks: https://github.com/erikbern/ann-benchmarks (MIT); BEIR: https://github.com/beir-cellar/beir (Apache-2.0)
- GraphRAG-Bench: https://github.com/GraphRAG-Bench/GraphRAG-Benchmark (MIT); MultiHop-RAG: https://github.com/yixuantt/MultiHop-RAG (ODC-BY); STaRK: https://github.com/snap-stanford/stark (MIT); CRAG: https://github.com/facebookresearch/CRAG (CC-BY-NC 4.0)
- Text-to-SPARQL: QALD-10 https://github.com/KGQA/QALD-10 (MIT); LC-QuAD 2.0 https://github.com/AskNowQA/LC-QuAD2.0 (none stated in the repository); chat-with-data https://github.com/datadotworld/cwd-benchmark-data (Apache-2.0)
- KG construction: Text2KGBench https://github.com/cenguix/Text2KGBench (Apache-2.0); Re-DocRED https://github.com/tonytan48/Re-DocRED (MIT); BioRED https://github.com/ncbi/BioRED; GTFS-Madrid-Bench https://github.com/oeg-upm/gtfs-bench (Apache-2.0)
- Systems: repository licences as listed in §4 (Kùzu archived; Memgraph, FalkorDB, Openllet and pgvector have no SPDX identifier in GitHub's metadata, their licence files say BSL 1.1 with the Memgraph Enterprise Licence, SSPL 1, a dual AGPL/commercial licence, and the PostgreSQL licence).
