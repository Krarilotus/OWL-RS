# NRESE Engine v2: Implementation Roadmap

Status: **agreed** (2026-09-25; decisions D1–D4 confirmed, see section 8). Execution starts with Milestone 0.
Architecture: [ARCHITECTURE.md](ARCHITECTURE.md) · decisions: [adr/](adr/) · capability tracking: [spec/06-target-capability-matrix.md](spec/06-target-capability-matrix.md)

---

## 1. Goal and how we'll know we're there

NRESE becomes a single-node RDF database that **reads like QLever and governs data like GraphDB**, and serves as the store behind ResearchSpace and the datamodel workflow (DMW).

"Done" is defined by evidence, not by features:

| Target | Evidence |
|---|---|
| GraphDB semantic parity | LUBM materialisation (RDFS, OWL-Horst, OWL 2 RL) matches GraphDB's inferred-triple counts; incremental deletes match full recomputation; W3C SHACL Core test suite passes; SPARQL 1.1 W3C test suite passes |
| QLever performance class | Published query mixes (WDBench/WDQS-style subsets, BSBM) within an agreed factor of QLever on the same hardware; load throughput and bytes/triple reported |
| GraphDB operational parity | RDF4J protocol clients (including ResearchSpace) work unchanged; transactional updates are sub-linear in dataset size; online backup; graph-level access control |
| Our own users | ResearchSpace runs against NRESE with adapted search templates; DMW's WP15 export round-trips; the RG dataset at full size meets agreed latency budgets |

## 2. How we plan

**The friend's list is guidance, not scope.** Section 6 maps every item: some are adopted as written, some adapted, some deferred, one rejected, with a reason for each. The list is GraphDB-centric and leaves out three things we need:
- the QLever performance side (compression, joins, streaming, bulk load)
- the correctness defects found in the audit
- the evidence infrastructure (test suites, benchmarks) that makes "parity" checkable

**Sequencing rules:**
1. **Foundations before features.** Reasoning, SHACL, text and geo indexes all consume the same three engine primitives: term IDs, snapshots and deltas. Building features on the v1 string-based snapshot would mean writing them twice.
2. **Evidence infrastructure first.** Differential tests (against a naive model and against spareval as the SPARQL oracle), W3C suites and the benchmark harness exist *before* the code they judge.
3. **Downstream need decides order within a tier.** ResearchSpace needs keyword search and robust updates. DMW needs SHACL (D18) and provenance queries. GraphDB parity needs reasoning.
4. **Every work package ends with a gate:** tests green, benchmark recorded, docs updated, and a reactor pass that removes whatever the package made obsolete. No parallel "old and new" paths survive a milestone.
5. **Reuse where it isn't our differentiator:** Oxigraph's parsers and SPARQL evaluator (ADR-0001), tantivy for text, rstar for geometry. Build ourselves where performance or semantics are the product: storage, joins, reasoning, SHACL.

**Sizes** are rough effort for one experienced Rust developer, focused: S ≤ 1 week, M 1–3 weeks, L 3–6 weeks, XL > 6 weeks. The whole roadmap is on the order of a year for one person. That's normal for this class of system: GraphDB and QLever are each many person-years.

## 3. Milestones at a glance

```
M0 Foundation & cleanup ──► M1 Engine core ──► M2 Governance ──┬──► M4 Performance (QLever track)
                                              (SHACL, FTS,      │
                                               RDF4J, RS)       ├──► M3 Reasoning (GraphDB track)
                                                                └──► M5 Geo & vectors ──► M6 Enterprise
```

M3 and M4 are independent after M2 and can run in parallel with two people. The order M2 → M3 is a proposal (decision D1 in section 8).

---

## 4. Milestones and work packages

### M0: Foundation & cleanup (size M)

Goal: a codebase where every concern has one owner, and the evidence tooling needed by later milestones exists.

