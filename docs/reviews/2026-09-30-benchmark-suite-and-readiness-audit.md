# Benchmark suite and production-readiness audit

30 September 2026. Asked for by the owner: standardise the benchmark suite so that it runs on every ontology store and reasoner it names and skips what a system can't do; include the industry's standard benchmarks next to the workloads already collected; and say which gaps and bugs stand between NRESE and a production-ready store, because NRESE is to have every capability implemented and tuned before the comparison runs start.

## Summary

- **The suite exists as four kits with four drivers.** They measure the right things, and each was built for its own question. There was no list of workloads, no statement of what each system can do, and no common result format. The list and the capability rule now exist ([benches/suite](../../benches/suite/README.md)); the common adapter, result format and driver don't.
- **Of 20 workloads the suite should have, 7 run today, 1 runs in part, 8 are to add, and 4 wait for an NRESE capability.** The industry benchmarks missing are LDBC SPB for reasoning, and Sparqloscope, BSBM and WatDiv for queries.
- **NRESE can run 15 of the 20.** It can't run LDBC SPB (full RDFS), keyword search, GeoSPARQL, federation and classification. Nine capabilities of the suite's seventeen are missing.
- **The published scorecard is out of date.** It was measured on 26 September, before the native executor, in single runs on a workstation, and it says NRESE answers joins 10 to 2000 times slower than QLever. Nothing in it should be quoted.
- **One day of looking found four wrong-answer bugs, two planner defects and a 40 ms stall in the server** (§5), all fixed. Three of the bugs were found by test generators written for the purpose; the existing random test had let them through, because nine of ten of its queries had no solutions. The rate says there are more.
- **Before any number is published:** the P0 and P1 items of §5, the capabilities of §4, and a reference machine.

## 1. What the repository runs today

| Kit | Workloads | Systems | Starts them with | Result | State |
|---|---|---|---|---|---|
| `benches/competitors` (basics) | olympics, YAGO tiny, DBpedia core, Wikidata lexemes, synthetic entities: load, size, restart, query mix, throughput, writes under load | NRESE, QLever, Oxigraph, Virtuoso, Jena TDB2; GraphDB, RDFox, AnzoGraph (results local) | Docker | one CSV per dataset | Runs. Its published `SCORECARD.md` predates the native executor |
| `benches/reasoning` | LUBM, OWL2Bench: materialisation, inferred set against an oracle, query answers | NRESE, Jena rule reasoners, Nemo, owlrl (oracle) | Docker | one CSV | Runs for RT1 and RT3. RT2, RT4, RT5, RT7 only through `local-bulk.sh`, NRESE only |
| `benches/integration` | RG × GS × GND, example tier | NRESE, Fuseki as its project configures it | processes | one CSV | Runs on the example tier. Cohort tier being built |
| `benches/nrese-bench-harness` | the client (`query-mix`), write scaling, compatibility packs against a reference endpoint, ontology packs | any SPARQL endpoint | — | JSON reports | Runs |
| `benches/cluster` | a SLURM job for the integration kit | | processes | as the kit | Ran without SLURM only |
| Conformance (in CI) | W3C SPARQL 1.1 (query 222/232, update 94/94, syntax), W3C SHACL Core (98/98) | NRESE | `cargo test` | pass counts | Runs on every push |

## 2. Findings about the suite

