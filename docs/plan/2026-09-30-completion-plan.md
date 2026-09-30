# Completion plan: from here to the benchmark runs

As of 30 September 2026, branch `refactor/engine-v2`. NRESE has every capability the
benchmark suite asks for (audit §4), and the native executor answers every standard
query. What remains is listed here, in the order it is done: first what decides the
reasoning numbers, then what decides whether the large tiers answer at all, then the
suite that runs everything, then completeness items no workload needs yet. When the list
is done, the suite can be set up and run (§6).

Every work package ends with: tests (differential against an oracle where one exists,
mutation-checked), the documentation it changes, a commit. Measurements are taken in bulk
at the end of a phase, not per package.

## Phase 1: reasoning performance

| # | Work package | Why | Done when |
|---|---|---|---|
| 1.1 | **Equality by representatives (W4)**, stage A: the closure computed over one representative per `owl:sameAs` class, by rewriting instead of copying | The equality module copies every fact to every combination of its terms' identities: O(k^n) facts for classes of size k. CQ05 aggregates 4.25 M rows, CQ08 evaluates 159 k, on the smallest tier | Expanding the representative closure gives exactly the replicated closure, on random ontologies with equality (property test, every profile with equality) |
| 1.2 | W4 stage B: the store keeps the representative closure (`reasoner.equality = "representatives"`) and queries read it through expansion at the scans | Materialisation time and memory shrink by the replication factor; query answers stay those of the standard | Query answers equal replicated mode on random data with equality (differential, all query shapes); commits maintain it (insertions incrementally; a deletion that can split a class recomputes the affected classes) |
| 1.3 | W4 stage C: late expansion: joins run on representatives; a column is expanded to its identities only where an operator reads terms or multiplicities (filters, expressions, grouping, DISTINCT, ORDER BY, LIMIT, output) | The query-time part of the win | Same differential test; the integration questions' plans show representative-level joins |
| 1.4 | W4 stage D: GraphDB's `onto:disable-sameAs` view: answers on representatives only | Users who want one row per individual, not per identity | Pseudo-graph recognised; documented |
| 1.5 | **Types in unnamed classes (W7)** | With the GND ontology, 55 % of inferred statements are memberships in unnamed union classes | A mode that keeps them out of the store and the answers while every consequence for named classes stays; closure equal on named classes |
| 1.6 | **Datatype rules (B3)**: OWL 2 RL table 8 (`dt-type1/2`, `dt-eq`, `dt-diff`, `dt-not-type`) without folding lexical forms | OWL2Bench and the W3C OWL 2 RL tests use them | W3C RL consistency and entailment tests for datatypes pass |
| 1.7 | **Cancellable materialisation (A8 remainder)** | A full materialisation, or the closure of a newly declared transitive property, can't be stopped | `Stop` polled in the batch executor's rounds and modules; a cancelled rematerialisation leaves the previous state |

## Phase 2: queries at scale

| # | Work package | Why | Done when |
|---|---|---|---|
| 2.1 | **Pipelined execution (W6)**: joins stream into aggregation, DISTINCT and the result writer instead of building every intermediate table | A query whose intermediate result exceeds the budget fails where a pipelined engine streams it | Peak memory of the question mix and of the large LUBM/SP2Bench queries stays within the budget; results unchanged |
| 2.2 | **Filter selectivity in the join order (W3b rest)** | A selective filter should pull its pattern to the front | Plans of the filtered benchmark queries show it; differential tests |
| 2.3 | **The R-tree for `geof:` filter functions**; GML and GeoJSON literals | GeoSPARQL benchmarks filter with functions | `FILTER(geof:sfWithin(?wkt, const))` uses the index; compliance benchmark subset passes |
| 2.4 | **Memory-mapped runs (Pf2 step 2, Pf5)** | The store serves from RAM: 67 M statements took 16 GiB (QLever 0.2 GiB); the full tiers and LDBC SPB at 1 B need more than RAM | Serving memory proportional to the working set; a dataset larger than RAM answers |

## Phase 3: the suite that runs everything