| WP | Scope | Layer | Done when |
|---|---|---|---|
| **F1 Ownership refactor** | Move the mutation pipeline and update lock from `nrese-server` into `nrese-store`. The server becomes transport-only. One error enum per crate. Module docs state ownership. | L3/L4 | `nrese-server` has no data-semantics code; all 278 existing tests green |
| **F2 Config cleanup** | Ontology preload becomes optional with no RG default paths (audit F5). The unimplemented `owl-dl-target` mode is removed (F8). Unknown config values become startup errors instead of silent defaults. The AI assistant stays a self-contained, runtime-disabled-by-default module; a Cargo feature was rejected because it would scatter `cfg` across ~10 sites while the HTTP client stays needed for OIDC anyway. | L4 | Server starts from any directory with an empty config |
| **F3 Docs and spec reset** | ARCHITECTURE, ADRs, this roadmap and the target capability matrix replace the Fuseki gap matrix. Fuseki-specific docs move to `docs/archive/`. | docs | One status source; no document claims Fuseki parity as the goal |
| **F4 Evidence infrastructure** | Harness retargeted to engine-neutral reference profiles (QLever, GraphDB, Fuseki as a correctness reference). Ontology fixtures vendored so tests are hermetic. Baseline numbers of the current v1 recorded (`write-scaling` command). Suite runners and dataset generators move to the milestone whose gate first needs them, so they're built against the engine they measure: W3C SPARQL 1.1 → Q1, SHACL → S1, OWL 2 RL and LUBM → R2, BSBM and WDBench → Pf3. | tooling | `write-scaling` reproduces the audit numbers (`benches/baselines/`) |
| **F5 Repo hygiene** | Remove stray `target-*` directories and the checked-out `cargo-target` under `artifacts/` (with the user's OK), plus CI for fmt/clippy/tests. | repo | Clean working tree; CI green |

**M0 status (2026-09-25):** F1–F4 done; F5 CI (`.github/workflows/ci.yml`: fmt, clippy `-D warnings` and tests for the workspace and harness, plus console typecheck/test/build) is in place and green locally. D6 is done: the stray build directories were deleted. **M0 is complete.** Side findings fixed along the way: the SOSA catalog URL actually served SSN, so the SOSA reasoning test had asserted an SSN axiom; and the console build had skipped type-checking, which hid six TypeScript errors.

### M1: Engine core (size XL). Replaces Oxigraph's store and fixes audit F1, F2, F3 (storage side), F6

| WP | Scope | Layer | Done when |
|---|---|---|---|
| **E1 Terms & dictionary** | `TermId` (tagged u64); arena + SwissTable dictionary. Inline canonical `xsd:integer`, `xsd:boolean`, `xsd:dateTime`, `xsd:date`, `xsd:decimal` (bounded), only when the lexical form is canonical. Blank-node scoping per load. | L1 | Round-trip property tests over all term shapes; no RDF-identity deviations (unlike QLever's numeric folding) |
| **E2 Permutation index** | Six permutations (`SPOG POSG OSPG GSPO GPOS GOSP`); immutable sorted runs; k-way merge scans with sign-sum visibility; size-tiered compaction (sync for small merges, background for large). | L1 | Differential test against a `BTreeSet` model over random operation sequences; every pattern is O(log n + k) |
| **E3 MVCC & transactions** | `Arc` snapshots; single writer slot; exact deltas; overlay view (snapshot + pending) so later update operations see earlier ones; abort = drop. | L1 | Readers never block (tested under a concurrent writer); snapshot isolation tests |
| **E4 Durability** | Segmented redo WAL (length + CRC32), fsync policy, checkpoints (atomic rename), recovery with torn-tail truncation; revision is persistent. RocksDB and the libclang dependency go away. | L1 | Crash-injection tests (kill at every write step) recover to the last acknowledged revision; audit F6 closed |
| **E5 Bulk load** | Parallel parse → intern in batches → parallel sort (IPS4o-class sorting, via rayon for now) → direct base-run build, bypassing the per-commit path for restore and initial loads. | L1 | Loads 100 M triples; throughput recorded against QLever and GraphDB loaders |
| **Q1 SPARQL adapter** | `spareval::QueryableDataset` over snapshots. Cancellation tokens wired to request deadlines. Protocol dataset parameters (`default-graph-uri`, `named-graph-uri`, `using-*`). Streaming result serialisation. Update planning into deltas (all `GraphUpdateOperation`s, `LOAD` behind policy). | L2 | W3C SPARQL 1.1 query + update suites at spareval's pass rate; audit F2 closed (nothing commits after a timeout) |
| **P1 Mutation pipeline** | plan → validate → deadline check → commit, all proportional to the delta. The v1 reasoner is fed through an adapter until M3. | L3 | One-triple insert at 10 M triples < 5 ms with reasoning off (audit: 2.9 s at 1 M) |
| **P2 Reactor** | Delete the Oxigraph `Store` usage, the staging clone and the string snapshots. | all | `grep oxigraph::store` is empty |

**M1 status (2026-09-25):** `nrese-engine` is in the workspace and CI.
- **E2–E4 done** and gated:
  - `index/model_tests.rs`: random operation sequences against a `BTreeSet`, all 32 pattern shapes, named-graph listing, policy and arbitrary compaction windows, old versions after compaction.
  - `tests/engine_tests.rs`: snapshot isolation, overlay, abort, readers during an open transaction, three concurrent readers never seeing a partial commit, and an RDF-term model test.
  - `tests/durability_tests.rs`: torn tail cut at every byte of the last record, mid-log corruption reported rather than skipped, crash during a checkpoint and between the checkpoint and WAL release, dictionary continuity across aborted transactions, directory lock, and background checkpoints bounding the WAL.
- **E1 partial:** inline `xsd:integer` and `xsd:boolean` only; `dateTime`, `date` and `decimal` are still open. Blank-node scoping per load belongs to the loader (Q1/E5).
- **Q1 partial:** the `nrese-sparql` crate exists.
  - Done: the spareval adapter over snapshots and transactions (one `ReadView` contract); SPARQL Update into one transaction without committing, so the pipeline decides; protocol dataset overrides; cancellation between and inside operations.
  - Evidence: `tests/differential_tests.rs` runs 30 queries against Oxigraph as an oracle, before and after each of 14 update scripts, with no mismatches.
  - Two intentional differences, each pinned by a test: literal lexical forms are preserved, and a graph exists only while it holds quads.
  - Open: streaming result serialisation, the W3C suite runner, `LOAD` behind a policy, and blank-node scoping for file loads.
- **E5, P1, P2 open.**
- **Engine-level numbers** (the `insert_latency` example; 7800X3D, Windows 11, NVMe; recorded in `benches/baselines/README.md`): one-quad commit at 10 M quads is 3.8 µs p50 in memory, and 1.9 ms p50 / 4.4 ms p99 durable (fsync per commit). The P1 gate itself is end-to-end over HTTP and is measured once P1 lands.

### M2: Governance and integration (size XL). What DMW and ResearchSpace need first

| WP | Scope | Layer | Done when |
|---|---|---|---|
| **S1 SHACL Core** | New `nrese-shacl` crate. Shapes compiled to an ID-based program: all SHACL Core constraint components, all property-path forms (sequence, alternative, inverse, zero-or-more, one-or-more, zero-or-one), severities, `sh:ValidationReport` graphs plus JSON. | L2 | W3C SHACL Core test suite passes |
| **S2 Incremental commit gate** | Target index (`targetNode/Class/SubjectsOf/ObjectsOf`). Affected-focus-node computation from Δ⁺/Δ⁻ through inverse shape paths. Block on the configured severity; the report goes back in the HTTP problem response. | L2/L3 | Validation cost proportional to affected nodes (benchmark); a full revalidation agrees with the incremental one on random deltas |
| **S3 SHACL-SPARQL** | `sh:sparql` constraints and SPARQL-based targets via `nrese-sparql`. | L2 | W3C SHACL-SPARQL tests pass |
| **T1 Full-text search** | tantivy index per repository, synchronised from commit deltas. Native `nrese:` search functions; shims for GraphDB `luc:` (query, score, soundex) and Blazegraph `bds:search` (ResearchSpace). Prefix/autocomplete. | L1/L2 | ResearchSpace keyword search works unchanged via the `bds:` shim; index stays consistent under crash recovery |
| **X1 RDF4J protocol** | `/repositories`, repository management, the transaction API (begin/add/delete/commit/rollback), statement endpoints, formats (TriG, N-Quads, JSON-LD, RDF/XML, Binary RDF). Multi-repository: one engine instance per repository. | L4 over L3 | RDF4J `HTTPRepository` client integration test; ResearchSpace connects |
| **X2 Federation & LOAD** | `SERVICE` via spareval's handler with an async HTTP client, allowlist, timeouts and SSRF guards. `LOAD` behind policy. | L2/L4 | Federated W3C tests pass; security tests for blocked targets |
| **RS1 ResearchSpace integration** | Repository config, search templates on the native syntax, a named-graph/LDP workflow test, performance with realistic editing load. | integration | A documented RS setup runs the RS form/LDP workflow end to end |

### M3: Reasoning, the GraphDB track (size XL). Closes audit F3 (reasoner side) and F4

| WP | Scope | Layer | Done when |
|---|---|---|---|
| **R1 Rule IR & rulesets** | Own rule IR. Parser for GraphDB `.pie` files as an import format. Built-in `rdfs`, `rdfs-plus`, `owl-horst`, `owl2-rl`, `owl2-ql` (GraphDB's materialisable variants). | L2 | Rulesets load; `.pie` round-trip tests on GraphDB's published rulesets |
| **R2 Semi-naive materialisation** | Rules compiled to join plans over permutation scans (ID columns, no strings). Semi-naive delta iteration, parallel per rule/stratum (RDFox-style). `owl:sameAs` via union-find rewriting. | L2 | LUBM(1/10/100) inferred counts equal GraphDB's for each profile; throughput recorded |
| **R3 Inferred layer & read models** | A separate inferred index stack with the same layout. Queries see asserted ∪ inferred by default. GraphDB pseudo-graphs `onto:explicit` / `onto:implicit`; per-request asserted-only switch. | L1/L2 | Query tests for all three read models; audit F4 closed |
| **R4 Truth maintenance** | Incremental insert (semi-naive from Δ⁺); incremental delete via Backward/Forward (GraphDB's `isSupported` behaviour), with DRed as the fallback. | L2 | Differential test: incremental equals from-scratch after random insert/delete sequences |
| **R5 Consistency & explanations** | Consistency rules (`owl:Nothing`, disjointness, `differentFrom`, irreflexive/asymmetric, property disjointness, max-cardinality 0/1 violations in owl2-rl). Derivation-tree explanations in reject responses. Atomic rollback. | L2/L3 | v1 `rules-mvp` fixture suite passes on v2; then `rules-mvp` is deleted |

### M4: Performance, the QLever track (size XL)

| WP | Scope | Layer | Done when |
|---|---|---|---|
| **Pf1 Compressed runs** | Block layout (e.g. 64 KiB blocks, first-key directory, per-column delta + SIMD bit-packing / Stream VByte). Static search over the block directory (Eytzinger/S-tree or PGM-index). | L1 | Bytes/triple reported and compared with QLever; scan throughput not worse than uncompressed |
| **Pf2 On-disk runs** | Memory-mapped immutable run files; the base run is no longer RAM-resident; checkpoint = write run files. | L1 | Datasets larger than RAM load and query |
| **Pf3 Native BGP executor** | Vectorised ID tables; merge joins on shared permutation order; worst-case-optimal joins (Leapfrog Triejoin / Free Join style) for cyclic patterns; adaptive choice between binary and WCOJ joins (Umbra-style). spareval handles the remaining operators; differential tests against spareval. | L2 | Query mix within the agreed factor of QLever; no result differences against the oracle |
| **Pf4 Statistics & optimiser** | Per-predicate counts, characteristic sets (Neumann & Moerkotte) for cardinality estimation, DP join ordering; `EXPLAIN` / profile output with operator timings and scan counts. | L1/L2 | Estimation error tracked; plans explained in the API |
| **Pf5 Dictionary at scale** | Sorted, front-coded or FSST-compressed main vocabulary (on disk); hash index only for the delta vocabulary; checkpoint rebuild reclaims unreferenced ids. | L1 | Dictionary bytes/term reported; lookups stay O(log n) or better |

### M5: Geospatial & vectors (size L)

| WP | Scope | Done when |
|---|---|---|
| **G1 GeoSPARQL** | WKT and GeoJSON literals; an R*-tree (rstar) maintained from deltas; `geof:sfWithin`, `sfIntersects`, `distance`, etc.; a query rewrite to use the index. | OGC GeoSPARQL compliance tests (core + topology subset) |
| **V1 Vector similarity** | An HNSW index over embeddings stored as literals or attached vectors; a nearest-neighbour SPARQL extension. Relevant to DMW's embedding service. | Recall@k vs exact search reported |

### M6: Enterprise operations (size L)

| WP | Scope | Done when |
|---|---|---|
| **O1 Security** | RBAC with graph-level read/write rules, enforced in the store (not the server), so SPARQL, GSP and RDF4J paths can't diverge. | Security test matrix across all write/read paths |
| **O2 Online backup/restore** | Snapshot-consistent online backups (MVCC makes them non-blocking); point-in-time restore from checkpoint + WAL. | Drill documented and automated |
| **O3 Observability** | Transaction, compaction, reasoning, SHACL and cache metrics; per-query resource accounting and throttling; structured `tracing` spans end to end. | Dashboards documented |

---

## 5. Evidence plan

| Kind | What | Used from |
|---|---|---|
| Model tests | Engine vs a naive `BTreeSet` model on random operation sequences; incremental vs from-scratch reasoning; incremental vs full SHACL | M1, M2, M3 |
| Oracle tests | Native executor vs spareval; NRESE vs a reference endpoint through the harness | M1, M4 |
| W3C suites | SPARQL 1.1 (query, update, protocol, federation), SHACL (core, SPARQL), OWL 2 RL conformance, RDF 1.1/1.2 syntax (through the parsers), GeoSPARQL | per milestone |
| Benchmarks | LUBM (reasoning), BSBM (mixed read/write), WDBench/WDQS subsets (QLever comparison), RG real data (our use case) | baseline in M0, then every milestone |
| Crash tests | Fault injection at every WAL/checkpoint step | M1 |

Benchmarks run on one documented machine profile. Numbers go into `docs/spec/06-target-capability-matrix.md` with dataset, version and commit.

## 6. Friend's GraphDB parity list: disposition

**A** = adopt as written, **Ad** = adopt with a design change, **D** = defer (with target), **R** = reject.

| Item | Disposition | Where | Note |
|---|---|---|---|
| 1.1 Provenance-aware quad indexing | **Ad** | E2, R3 | Instead of per-entry `is_explicit`/`is_inferred` bitflags: two index stacks (asserted, inferred) with identical layout, merged at scan time. Same queries, including GraphDB's `onto:explicit`/`onto:implicit`, but writes to asserted data never rewrite inferred entries and the LSM visibility invariant stays simple. Decision D2. |
| 1.1 RDF-star / triple terms | **A**, later | E1 extension in M2 or M3 | New `TermKind::Triple` whose dictionary key is the three component ids; aligns with RDF 1.2. Priority decision D4. |
| 1.2 Arena dictionary, sequential u64 ids | **A** | E1 | Done in the spike. |
| 1.2 Inline native literal types | **A** | E1 | Integer, boolean, dateTime, date, decimal, **only for canonical lexical forms**, so RDF term identity is preserved. |
| 1.2 Dictionary cache eviction | **D** | Pf5 | Only meaningful once the dictionary lives on disk. |
| 1.2 Rollback of dictionary ids on abort | **R** | — | Ids from aborted transactions are simply unreferenced. Rolling them back would force readers and the single writer to coordinate on id reuse, for no functional gain. Unreferenced entries are reclaimed when a checkpoint rebuilds the vocabulary (Pf5). |
| 1.3 MVCC, WAL, checkpoint, compaction | **A** | E2–E4 | |
| 2.1 `.pie` rule language | **Ad** | R1 | Supported as an import format; the engine uses its own rule IR. |
| 2.1 rdfs, owl-horst, owl2-rl, owl2-ql | **A** | R1 | Plus `rdfs-plus`. |
| 2.1 Rules compiled to ID-level code | **A** | R2 | Rules compile to join plans over permutation scans. |
| 2.2 Semi-naive fixpoint, consistency rules | **A** | R2, R5 | |
| 2.3 TMS / `isSupported`, atomic rollback | **A** | R4, R5 | Backward/Forward is the published algorithm for exactly this. |
| 3.1 SHACL Core, paths, SHACL-SPARQL | **A** | S1, S3 | |
| 3.2 Pre-commit, target index, incremental Δ⁺/Δ⁻, report | **A** | S2 | |
| 4.1 SPARQL 1.1 query/update/GSP | **A** | Q1 | Largely covered by spareval. |
| 4.1 Federated `SERVICE` | **A** | X2 | With SSRF protection. |
| 4.2 RDF4J protocol incl. transactions | **A** | X1 | Also gives ResearchSpace a native repository type. |
| 4.2 Binary RDF and other formats | **A** | X1 | |
| 5.1 FTS with tantivy, auto-sync, `luc:` predicates | **A** | T1 | Plus a `bds:search` shim for ResearchSpace (ADR-0006). |
| 5.2 GeoSPARQL, R-tree | **A** | G1 | |
| 5.3 HNSW vector index | **A** | V1 | |
| 6.1 Online backup, parallel bulk loader | **A** | O2, E5 | |
| 6.2 Cost-based optimiser, explain/profile, timeouts | **A** | Pf4, Q1 | Timeouts with real cancellation already come in M1. |
| 6.3 RBAC + graph-level security, Prometheus, tracing | **A** | O1, O3 | Prometheus and tracing exist in v1 and are extended. |

**Missing from the list and added:**
- **Correctness defects from the audit:** O(dataset) writes, commit-after-timeout, and the non-persistent revision (M1).
- **The whole QLever performance track:** compression, on-disk runs, WCOJ joins, statistics, streaming results, scale-out dictionary (M4).
- **Evidence infrastructure:** W3C suites, LUBM/BSBM/WDBench, crash tests (M0 onwards).
- **Multi-repository support** (X1) and **ResearchSpace integration** (RS1).

## 7. Risks

| Risk | Mitigation |
|---|---|
| The scope is large relative to team size | Milestones are independently useful. M1 alone makes NRESE a correct, fast-writing SPARQL store; M2 alone covers DMW/ResearchSpace needs. Re-plan at each gate. |
| Semantic drift from GraphDB rulesets | Compare inferred-triple counts on LUBM and on our own data against a real GraphDB instance (a Free edition is enough for testing). |
| The spareval evaluator is slow on complex queries until Pf3 | Acceptable for M1–M3 (it's Oxigraph-class). Pf3 targets the hot operators first, guided by profiles. |
| Memory before compression (192 B/quad) | Fine up to ~50 M quads on a 16 GB machine. Pf1/Pf2 are needed before RG-scale-plus-provenance growth exceeds that; the trigger is monitored. |
| Oxigraph crate API churn | Versions pinned; upgrades are explicit work items. |

## 8. Decisions

| # | Question | Decision |
|---|---|---|
| D1 | After M1, what comes first: governance (SHACL, FTS, RDF4J, ResearchSpace) or reasoning? | **Governance first** (decided 2026-09-25). DMW (D18) and ResearchSpace need it; reasoning builds on the same engine afterwards. |
| D2 | How to mark inferred data: separate asserted/inferred index stacks, or per-entry flags? | **Separate stacks** (decided 2026-09-25; see 1.1 above). |
| D3 | Full-text engine: tantivy, or our own inverted index? | **tantivy** (decided 2026-09-25). |
| D4 | RDF-star / RDF 1.2 triple terms: early (M2) or with reasoning (M3)? | **M3** (decided 2026-09-25), unless a concrete DMW/RS need appears earlier. |
| D5 | Is the v1 `rules-mvp` reasoner kept running through an adapter during M1–M2? | Default **yes**, then deleted in R5. Reversible; not yet explicitly confirmed. |
| D6 | May the stray build directories in the working copy be deleted (F5)? | **Yes** (decided 2026-09-25): 23 untracked `target-*` directories (about 50 GB) were removed. |
