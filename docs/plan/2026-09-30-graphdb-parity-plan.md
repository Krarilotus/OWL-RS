# GraphDB parity: status and plan

> A record of its date. Its status columns and lines are not kept up to date: the
> current status is in [STATUS.md](../STATUS.md).

**Date:** 30 September 2026. **Inputs:**
- the GraphDB parity list (packages 1–6, "the friend's list");
- its disposition in [ROADMAP §6](../ROADMAP.md#6-friends-graphdb-parity-list-disposition);
- the [architecture and direction review](../reviews/2026-09-30-overall-architecture-and-direction-review.md).

The list is **guidance, not the scope**. It is GraphDB-centric and leaves out things an enterprise product needs:
- honest semantic contracts;
- strict lifecycle states;
- installability;
- evidence.

It also lists items we deliberately do differently (§6 of the roadmap). This plan gives the status of every item, then an order of work that closes the gaps without eroding what makes the engine good.

## 1. Where each item stands

Legend: ✅ done · 🟡 partial · ❌ missing · ➖ deliberately different.

| # | Item | Status | Evidence / gap |
|---|---|---|---|
| 1.1 | Provenance-aware quad indexing | ✅ ➖ | Two stacks (asserted/inferred) instead of per-entry flags; read models and `onto:explicit`/`onto:implicit`; differential-tested |
| 1.1 | RDF-star / triple terms | ✅ | RDF 1.2 triple terms (`TermKind::Triple`, keyed by component ids) in every layer; W3C SPARQL 1.2 and RDF 1.2 suites |
| 1.2 | Arena dictionary, sequential ids | ✅ | Engine dictionary; bulk restore |
| 1.2 | Inline native literals | ✅ | integer, decimal, boolean, date, dateTime (canonical forms only) |
| 1.2 | Dictionary cache eviction | ➖ | Only meaningful with an on-disk dictionary (Pf5) |
| 1.2 | Rollback of dictionary ids | ➖ | Rejected: unreferenced ids are reclaimed at checkpoint rebuild |
| 1.3 | MVCC, WAL, checkpoints, compaction | ✅ | Crash and torn-tail tests; background compaction; checkpoint format 7, used in place (memory-mapped index, dictionary and text order; restart in milliseconds) |
| 2.1 | `.pie` rule language | 🟡 | `.pie` import into the rule IR (B2, 2 October 2026): 8 of GraphDB's 12 built-in rulesets compile (RDFS, RDFS-Plus, OWL-Horst, Publishing); OWL 2 RL/QL need contexts on variable predicates and existentials. No export yet |
| 2.1 | Profiles: `rdfs` | ✅ | `rdfs` and `rdfs-full` (every RDFS rule and the axiomatic triples) |
| 2.1 | Profiles: `owl-horst`, `owl2-ql`, `rdfs-plus` | ✅ | `owl-horst`, `owl2-ql` (materialisable part) and `rdfs-plus` modes |
| 2.1 | Profiles: `owl2-rl` | 🟡 | 57 rules; `eq-ref` omitted (self-difference is accepted), datatype Table 8 missing |
| 2.1 | Rules compiled to id-level code | ✅ | Schema-grounded rule instances, dispatch index, parallel joins |
| 2.2 | Semi-naive fixpoint, consistency rules | ✅ | Batch = naive on 400 random ontologies |
| 2.3 | TMS, retraction, atomic rollback | ✅ | DRed + B/F; incremental = rematerialisation on 84 k random changes |
| 3.1–3.2 | SHACL Core, SHACL-SPARQL, commit gate, incremental validation | ✅ | `nrese-shacl`: W3C Core 98 of 98, SHACL-SPARQL 22 of 22; served at `/dataset/shacl`; commit gate (off, report, enforce) with incremental validation of the affected focus nodes (2 October 2026). Not yet: parallel, set-at-a-time evaluation (C1c). Design: [shacl.md](../design/shacl.md) |
| 4.1 | SPARQL 1.1 Query, Update | ✅ | W3C suite 505/505 with federation (2 October 2026); the native executor answers every query |
| 4.1 | Federated `SERVICE` | ✅ | Allowlist, timeouts and row limits (`store.federation`) |
| 4.1 | Graph Store Protocol | 🟡 | Works; status codes for missing graphs differ from the spec (review) |
| 4.2 | RDF4J protocol, repositories, transactions | 🟡 | X1: the RDF4J REST protocol with transactions over the one dataset (2 October 2026); several repositories, reads inside a transaction and persisted namespaces missing |
| 4.2 | Formats | 🟡 | N-Triples, N-Quads, Turtle, TriG, RDF/XML; JSON-LD and Binary RDF missing |
| 5.1 | Full-text search, `luc:` predicates | 🟡 | Full-text search with Blazegraph's `bds:` vocabulary (BM25, phrases, Snowball stemming), Jena's `text:query` and GraphDB's legacy `luc:` predicates on one index (2 October 2026); GraphDB's connectors (configured indexes) missing |
| 5.2 | GeoSPARQL, R-tree | ✅ | GeoSPARQL functions and relations with an R-tree (benchmarked with the GeoSPARQL compliance kit) |
| 5.3 | Vector similarity | ❌ | V1 |
| 6.1 | Online backup/restore | 🟡 | Snapshot-consistent export and restore through the pipeline; no manifest/versioning or point-in-time restore |
| 6.1 | Parallel bulk loader | ✅ | E5; Binary RDF input missing (follows the format) |
| 6.2 | Cost-based optimiser, statistics | ✅ | Distinct statistics, DP join ordering, WCOJ |
| 6.2 | Explain / profile | ✅ | `explain=true`, operator rows and timings |
| 6.2 | Timeouts, throttling | 🟡 | Real cancellation, rate limits, per-query memory budget; no admission control across queries; cancellation doesn't reach commit-path reasoning |
| 6.3 | RBAC, graph-level security | 🟡 | Read/Admin grants only; no editor role, no graph rules |
| 6.3 | Prometheus metrics | 🟡 | Readiness, counts, cache; no request outcomes, latencies, WAL/compaction/backup metrics |
| 6.3 | Structured tracing | ✅ | `tracing` spans and request ids |

**Summary:**
- Packages 1, 2 (except profiles) and 6.2 are largely done: the hard engine and reasoning core.
- The gaps are four things:
  - reasoning profile breadth (2.1);
  - governance (3, 6.3);
  - integration protocols (4.2, federation);
  - specialised indexes (5).

## 2. What must come first, and why

The review's P0/P1 findings come before any new feature. Every later feature (SHACL gates, RDF4J transactions, graph security) builds on the same boundaries: lifecycle states, typed errors, validated configuration and honest capability reporting. If they're wrong, each new feature inherits the defect.

## 3. Phases

Each phase is a set of slices. A slice isn't done until it has:
- code;
- tests (differential or conformance where one exists);
- docs;
- the removal of what it replaces.

Sizes: S ≤ 1 week, M 1–3 weeks, L 3–6 weeks (one developer).

### Phase A: trust and hygiene (review P0/P1), S–M. **In progress.**

| Slice | Scope | Done when |
|---|---|---|
| A1 ✅ | Reasoning state: semantic fingerprint, quarantine for inconsistent baselines | Lifecycle test (import, quarantine, repair); committed `ceb8c8e` |
| A2 ✅ | Self-difference and other reflexive-equality consistency cases (`eq-ref` gap) | `x owl:differentFrom x` rejected on commit and in full materialisation; committed `b349d11` |
| A3 ✅ | Typed failure meaning at HTTP: request errors 4xx, server faults 5xx with request id | Per mutation kind: syntax, parse, cancel, reject and injected engine error; committed `f0d5e77` |
| A4 ✅ | Configuration: unknown keys rejected (with path); `config check` command; redacted effective config | Misspelt keys fail at startup; aliases and env overrides still work; committed `2213722` |
| A5 ✅ | CI: the harness lockfile; the exact locked CI commands pass | `cargo test --locked` for the workspace and the harness; committed `97a6a78` |
| A6 ✅ | Honest docs: capability matrix, README, a **supported-semantics contract** (rules supported/omitted, datatype map, graph scope, read models) | Matrix, README, config reference and `/version` capabilities agree: [reasoning-semantics.md](../spec/reasoning-semantics.md); `/version` `reasoning_semantics`; profiles list only what their mode computes |
| A7 ✅ | Ontology diagnostics through the public boundary (malformed/cyclic/oversized lists) | Materialisation and commit reports carry typed diagnostics: `OntologyDiagnostic` in `MaterialisationReport` and the run record; a commit reports only what it introduced |
| A8 ✅ | Cancellation and work budgets inside commit-path reasoning | A cancelled large reasoning update releases the writer promptly and leaves both stacks unchanged: 1 M-type commit cancelled after 100 ms returns within 2 s (23.8 s without the poll); not yet polled: startup-style full materialisation, new transitive closures |

### Phase B: reasoning profiles, GraphDB semantic parity (2.1, 2.2), M–L

GraphDB's value is its rulesets. Ours are data (rule text), so breadth is mostly rules plus evidence.

| Slice | Scope | Done when |
|---|---|---|
| B1 | Profile registry: `rdfs` (full RDFS entailment; axiomatic triples optional), `rdfs-plus`, `owl-horst`, `owl2-rl`, `owl2-ql` (materialisable part); per-repository selection, reported in capabilities | Each profile's fixtures pass; inferred sets equal GraphDB's for the same profile on LUBM (local comparison, licence permitting) |
| B2 | `.pie` import (and export) into the rule IR | Round trip on GraphDB's published rulesets |
| B3 | Datatype Table 8 subset: value-space equality and literal consistency, without folding lexical forms | W3C RL datatype tests for the supported types |
| B4 | Hierarchy module: prune rule instances the hierarchy already implies (the measured 27 M redundant derivations on LUBM(100)) | Batch = naive unchanged; derivations and closure time recorded |

### Phase C: SHACL (3.1, 3.2), L

| Slice | Scope | Done when |
|---|---|---|
| C1a ✅ | `nrese-shacl` crate: shapes compiled to an id-level program; SHACL Core components, all property paths, severities; the report as a graph | W3C SHACL Core suite: 98 of 98 |
| C1b ✅ | Store operation and server endpoint: validate the repository, or a posted shapes graph; the report as RDF and JSON; configuration | HTTP tests; the report round-trips as RDF (`shacl_api_tests.rs`) |
| C1c | Parallel and set-at-a-time evaluation | Same reports as C1a; perf-lab numbers recorded |
| C2 | Commit gate after reasoning, in store orchestration; a target index; incremental validation from Δ⁺/Δ⁻ | Incremental = full revalidation on random deltas; cost proportional to affected focus nodes |
| C3 | SHACL-SPARQL constraints and targets through `nrese-sparql` | W3C SHACL-SPARQL tests |

### Phase D: protocols and integration (4.1, 4.2), L

| Slice | Scope | Done when |
|---|---|---|
| D1 | **Multi-repository core:** a repository registry (one engine per repository, each with its own profile, SHACL and config); the server resolves the repository per request. This is an architectural prerequisite for RDF4J and for per-repository reasoning, so it's done cleanly first. | Two repositories with different profiles side by side; the existing single-repository API unchanged (default repository) |
| D2 | RDF4J protocol: `/repositories`, statements, transactions (begin/add/delete/commit/rollback, which map onto the mutation pipeline and its gates), contexts, size | RDF4J `HTTPRepository` client integration test; ResearchSpace connects |
| D3 | Formats: JSON-LD, Binary RDF (RDF4J), TriX if needed | Round trips; RDF4J client uses Binary RDF |
| D4 | Federation: `SERVICE` with an async client, allowlist, timeouts, SSRF guards | Federated W3C tests; blocked-target security tests |
| D5 ✅ | Graph Store Protocol status codes | W3C GSP behaviour for missing graphs (done with U1) |

### Phase E: governance and operations (6.1, 6.3), M–L

| Slice | Scope | Done when |
|---|---|---|
| E1 | RBAC enforced in the store: roles (reader, editor, admin), graph-level read/write rules, applied to SPARQL, GSP and RDF4J alike | Security matrix across all read and write paths |
| E2 | Observability: request outcomes and latency, active/queued work, WAL/checkpoint/compaction/backup metrics | Induced failures show up in metrics and logs; example alerts |
| E3 | Backup and restore contract: versioned manifest, consistent online backup during commits, restore into a clean install, upgrade compatibility | Automated drill |
| E4 | Admission control: aggregate memory across concurrent queries, fallback paths included | Bounded peak under concurrent load; clear reject outcomes |
| E5 | Packaging: embedded console assets, runtime console config, authenticated console flows, reader capabilities | Clean-install walkthrough under auth |

### Phase F: specialised indexes (5.1–5.3), L each

| Slice | Scope | Done when |
|---|---|---|
| F1 | Full-text search: an inverted index per repository, synchronised from commit deltas; native functions plus `luc:` (GraphDB) and `bds:` (Blazegraph/ResearchSpace) shims | ResearchSpace keyword search unchanged; consistent after crash recovery |
| F2 | GeoSPARQL: WKT/GeoJSON, an R-tree maintained from deltas, `geof:` functions, index-aware rewrite | GeoSPARQL core + topology compliance subset |
| F3 | Vector similarity: an HNSW index, nearest-neighbour SPARQL extension | Recall@k against exact search reported |
| F4 | RDF-star / RDF 1.2 triple terms | RDF 1.2 tests |

Secondary indexes (F1–F3) follow the review's publication rule: an index names the revision it describes, and readers never see data and an index from different revisions.

## 4. Order and parallelism

- **A before everything.** It's small, and it fixes the boundaries everything else crosses.
- **C1 (the SHACL validator) next** (owner decision 3). It needs nothing from B or D.
- **B alongside, once its oracle exists.** B1's gate compares against GraphDB and RDFox, which are set up at the office with the owner. B3 and B4 don't need them.
- **D1 (multi-repository) before C2, D2 and E1.** The SHACL commit gate, RDF4J and security are all per repository. Retrofitting that later would touch every one of them.
- **Then D2 and F1** (RDF4J, full-text), which ResearchSpace needs.
- **Performance work continues alongside, profile-driven.** No benchmark rounds until asked.

## 5. Codebase health rules for every slice

- **Ownership as in [ARCHITECTURE](../ARCHITECTURE.md):**
  - engine: storage;
  - exec: kernels;
  - sparql: semantics;
  - reasoner: rules;
  - store: orchestration and gates;
  - server: transport and policy.
- New subsystems (SHACL, full-text, geo) are their own crates or store modules behind the same publication boundary.
- Configuration: typed and validated. Unknown keys fail. Tuned defaults, with use-case modes exposed (not every internal heuristic).
- Every slice removes what it replaces. No parallel old paths.
- Evidence per slice: differential or conformance tests; mutation checks for new test oracles.

## 5a. The path to a package the Datamodel Workflow and ResearchSpace can use

Owner request of 30 September 2026. The parity phases stay as they are; this is the order in which their slices make the two consumers work, with the small compatibility work they need first.

**What each consumer does to a store:**

| Consumer | How it talks to the store | What it needs beyond that |
|---|---|---|
| Datamodel Workflow (its plan, WP15 and D18) | SPARQL 1.1 Update and the Graph Store Protocol for export, one named graph per module and version; SPARQL queries (competency questions, provenance); re-exporting a version must give identical results | Optionally SHACL validation in the store (its shapes are the ABox contract); login through the installation's identity provider |
| ResearchSpace | RDF4J's client: either a SPARQL repository (one endpoint URL for queries and updates, long weighted `Accept` lists) or an RDF4J HTTP repository; named graphs for its LDP containers, written with SPARQL Update | Keyword search (`bds:search` in its stock templates) |

**Steps:**

| Step | For | Scope | Done when |
|---|---|---|---|
| U1 ✅ | both | The HTTP surface as real clients use it: content negotiation with weights and wildcards; the default graph as the merge of all graphs (a setting); one SPARQL endpoint for queries and updates; `using-graph-uri`; Graph Store status codes (D5); N-Quads, TriG and JSON-LD for graph results | Protocol tests with RDF4J's and common HTTP clients' request shapes (`client_compat_tests.rs`); reference: [http-api.md](../ops/http-api.md) |
| U1b ✅ | both | Native executor support for the union default graph: scans, index joins, property paths, aggregates and updates read all graphs in graph-last order and drop repeated statements | Differential tests against the general evaluator (4,700 random queries, targeted shortcut tests, updates; each deduplication site mutation-checked). Open: its speed against default mode, in the next benchmark batch; range filters and the worst-case-optimal join still apply to the plain default graph only |
| U2 ✅ | DMW | SHACL validation over HTTP (C1b): the repository's shapes graph, or shapes sent with the request | A report as RDF and JSON for DMW's shapes |
| U3 ✅ | both | The package: a container image with the console built into the binary, a compose file with ResearchSpace, a quick start, one integration guide per consumer | Built and run on Linux: 152 MB image, runs as its own user, data in a volume survives a restart. Guides: [datamodel-workflow.md](../integration/datamodel-workflow.md), [researchspace.md](../integration/researchspace.md) |
| U4 ✅ | both | Smoke tests against the real clients: the ResearchSpace platform in Docker on NRESE; DMW's export protocol replayed | `scripts/smoke-dmw-export.sh` and `scripts/smoke-researchspace.sh` pass against the image. Found and fixed on the way: the default graph as the merge of all graphs, `CLEAR`/`DROP` of empty graphs, request size limits, OpenSSL in the binary, the console's undeclared dependency. Open: ResearchSpace's keyword search (U5); its pages and forms in a browser |
| U5 | ResearchSpace | Full-text search and the `bds:search` shim (F1) | ResearchSpace's keyword search works on its stock templates |
| U6 | ResearchSpace | Multi-repository (D1), then the RDF4J protocol (D2) | The RDF4J HTTP repository type connects |
| U7 | DMW | SHACL on commit (C2) | A commit that breaks a shape is rejected with the report |

U1 to U4 make a prototype both can run against. U5 to U7 remove the remaining workarounds (regex-based search templates in ResearchSpace; validation only on request for DMW).

## 5b. What a real integration workload asks of the engine

Found on 30 September 2026 with the RG × GS × GND integration workload ([benches/integration](../../benches/integration/README.md); measurements in [reasoning-benchmark.md](../design/reasoning-benchmark.md), §7): persons of three sources linked by `owl:sameAs`, reasoned under OWL 2 RL, queried with the project's competency questions. The workload is small on its example data (66 k statements after reasoning) and already showed each of these.

| Step | Scope | Done when |
|---|---|---|
| W1 ✅ | `HAVING` may name a `SELECT` alias (a query returned no rows here and rows on Fuseki) | `compat.rs`; unit tests and a store test |
| W2 ✅ | Filter pushdown below joins, OPTIONAL, UNION, MINUS, BIND and into subqueries; inside a basic graph pattern, each conjunct runs after the first join step that binds its variables | `native/pushdown.rs`, `Context::filter_bound`; differential tests with solutions, each rule mutation-checked. CQ06 11 s → 0.7 ms, CQ09 280 → 1.5 ms, CQ08 from "memory budget exceeded" to 0.29 s |
| W3a ✅ | A property path joined to a pattern that binds one of its ends starts from those values (it was computed for every node of the graph) | `Context::path_from`; 3,000-query differential test, mutation-checked. CQ03 3.5 → 0.5 ms, CQ10 3.3 → 0.3 ms |
| W3b | **One join order per group.** Triple patterns are ordered by cost only within a basic graph pattern; a property path, a subquery or an OPTIONAL splits the group, and the parts are joined as written. Filters don't yet inform the order (a selective filter should pull its pattern to the front) | The group's patterns, paths and subpatterns are ordered together, with filter selectivity; differential tests; the questions' plans show it |
| W4 | **Equality by representatives.** OWL 2 RL's equality rules copy every statement to every identity of its terms: 883 `owl:sameAs` statements for 65 resources, and answers repeated per identity. Keep one representative per identity group in the inferred stack and expand in answers on request, as a reasoning mode (GraphDB and RDFox document this optimisation). Belongs with the profiles of phase B | Closure and answers equal the replicated ones after expansion (property test); closure size and the questions' times recorded |
| W5 ✅ | **Aggregates over independent OPTIONALs without their product,** and joins on sets where the consumer ignores duplicates (`SELECT DISTINCT`; groups of `DISTINCT`, `MIN`, `MAX`, `SAMPLE` aggregates). CQ05 joined a person's dates, places, affiliations and co-mentions: about 10¹⁰ rows on the example data | `native/sets.rs`: an OPTIONAL that only feeds aggregates is joined to the distinct rows of the rest by itself; a `HAVING` over the rest prunes groups before. 3,600-query differential test against the as-written evaluation and spareval, each rule mutation-checked. CQ05: from the memory budget to 9,009 rows in 0.6 s |
| W6 | **Bounded memory for large intermediate results.** The executor builds each operator's result as a table. A query that fails at the 4 GiB budget had the server at 5.8 GB. Pipelined execution through joins into aggregation, and admission control over all running queries (E4) | Peak memory of the question mix stays within the configured budget |
| W7 | **Types in unnamed classes.** With the GND ontology, 120,627 of 219,840 inferred statements say that a resource is a member of an unnamed `owl:unionOf` class (the ontology's domains and ranges). A mode that derives such a membership only where a rule or a query needs it | Inferred set equal on named classes; closure size recorded |

Order: W3b and W6 first (they decide whether the larger tiers answer at all), then W4 with phase B, then W5, W7. The [audit](../reviews/2026-09-30-benchmark-suite-and-readiness-audit.md) puts these next to the other production gaps.

## 6. Owner decisions (30 September 2026)

1. **Dependencies for the specialised indexes (F1–F3): use libraries, replace what underperforms.**
   - A library is allowed if its licence permits commercial use without conditions on our code (MIT, Apache-2.0, BSD, and similar), and it is maintained.
   - Each one sits behind our own trait, is measured in the perf lab, and is replaced by our own Rust implementation where it costs us: an architecture that doesn't fit the engine (its own storage, its own threading), or measured inefficiency.
   - Starting points: tantivy (MIT) for full-text, an R-tree crate for geo, and our own HNSW unless a crate measures better.
2. **Licence model: MIT OR Apache-2.0**, as Oxigraph (changed by the owner on 1 October 2026; it replaces the decision below, kept for the record). `LICENSE-MIT` and `LICENSE-APACHE` hold the texts, the Cargo manifests say `MIT OR Apache-2.0`, and contributions come in under the same terms (Apache-2.0 §5), so no contributor agreement is needed. Nothing under the AGPL had been published. Dependencies stay permissive. Open for a lawyer still: the legal name on the copyright line, an employer's claim, and copyright in AI-assisted code.

   *Superseded:* **AGPL-3.0-only, plus commercial licences from the copyright holder** (decided 30 September 2026, after the explanation).
   - `LICENSE` holds the AGPL text; the Cargo manifests say `AGPL-3.0-only`. "Only" and not "or later", because widening to later versions stays possible and narrowing doesn't.
   - Checked before applying it: all 283 third-party crates are permissively licensed; the owner is the only human author; the engine-v2 branch had never been published.
   - The v1 prototype on `main` (up to `8088e9a`) was published with `Apache-2.0` in its metadata and stays available under that.
   - Outside contributions need a contributor agreement ([CONTRIBUTING.md](../../CONTRIBUTING.md)); none exists yet, so none are merged.
   - **Open points for a lawyer before the first sale:** the contributor agreement and the commercial licence text; the legal name on the copyright line; whether an employer has a claim; and how far copyright protects the parts written with an AI assistant, which differs by country.
   - Packaging (E5) must ship the third-party notices with binaries.
3. **Order: SHACL (phase C) before RDF4J and full-text (D2, F1).**
4. **Implementation language:** protocols and formats still to come (RDF4J, Binary RDF, JSON-LD, federation) are implemented natively in Rust, in the server and its crates. No JVM sidecar, no wrapped reference implementation, no interpreted glue on a request path.

**Still to do with the owner, at the office:** set up the licensed systems (GraphDB, RDFox, Stardog, AnzoGraph) under evaluation agreements. Their results stay local and unpublished.