1. **No single list.** Which workloads exist, what each needs and which system can run it was spread over four READMEs and two design documents. Fixed: `benches/suite/workloads.toml` and `systems.toml`, and `suite.py` prints the matrix.
2. **Four drivers, three result formats.** Each kit has its own way to start a system, measure and write results. Adding a system means writing it up to four times, and results can't be joined. To do: one adapter contract per system (`prepare`, `load`, `reason`, `serve`, `stop`) and one result schema.
3. **Two kits need Docker.** The cluster has none. The integration kit shows the alternative (plain processes); the others need an Apptainer path.
4. **The industry's benchmarks are missing.** LUBM and OWL2Bench are there. LDBC SPB, the one audited reasoning benchmark that vendors publish on, is not; nor are Sparqloscope (the current cross-engine query benchmark, from QLever's authors), BSBM and WatDiv. §3 lists them.
5. **The published scorecard is stale** (26 September, before the native executor) and was never repeated. It should be withdrawn from the README until it is rerun.
6. **Single runs on shared workstations.** No result so far comes from a machine that ran nothing else. The cluster fixes that ([benches/cluster](../../benches/cluster/README.md)).
7. **The correctness oracles have limits.** owlrl finishes only on small inputs; beyond, Nemo is the reference. For queries, the differential tests trust spareval, which this audit found wrong in four cases (§5). A second, independent engine should answer the conformance queries too.
8. **Reasoning tasks are covered unevenly.** RT1 and RT3 run across systems. RT2 and RT5 (incremental maintenance, commit latency) run for NRESE only; RT4 and RT7 (inconsistency, explanations) have no workload; RT6 (classification) has no NRESE capability.
9. **Entailment regimes were not named.** Systems that derive different things were about to be compared on the same queries. The integration workload made it visible: under full equality the answers are larger than under the identity closure alone. Every reasoning result now has to state its regime.

## 3. The standard suite

The rule: a workload names the capabilities it needs; a system names the ones it has; a pair runs if the system has them all, and is skipped with the missing capability as the reason. `benches/suite/suite.py` prints the matrix below from the two files.

**Workloads** (state: ✅ runs, ◐ runs in part, ＋ to add, ⏸ waits for an NRESE capability):