| # | Work package | Done when |
|---|---|---|
| 3.1 | **One adapter contract** per system (start, load, reason, query, update, stop, capabilities) and **one result schema** (CSV/JSON: system, workload, tier, task, run, rows, ms, peak memory, status) | Every system of `systems.toml` has an adapter; the existing kits write the schema |
| 3.2 | **One driver** (`benches/suite/suite.py run …`) that runs the capability matrix, three runs each, cleans up after every run | A dry run over all systems and workloads on the main PC |
| 3.3 | **The Apptainer path** for Draco (SIF images of each system, the SLURM job generalised from `benches/cluster`) | A job file per workload; tested locally with Apptainer in WSL |
| 3.4 | **Missing workloads**: LDBC SPB (generator, rules, query mix), Sparqloscope, BSBM, WatDiv, ERA-SHACL, the ORE 2015 EL corpus | Each has a kit that fetches or generates its data outside the repository and checks its answers |
| 3.5 | **A second oracle**: Jena answers the W3C suite and the differential queries in a nightly job | The job and its report |
| 3.6 | **Soak and fuzzing**: sustained concurrent load; fuzz targets for the HTTP surface and the parsers | Targets in the repository; a soak run's report |

## Phase 4: completeness

| # | Work package |
|---|---|
| 4.1 | Custom rules (B2): user rules in the rule IR's syntax, and GraphDB `.pie` import |
| 4.2 | SHACL-SPARQL (C3) and the SHACL commit gate (C2) |
| 4.3 | Zero-length paths from a term outside the graph (four W3C tests), `BNODE(label)` per solution |
| 4.4 | Jena's `text:query`; phrase search and stemming |
| 4.5 | Operations: request outcome and latency metrics, WAL/checkpoint/backup metrics (E2); versioned backups and point-in-time restore (E3); repository isolation and graph-level access control (D1, E1) |
| 4.6 | The deviations the Jena oracle found, which NRESE shares with spareval: string functions of numbers with non-canonical lexical forms (`STR("07"^^xsd:integer)` is `"07"`), and path alternatives as a UNION with duplicates. The native executor follows the standard; the differential tests learn where spareval doesn't (benches/oracle/README.md) |

## Phase 5: measurement

The integration workload (example, cohort and full tiers; NRESE and Fuseki), LUBM and
OWL2Bench at scale, before and after phases 1 and 2, on the main PC; then the suite on
Draco. The scorecard is replaced by the suite's report.

## 6. What waits on the owner

- The Draco access ticket.
- Licence files and the vendors' answers (GraphDB, RDFox, Stardog).
- Daniel on the missing `rules/sameas.rules`; the workload authors' agreement to be
  named and to publish with their data.
- Whether and when to push `refactor/engine-v2`.

## Status

Updated as packages land (commit in brackets).