| Area | Workload | Why it is in | State |
|---|---|---|---|
| Reasoning | LUBM (1 to 1000) | The standard materialisation benchmark | ✅ |
| | OWL2Bench (RL, QL, EL, DL) | Language coverage per profile; replaces UOBM | ✅ |
| | **LDBC SPB 2.0** | The audited industry benchmark: queries under inference with continuous updates | ＋ (needs full RDFS: B1) |
| | RG × GS × GND integration | Identity reasoning over real links | ◐ |
| | DBpedia and YAGO with their ontologies | Real, deep hierarchies | ＋ |
| | Injected contradictions | Detection and explanation (RT4, RT7) | ＋ |
| | W3C OWL 2 test cases (RL) | Conformance of the rule set | ＋ |
| | ORE 2015 | Classification | ⏸ (no classification) |
| Queries, updates | Basics mix (5 datasets) | Load, size, restart, query mix | ✅ |
| | **Sparqloscope** | Each SPARQL feature measured on its own; DBLP and Wikidata | ＋ |
| | **BSBM** | The classic query and update mix | ＋ |
| | **WatDiv** | Query shapes as a stress test | ＋ |
| | Write scaling | Commit latency by store size, writes under read load | ✅ |
| | W3C SPARQL 1.1 | Conformance | ✅ |
| Validation | W3C SHACL | Conformance | ✅ |
| | ERA-SHACL-Benchmark | Validation time and memory on real data | ＋ |
| Search, space, federation | Keyword search (ResearchSpace's queries, LUBMft) | | ⏸ (F1) |
| | GeoSPARQL compliance benchmark, Geographica 2 | | ⏸ (F2) |
| | FedBench | | ⏸ (D4) |
| Consumers | ResearchSpace and the Datamodel Workflow on the store | The integrations work | ✅ |

Left out on purpose: SP²Bench (superseded by the above), UOBM (OWL2Bench extends it), WDBench and full Wikidata (need a server class the cluster's standard nodes don't have; later), LDBC SNB (property-graph oriented).

**Systems:** NRESE; without reasoning QLever, Oxigraph, Virtuoso; with reasoning Jena (rule reasoners), RDF4J (RDFS), Nemo (datalog), GraphDB, RDFox, Stardog, AnzoGraph; as oracles owlrl and the DL reasoners. The last four commercial systems need licences and their vendors' permission to publish.

## 4. NRESE first: what is missing

The owner's rule: every capability implemented and tuned before the comparison runs.

| Capability | State | What is missing | Plan step |
|---|---|---|---|
| Load, query, update | done | Tuned within a basic graph pattern; see §5 for the planner | — |
| OWL 2 RL materialisation, incremental maintenance | done, with documented exceptions | Datatype reasoning; equality by representatives; unnamed-class typing | B3, W4, W7 |
| Consistency with explanations | done for commits | The W3C RL consistency tests; proof trees for inferred statements | B3, R6 |
| Full RDFS, OWL-Horst, OWL 2 QL profiles | **missing** (RDFS is a six-rule subset) | The profile registry | B1 |
| Custom rules | **missing** | `.pie` import into the rule IR | B2 |
| SHACL Core | done on request | The commit gate, incremental validation, parallel evaluation | C2, C1c |
| SHACL-SPARQL | **missing** | | C3 |
| Full-text search | **missing** | | F1 |
| GeoSPARQL | **missing** | | F2 |
| Federation (`SERVICE`) | **missing** | | D4 |
| Classification | **missing**, not planned | Decide: build, or leave RT6 to the DL reasoners | R8 |

## 5. Production-readiness: gaps and bugs

P0 = wrong answers or lost data; P1 = a production deployment would hit it; P2 = needed for parity, not for safety.

### Found and fixed during this audit

| Bug | Effect | Found by |
|---|---|---|
| `HAVING` on a `SELECT` alias | No rows, where Jena returns rows | The integration workload (CQ10) |
| Compiled filters on values the query computed | `BIND(STR(?a) AS ?f) FILTER(isLiteral(?f))` returned nothing; likewise `CONTAINS`, `REGEX`, `LANG`, comparisons on bound values | A new test generator |
| `=` on dates with a timezone on one side | An error instead of false | The same |
| `=` between a tagged string and an ill-typed literal | An error instead of false | A second new generator |
| Filters evaluated after all joins of their group | 11 s and out-of-memory on 66 k statements | The integration workload |
| Open property paths computed for every node | The same | The integration workload |
| No `TCP_NODELAY` on the server's connections | A streamed result waited 40 ms for the client's delayed acknowledgement (CQ06: 44 ms for a 0.7 ms query) | The integration workload, rerun after the fixes above |

### Fixed after the audit (same day)

| Gap | What changed | Commit |
|---|---|---|
| 3: the memory budget was no bound | Operators charge hash tables and working memory, tables grow within what is left, a server-wide budget (`budgets.total_query_memory`, default half the machine's memory) caps all queries together. Measured on Linux: a 1 GiB budget peaks at 815 MiB (was 5.8 GB with 4 GiB) | `55dc842` |
| 8: aggregates over independent OPTIONALs | `SELECT DISTINCT` and groups whose aggregates ignore duplicates work on sets; an OPTIONAL that only feeds aggregates is joined to the kept groups. CQ05 answers (9,009 rows, 0.4 s) | `ca89027` |
| 1, in part: the general evaluator ran too much | Now native: joins on possibly unbound variables; datasets (`FROM`, `FROM NAMED`, `USING`, `WITH`, the protocol's parameters); `GRAPH` over any pattern; EXISTS over any pattern and anywhere in an expression, correlated through filters; `DESCRIBE`; `BASE`; NOW, RAND, UUID, STRUUID, BNODE(), the hashes, TIMEZONE, TZ | `1b86db5`, `7d59d4f`, `8c7d47e`, `52b7761` |
| Native answers where spareval's are wrong | Several `FROM` graphs merge (a statement they share counts once); `GRAPH` naming a graph outside the dataset has no solutions; per graph under `GRAPH ?g`: `VALUES`, subqueries, `MINUS` without other shared variables; GROUP_CONCAT is a simple literal. Five W3C tests the oracle fails now pass (`agg-empty-group-count-graph`, `bindings#graph`, `graph-minus`, `agg-groupconcat-04`, `-06`) | `7d59d4f`, `637037d` |
| 15, in part: weak generators | Generators with solutions for datasets and `GRAPH`, the merged default graph, EXISTS in every position, updates with datasets, DESCRIBE; each new rule mutation-checked | same |

### Open

| # | Priority | Gap | Evidence | Step |
|---|---|---|---|---|
| 1 | P0 | **The general evaluator returns wrong answers in known cases,** and still runs what the native executor doesn't: `BNODE` with a label, `ADJUST`, functions outside SPARQL 1.1 (other than the XSD casts), `SERVICE`, RDF 1.2 triple terms, EXISTS correlated inside an OPTIONAL, MINUS, BIND or limited subquery of its pattern, and an update's `WHERE` after an earlier operation of the same request changed something. Its known errors: (a) a filter on a variable a subquery hides is moved into the subquery; (b) after a zero-length path from a variable bound to a literal, `=` compares terms; (c) `DATATYPE` of an `xsd:int` is `xsd:integer`; (d) with both ends of a path bound, a pair that exists twice counts once; (e) the cases in the row above. The native executor itself departs from the standard in one case the oracle shares: a zero-length path from a constant that isn't a node of the graph gives nothing (four W3C tests added after SPARQL 1.1) | Differential tests, each case pinned; W3C suite | Native reads inside a transaction; the zero-length rule; report upstream |
| 2 | P0 | **The tests' oracle is the evaluator of item 1.** Where both executors are wrong the same way, nothing notices | This audit | A second engine (Jena) answers the conformance and differential queries in a nightly job |
| 4 | P1 | **Results of operators are built as whole tables.** A query whose intermediate result exceeds the budget fails, where a pipelined engine would stream it | CQ05 | W6 |
| 5 | P1 | **The store serves from memory.** Data larger than RAM can't be served; last measured, 67 M statements took 16 GiB to serve (QLever: 0.2 GiB) | Roadmap, Pf2 | Pf2 step 2 (memory-mapped runs), Pf5 |
| 6 | P1 | **Join order stops at pattern boundaries.** A path, subquery or OPTIONAL splits a group; the parts are joined as written. Only paths take bound values from their partner, and filters don't inform the order | Plans of the integration questions | W3b |
| 7 | P1 | **Equality reasoning replicates.** Every statement is copied to every identity of its terms; answers repeat per identity | 883 `owl:sameAs` statements for 65 resources | W4 |
| 9 | P1 | **A full materialisation can't be cancelled**, nor the closure of a newly declared transitive property | Capability matrix | A8 remainder |
| 10 | P1 | **No repository isolation and no graph-level access control.** One dataset per server; authentication exists, authorisation by graph doesn't | Capability matrix | D1, E1 |
| 11 | P1 | **Backups have no versioned manifest and no point-in-time restore;** no upgrade test for the on-disk format | Capability matrix | E3 |
| 12 | P1 | **Operations can't be observed:** no request outcomes or latencies, no WAL, checkpoint or backup metrics | Capability matrix | E2 |
| 13 | P1 | **What ResearchSpace needs is incomplete:** keyword search finds nothing; the RDF4J repository type doesn't connect | Smoke test | F1, D2 |
| 14 | P1 | **SHACL doesn't gate commits.** Validation runs on request | Capability matrix | C2 |
| 15 | P1 | **Plain updates still use the weak generator** (the main random test: a fifth of its queries have solutions) | This audit | A generator with solutions for updates without a dataset |
| 16 | P1 | **No test under sustained concurrent load,** and no fuzzing of the HTTP surface or the parsers | Not found in the repository | A soak test in the perf lab; fuzz targets |
| 17 | P1 | **No reference machine and no repeated runs** behind any published number | §2 | The cluster |
| 18 | P2 | Reasoning profiles beyond OWL 2 RL, datatype reasoning, custom rules | §4 | B1 to B3 |
| 19 | P2 | SHACL-SPARQL, full-text, GeoSPARQL, federation, RDF 1.2 triple terms, vector search | §4 | C3, F1 to F4, D4 |
| 20 | P2 | TLS and client certificates end at a reverse proxy | Server setup guide | Decide whether that is the supported deployment |

## 6. Order of work

1. **Correctness before speed:** items 1, 2 and 15. Every area gets a generator with solutions; a second oracle runs nightly.
2. **The executor's resource behaviour:** items 4 and 6 (W3b, W6). They decide whether the larger tiers of the integration workload answer at all.
3. **Reasoning as the suite needs it:** the profiles (B1, which also unlocks LDBC SPB), equality by representatives (W4), datatypes (B3).
4. **The capabilities the consumers wait for:** full-text (F1), multi-repository and RDF4J (D1, D2), the SHACL commit gate (C2).
5. **The suite's plumbing,** while 1 to 4 proceed: one adapter contract and result schema, the Apptainer path, then the missing workloads (LDBC SPB, Sparqloscope, BSBM, WatDiv, ERA-SHACL).
6. **Then the runs,** on the cluster, three times each, with the licensed systems only after their vendors agree.