- 1.1 done (`fb0128d`). Measured with `reason_query --equality-report` on the integration workload's example tier: with the project's ontology the replicated closure has 65,928 facts, the representative one 25,902 (9 classes, the largest of 24 identities), computed 4 times faster; with the GND ontology 253,599 against 187,531, most of the rest being memberships in unnamed classes. So W7 (1.5) goes next, then 1.2 and 1.3.
- 1.5 done: `reasoner.unnamed_classes = "skip"`. With the GND ontology, 219,840 inferred statements become 99,213 and materialisation takes half the time; the ten questions answer the same.
- 1.3, in part: with the data closed under equality (`QueryOptions::equality_closed`, set by the store), an OPTIONAL that feeds only duplicate-insensitive aggregates joins on one representative per identity class. CQ05's cost is not replication of keys but the distinct place identities its GROUP_CONCAT asks for (472 per group), so it stays. 1.2 (representative storage) is deferred until the cohort and full tiers show that storage or materialisation time matters: on the example tier the replicated closure is 2.5 times larger and takes 0.03 s.
- 1.7 done: the batch executor and the transitive-closure kernel poll a stop signal; `StoreService::rematerialise_until`; a mutation's rematerialisation stops with the request.
- 1.6 done for consistency: dt-not-type and dt-diff with the XSD value spaces OWL 2 uses (`crates/nrese-store/src/datatypes.rs`); typing and equating literals by value (dt-type, dt-eq) left out, as documented.
- Cohort tier measured (office PC, 1.61 M asserted statements, OWL 2 RL): materialisation 3.25 s, peak 1.1 GB; all ten questions answer, 1.8 s together (CQ05 1.34 s, CQ08 0.30 s). `--equality-report`: the replicated closure has 3.33 M facts and takes 3.09 s, the representative one 2.04 M and 0.21 s (8,723 classes, the largest of 38). So 1.2 pays at scale (14 times faster materialisation); it is queued after phases 2 and 3.
- 2.2 done: the join order plans each pattern with what its filters leave (equality with a constant 1 %, ranges 30 %, string tests 25 %, negations 90 %); probing keeps exact counts.
- 2.3 done: `FILTER(geof:R(?wkt, constant))` over a `geo:asWKT` (`asGeoJSON`, `asGML`) pattern starts the joins from the R-tree's candidates (400 points, a box of 9: no step reads more than 50 rows); GeoJSON and GML simple-feature literals, `geof:asGeoJSON`. The OGC compliance subset runs with the suite (phase 3).
- 3.1 and 3.2 done: `benches/suite/suite.py run` walks the matrix through one adapter per system (`suitekit/adapters.py`: NRESE, QLever, Oxigraph, Jena with TDB2 or its rule reasoners, Virtuoso, GraphDB, RDFox, AnzoGraph, Nemo, owlrl) and writes one result schema (`suitekit/schema.py`); `suite.py report` makes the tables and leaves out the licensed systems unless asked. A dry run covers every system and workload; real runs on the main PC (LUBM(1) on NRESE, Jena, Nemo and owlrl; the olympics mix on NRESE, QLever, Oxigraph, Virtuoso and Jena) agree on the answers and clean up after themselves. The RDFox adapter and GraphDB with a ruleset wait for their licences to run.
- 3.3 done: `--runtime apptainer` with `benches/cluster/build-sif.sh` and `benches/cluster/suite.sbatch`; run in WSL (Apptainer 1.5.4) on LUBM(1) and the olympics mix with NRESE, QLever, Oxigraph and Nemo. Not yet run under SLURM (no account).
- 3.5 done: the Jena oracle (`benches/oracle`, nightly workflow): 9,403 queries of the differential tests, all agreeing or explained. It found a bug that both evaluators shared: decimal multiplication and division with a zero operand were errors in oxsdatatypes, fixed in a vendored copy (`vendor/oxsdatatypes`); and three known deviations of NRESE (4.3, 4.6). Jena needed its optimiser off and showed two deviations of its own.
- Found on the way: the bench harness's lockfile was stale since the GeoSPARQL dependencies (CI's `--locked` would have failed); updated.
- 3.4, first workload: the W3C OWL 2 test cases of the RL profile (`crates/nrese-store/tests/w3c_owl2_rl`, in CI): 70 pass, 16 ask for conclusions outside the OWL 2 RL/RDF rules (listed with reasons), 3 skipped. It found a panic in the consistency check (a grounded instance with fewer variables than its rule), the missing rule `dt-type1` (the datatypes as `rdfs:Datatype`, now OWL 2 RL's axioms), and that data values were compared by IEEE equality where OWL 2 compares by identity (`+0.0` and `-0.0`); all three fixed. With it, 1.6 is done: the datatype tests of the suite pass.
- 3.4, second workload: the GeoSPARQL compliance benchmark (`crates/nrese-sparql/tests/geosparql_compliance`, in CI): 175 of 212 queries pass; the 37 others are reference answers that contradict GeoSPARQL's definitions or the benchmark's own requirements, each checked against the data and listed. It found and fixed: empty WKT and GML literals and empty GML points, the equality of empty geometries, `geof:boundary`, buffers in metres on CRS84, clipping noise in constructed coordinates, relation patterns that ignored the relation statements in the data, and constructions now answer in their input's serialisation (GML in, GML out).
