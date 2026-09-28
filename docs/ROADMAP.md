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

### 3.1 Execution order from 2026-09-27 (supersedes the diagram's order; D1 revised, D11)

The user's direction: make NRESE as strong as possible as a package of its own. That means efficient basics, maximum performance across the board, and industry-leading reasoning, query and operations performance. So **M4 and M3 come before M2**, in five phases:

| Phase | Work packages | Why this order | Gate (all measured, recorded in `benches/baselines/`) |
|---|---|---|---|
| **1. Speed foundation** (S) | **Pf7a** toolchain and codegen (Rust 1.98.1, release profile, mimalloc); **PL** perf lab (in-process runner for the scorecard and reasoning query sets, p50/p99, profiling recipe) | It lifts every later number, and every later WP needs a fast, in-process measurement loop instead of the Docker scorecard | Baseline per query recorded; no regressions |
| **2. Execution core and native queries** (XL) | **XC** shared execution core (D11); **Pf4** statistics; **Pf3** native executor for BGPs, joins, filters, aggregates, ORDER/LIMIT and OPTIONAL, with spareval kept for the rest | Queries are the largest gap: QLever is 10–2000× faster. The reasoner's batch executor (§4.1 of the design) needs the same operators | Each scorecard query within 2× of QLever, or faster; no result differences against spareval or the W3C suite |
| **3. Reasoner v2** (XL) | **R1–R4**, then **R5**, **R6**, as designed, on the execution core | Reasoning is the headline (D9) | Inferred sets identical to the reference on LUBM and OWL2Bench; ≥ 10× Nemo/Jena on RT1; OWL2Bench RL(1) < 1 s; commit-path p50 ≤ 1 ms |
| **4. Storage efficiency** (L) | **Pf2** memory-mapped runs with instant restart, **Pf1** compressed blocks, **Pf5** dictionary at scale, **E7** loader v2 | Serving memory (24 GiB at 67 M against 0.3 GiB), restart (21 s against 1 s) and bytes/triple (42–57 against 17–42) | Restart < 1 s at 100 M; bytes/triple ≤ QLever; load stays fastest |
| **5. Operations** (M) | **Pf6** group commit, sync policies, write-path isolation, presets; **O2** online backup; **O3** metrics for the new components | Write p99 under read load is 0.2–2 s now | Write p99 < 20 ms under 8 readers with group commit |

Phase 4 moves earlier if memory blocks the reasoning benchmarks at LUBM(1000). **M2 (governance) follows**, unless DMW or ResearchSpace need a piece of it sooner.

**Status (2026-09-27):**

Phase 1 is done:
- **Pf7a:** Rust 1.98.1 pinned; fat LTO with one codegen unit; mimalloc. Together that's 12–35% on the perf lab's query sets and 28% on loads.
- **PL:** `benches/perf-lab.sh` with committed reports in `benches/baselines/perf-lab/`.

Phase 2 is under way:
- **XC1:** order-preserving integers, literal kinds, GPSO/PSOG, exact counts, sorted, ranged and group-counted scans; format v4.
- **XC2:** `nrese-exec`, which holds id tables, joins, grouping, memory budgets and graph closure.
- **XC3:** the native executor, covering BGP, FILTER (compiled id-level predicates and range pushdown), OPTIONAL, UNION, MINUS, (NOT) EXISTS, BIND, VALUES, aggregates, ORDER BY, LIMIT and property paths. Unsupported queries fall back to spareval.

Evidence:
- The Oxigraph-oracle differential suite.
- The W3C suite, unchanged at 485/495.
- 7,500 randomized native-versus-spareval queries.

Perf lab against the same build without the native executor (sum of p50):

| Dataset | Speedup |
|---|---|
| Olympics | 5.6× |
| entities 10 M | 16× |
| DBpedia 67 M | 8.9× |

Single queries are up to 10³–10⁶× faster (counts, grouped counts, star joins, date ranges).

Next:
- XC4: statistics, the optimiser and EXPLAIN.
- XC5: Leapfrog Triejoin and parallelism.
- The Docker scorecard against QLever for the Phase 2 gate.

**Status (2026-09-28): Phase 3 is under way.**
- **R1:** the rule IR, the `rdfs` and `owl2-rl` rulesets (W3C rule names), list axioms compiled per axiom (every member sequence under equality), and the naive reference evaluator.
- **R2:** the batch executor. It grounds schema atoms against the TBox, runs parallel semi-naive evaluation over per-predicate sorted relations, and deduplicates by sort and merge.
- **R3 (transitive module):** SCC-condensation closure (`nrese-exec::graph::transitive_closure`) replaces transitivity rules.
- **R4a:** modes `rdfs` and `owl2-rl` materialise into the inferred stack. This happens after bulk loads, at startup (`Engine::rematerialisation`), and on every commit, inside the transaction. Consistency violations reject the commit. Queries see inferences under the default read model.

Evidence:
- The batch executor equals the naive one on 400 random ontologies covering every rule table, lists and 8 consistency rules.
- Inferred sets are identical to the owlrl oracle or Nemo on LUBM(1/10) and OWL2Bench QL/EL/RL/DL(1).
- All 14 LUBM(1) queries give the published counts through the native engine. LUBM(10) and (100) agree with Nemo plus Oxigraph.

RT1 closure times (laptop, 16 threads):

| Dataset | NRESE v2 | NRESE v1 | Jena OWL-micro | Nemo |
|---|---|---|---|---|
| LUBM(10) | 0.26 s | 1.5 s (incomplete) | | 35 s |
| LUBM(100) | 3.3 s | 24 s (incomplete) | | 398 s |
| OWL2Bench RL(1) | 0.60 s | 900 s | 1,439 s | 1,859 s |

End to end on LUBM(100) (`reason_query`): load 4.7 s, reasoning with install 4.6 s, and all 14 queries in 1.2 s.

**Status (2026-09-28, second batch):**
- **R4b, the delta executor:** commits maintain the inferred stack incrementally.
  - DRed: overdelete, rederive, then semi-naive insert. Transitive properties are closed per new edge. Only schema facts in the delta ground new rule instances.
  - A dispatch index visits only rule instances that can match the delta.
  - The ground program is cached per committed revision.
  - It is exact against rematerialisation on 48,000 random changes and on 3,000 random commits through the engine, including named graphs.
- **Read models per request:** GraphDB's `infer=false`, `FROM onto:explicit` and `FROM onto:implicit`, on both executors (differential-tested).
- **Batch executor:**
  - Two-level relations: a round no longer copies whole relations.
  - Rounds after the first re-ground through new schema facts only.

Commit latency through the store pipeline under `owl2-rl`, one ABox statement per commit (`reason_query --commits`):

| Dataset | Delete p50 / p99 | Insert p50 |
|---|---|---|
| LUBM(10) | 0.20 / 0.42 ms | 0.06 ms |
| LUBM(100) | 0.61 / 1.47 ms | 0.15 ms |

That meets the Phase 3 commit-path gate (p50 ≤ 1 ms). Batch and end-to-end numbers are unchanged within noise (`benches/reasoning/local-bulk.sh`).

**Status (2026-09-29, third batch):**
- **R3 equality module:** in the batch executor, `eq-rep-s/p/o` are replaced by one expansion per fact over its terms' `sameAs` classes, O(k) instead of O(k²) per fact.
- **R5 B/F deletion:**
  - A deletion candidate with a backward proof down to still-asserted facts is kept and doesn't propagate.
  - Ground instances, bodiless facts and list rules carry the schema and list facts they were grounded on (premises). A proof must prove those too, so no dependency stays hidden.
- **Transitive deletes by component:** a lost edge affects only pairs from its source's predecessors to its target's successors. Pairs still connected by base edges keep a proof. The rest is re-closed by SCC condensation.
- **Startup:** a marker file (`reasoning.state`) records the ruleset the inferred stack is exact for. With a matching marker, the server skips rematerialisation. With reasoning off, a leftover inferred stack is cleared.

Evidence:
- 84,000+ random insert/delete changes are exact against rematerialisation.
- OWL2Bench RL(1), deleting one edge inside a clique of about 1,000 people (1.4 M inferred facts): 294 s became 0.65 s, or 1.25 s when the node really leaves the clique.

End-of-batch numbers (`local-bulk.sh`):

| | Result |
|---|---|
| LUBM(100) batch closure | 2.58 s |
| LUBM(100) commits, store pipeline | delete p50 0.42 ms, p99 0.58 ms; insert p50 0.15 ms, p99 0.28 ms |
| LUBM(100) 14 queries | 0.87 s |
| OWL2Bench RL(1) closure | 0.53 s |

Next:
- The delta executor's own equality module; it uses the generic rules today.
- The Docker scorecard with `nrese-v2` on the reference machine (office PC).
- R6: consistency explanations and reject reports for v2.
- The v1 fixture suite on v2, then removing `rules-mvp`.

**Execution core (XC), decision D11.** A new crate, `nrese-exec`, holds the ID-level execution machinery shared by SPARQL and reasoning:
- **ID tables:** columnar `u64`, sorted on a key prefix
- **Sorting:** radix sort
- **Joins:** merge join, galloping join, Leapfrog Triejoin for cyclic patterns, hash join as the fallback
- **Aggregation:** COUNT/GROUP BY over ids
- **Parallelism:** rayon morsels with deterministic output
- **Memory:** a budget per query or per reasoning job

Scans reach storage through a seekable, block-wise cursor. That keeps Pf1 and Pf2 (compression, mmap) behind the interface: they change storage without touching an executor.

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
| **E6 Inferred stack** (reasoning R0, pulled forward) | A second index stack in `Version` with three permutations (SPO/POS/OSP) for inferences in the default graph; asserted and inferred deltas are committed atomically under one revision; the WAL and checkpoint formats carry both stacks; SPARQL Update and the other request paths can't reach it; only the pipeline's reasoning stage writes it. Done now, while the v2 format has no deployments to migrate. | L1 | Model tests over both stacks; crash tests recover both; disjointness `inferred ∩ asserted = ∅` checked in debug builds |
| **Q1 SPARQL adapter** | `spareval::QueryableDataset` over snapshots. Cancellation tokens wired to request deadlines. Protocol dataset parameters (`default-graph-uri`, `named-graph-uri`, `using-*`). Streaming result serialisation. Update planning into deltas (all `GraphUpdateOperation`s, `LOAD` behind policy). | L2 | W3C SPARQL 1.1 query + update suites at spareval's pass rate; audit F2 closed (nothing commits after a timeout) |
| **P1 Mutation pipeline** | plan → validate → deadline check → commit, all proportional to the delta. The v1 reasoner is fed through an adapter until M3. | L3 | One-triple insert at 10 M triples < 5 ms with reasoning off (audit: 2.9 s at 1 M) |
| **P2 Reactor** | Delete the Oxigraph `Store` usage, the staging clone and the string snapshots. | all | `grep oxigraph::store` is empty |

**M1 status (2026-09-26): complete.** Details as recorded along the way: `nrese-engine` is in the workspace and CI.
- **E2–E4 done** and gated:
  - `index/model_tests.rs`: random operation sequences against a `BTreeSet`, all 32 pattern shapes, named-graph listing, policy and arbitrary compaction windows, old versions after compaction.
  - `tests/engine_tests.rs`: snapshot isolation, overlay, abort, readers during an open transaction, three concurrent readers never seeing a partial commit, and an RDF-term model test.
  - `tests/durability_tests.rs`: torn tail cut at every byte of the last record, mid-log corruption reported rather than skipped, crash during a checkpoint and between the checkpoint and WAL release, dictionary continuity across aborted transactions, directory lock, and background checkpoints bounding the WAL.
- **E1 done:**
  - Canonical `xsd:integer`, `xsd:boolean`, `xsd:decimal`, `xsd:date` and `xsd:dateTime` values are inlined in the term id (`term/inline.rs`, which documents the ranges).
  - Only XSD 1.1 canonical lexical forms are inlined, so decoding reproduces the input exactly. A fuzz test over 200 k near-miss strings checks that.
  - Non-canonical forms (`"1.50"`, `"…+00:00"`) stay dictionary terms with their own identity; `=` still compares values. Pinned in `nrese-sparql`'s differential tests, whose oracle dataset now covers date ranges, dateTime ordering and decimal arithmetic.
  - The term encoding change bumped the WAL and checkpoint format to 3.
  - Blank-node scoping is per payload for requests and per load for bulk loads.
- **Q1 partial:** the `nrese-sparql` crate exists.
  - Done: the spareval adapter over snapshots and transactions (one `ReadView` contract); SPARQL Update into one transaction without committing, so the pipeline decides; protocol dataset overrides; cancellation between and inside operations.
  - Evidence: `tests/differential_tests.rs` runs 30 queries against Oxigraph as an oracle, before and after each of 14 update scripts, with no mismatches.
  - Two intentional differences, each pinned by a test: literal lexical forms are preserved, and a graph exists only while it holds quads.
  - Done since (2026-09-26):
    - Streaming results: store `PreparedQuery` plus `run_query` into any writer; the server streams through a bounded channel. 10 M rows use +1.9 MiB of server memory.
    - Read-query deadlines: 408 at the deadline, evaluation cancelled, also on client disconnect.
    - Protocol dataset parameters over HTTP.
    - Blank-node scoping for file loads is done by the bulk loader (E5).
  - W3C suite runner done (`nrese-sparql/tests/w3c_sparql11`):
    - It runs the pinned `w3c/rdf-tests` (`scripts/fetch-w3c-tests.sh`) in CI on engine v2 *and* on the Oxigraph oracle.
    - Result: 485/495 passed (the oracle: 477). Query evaluation is 222/232; update evaluation 94/94; all 166 syntax tests and 3/3 result-format tests pass.
    - All 10 failures are spareval behaviours shared with the oracle, listed with reasons in `expected-failures.txt`. A new failure or an unexpected pass fails CI.
    - We pass 8 tests the oracle fails, because we preserve literal forms.
  - **Q1 done. M1 is complete.**
  - **Moved to X2:** `LOAD` needs the same outbound HTTP client, allowlist and SSRF guards as federated `SERVICE`, so it is built once there.
- **P1 done (gate passed):**
  - `nrese-store` runs every write as plan (into an engine transaction) → gates → ticket claim → commit.
  - The transaction's exact delta is the preview; the dataset clone and full diff are gone.
  - The mutation ticket's cancel also stops SPARQL evaluation.
  - End to end over HTTP at 10 M triples, a one-triple insert takes 216 µs p50 in memory and 2.05 ms p50 / 3.9 ms p99 durable. A hard-kill restart recovered exactly.
- **P2 done in substance:**
  - The Oxigraph `Store`, RocksDB/libclang and the `durable-storage` feature are gone, and on-disk mode is always available.
  - Oxigraph remains only as the differential-test oracle (a dev-dependency of `nrese-sparql`).
  - The string snapshots remain only for the v1 reasoner when enabled, and M3 retires them.
- **E5 done; competitor comparison pending:**
  - `Engine::bulk_load` holds the writer slot and interns batches from many threads. The dictionary prepares keys outside its lock.
  - It sorts once, builds the base run directly and publishes one revision.
  - Durable mode writes a checkpoint *before* publishing, so there is no WAL record and no 4 GiB cap.
  - `nrese-server load` parses N-Triples and N-Quads on all cores; other formats parse on one thread with parallel interning. Blank nodes are fresh per load and consistent across chunks.
  - Measured: 100 M triples, durable, in 37.3 s (2.68 M t/s) with a 23 GB peak; restart takes 20.3 s.
  - The gate's comparison against QLever's and GraphDB's loaders on the same machine is still open.
  - HTTP restore still runs through the gated pipeline.
- **E6 done (gate passed):**
  - Every version holds an asserted and an inferred stack; the inferred one keeps three permutations, halving its index memory.
  - Both stacks commit atomically under one revision, and WAL and checkpoint format 2 carries both. Older formats are rejected with `UnsupportedFormat`.
  - `ReadModel` (`Materialised`/`Asserted`/`Inferred`) is in the engine and in `nrese-sparql`'s `ReadView`.
  - The engine enforces disjointness on the transaction's final state.
  - Backups export asserted statements only.
  - Evidence: `tests/inferred_stack_tests.rs` runs both stacks against a two-set model, in memory with compaction and durable across checkpoints and reopening. Removing either disjointness step makes it fail. The index model tests run for both layouts.
  - The one-quad commit at 10 M is unchanged (2.9 µs p50 in memory).
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

The full design, targets and evidence plan are in [design/reasoner-v2.md](design/reasoner-v2.md). **Reasoning is the headline** (D9). The evidence comes from the reasoning benchmark ([design/reasoning-benchmark.md](design/reasoning-benchmark.md)):
- tasks RT1–RT7: materialisation, maintenance, entailment queries, consistency, commit-path reasoning, classification, explanations
- datasets: LUBM, OWL2Bench, DBpedia, YAGO, ORE 2015, large biomedical TBoxes
- comparisons: GraphDB, RDFox, AnzoGraph, Stardog, Jena, Nemo and the DL reasoners

Every R work package's "done when" cites it.

Reasoning behaviour is **configured per repository and per request** (decision D7, design §2.2); the targets below apply to the tuned defaults.

**Targets:**
- inferred sets identical to GraphDB on LUBM/UOBM/SPB
- W3C OWL 2 RL conformance
- ≥ 10× GraphDB load plus materialisation on the same machine
- commit-path reasoning p50 ≤ 1 ms at 100 M
- incremental equals rematerialisation in 100 % of property tests

| WP | Scope | Layer | Done when |
|---|---|---|---|
| **R0 Engine: inferred stack** | Pulled into M1 as **E6** (see there). | L1 | — |
| **R1 Rule IR & rulesets** | Reasoning-profile schema (design §2.2), validated at startup; IR (SCCs, strata, schema atoms); built-in `rdfs`, `rdfs-plus`, `owl-horst`, `owl2-rl`, `owl2-ql` as data; `.pie` import/export; the naive reference evaluator (the oracle). | L2 | `.pie` round-trip on GraphDB's published rulesets; reference evaluator passes RDFS/RL fixtures |
| **R2 Schema compiler + batch executor** | TBox closure and rule specialisation (dispatch tables); list axioms compiled to fixed-arity rules; vertically partitioned sorted working set; semi-naive evaluation; sort-merge and Leapfrog Triejoin; morsel parallelism with deterministic output; bulk install into the inferred stack. | L2 | Batch equals naive (proptest); LUBM-1/10/100 inferred sets equal GraphDB's; throughput and thread scaling recorded |
| **R3 Modules** | Hierarchy (SCC + bitset reachability), transitive properties, equality (union-find rewriting with read-time expansion), symmetric/inverse. | L2 | Each module equals its generic rules; UOBM parity |
| **R4 Read models + commit-path reasoning** | `Materialised` / `Asserted` / `Inferred` read models; `onto:explicit` / `onto:implicit`; per-request switch; delta executor in the mutation pipeline (inserts, `timing = commit`); single-graph placement for any graph; `ruleset = none` at zero cost; bulk load `--reason`. | L2/L3 | Query tests for all three models (audit F4 closed); insert reasoning p50 ≤ 1 ms at 100 M |
| **R5 Truth maintenance + timings** | DRed, then B/F / FBF; TBox deltas; sameAs maintenance; asserted ↔ inferred moves; reasoning jobs for large TBox changes; `deferred` and `on-demand` timings; counting evaluated. | L2 | Incremental equals rematerialisation in CI; delete latency and TBox-change cost recorded |
| **R6 Consistency & explanations** | Consistency rules (`false` heads, `.pie` `Consistency`), proof search, proof-carrying rejects, proof API. The v1 fixture suite passes on v2; then `rules-mvp` is deleted. | L2/L3 | W3C RL consistency tests; explanation ≤ 10 ms; v1 removed |
| **R7 RDF 1.2 triple terms** | `TermKind::Triple`, syntax, SPARQL-star (decision D4). | L1/L2 | RDF 1.2 tests |
| **R8 Beyond GraphDB** (stretch) | EL classification module (ELK-style), OWL 2 QL rewriting mode. Each item is a separate decision. | L2 | per item |
| **R9 Graph-scoped reasoning** | `placement = source-graph`: per-graph rule evaluation, shared schema graphs, quad layout for the inferred stack. | L1/L2 | per-graph results equal evaluating each graph separately |

### M4: Performance, the QLever track (size XL)

**Goal (2026-09-26): at least state of the art on every basics metric, and faster where we can be** (decision D10), **with every trade-off configurable per use case** (decision D7 applies to storage as much as to reasoning). The means are machine-code-level optimisation, algorithm engineering, data structures and caches, all measured.
- **Scorecard first.** Every WP below is judged on the Pf0 scorecard, never on a single metric. Load time is one column among nine.
- **Order within M4, following the first scorecard** ([benches/competitors/SCORECARD.md](../benches/competitors/SCORECARD.md), 2026-09-26):
  1. Pf0: done for load, size, memory, restart and queries; concurrency and write-under-load still to come.
  2. Pf7: the latest toolchain, codegen and cheap wins. It comes first because it lifts every later measurement.
  3. **Pf3 and Pf4: queries, the largest gap.** QLever is 10–2000× faster on joins and aggregates.
  4. Pf2: restart is 22 s at 67 M against QLever's 1.1 s; serving memory is 16 GiB against 0.2 GiB.
  5. Pf1 and Pf5: 42–57 bytes per triple against QLever's 17–42.
  6. E7: load is already the fastest of the group, 1.7–12× ahead at 67 M.
  7. Pf6: profiles, refined as the knobs appear.
- **D1 is resolved** (2026-09-27): §3.1 sets the phase order.

| WP | Scope | Layer | Done when |
|---|---|---|---|
| **Pf0-R Reasoning benchmark** (now; D9) | [design/reasoning-benchmark.md](design/reasoning-benchmark.md): RT1–RT7 over LUBM, OWL2Bench, DBpedia and YAGO with their ontologies, ORE 2015, and large biomedical TBoxes. Generators run in Docker. Oracles: owlrl, HermiT/ELK via ROBOT, and later RDFox/GraphDB. Publishable baselines: Jena, Nemo, Virtuoso. AnzoGraph, GraphDB, RDFox and Stardog stay local. NRESE v1 on RT4/RT5 now; v2 as M3 lands. | tooling | Every task runs end to end for the available systems, with correctness checked against an oracle |
| **Pf0 Benchmark scorecard** (basics; regression guard) | `nrese-bench-harness scorecard` plus the Docker comparison kit (`benches/competitors/`), covering NRESE, QLever, Jena/Fuseki, Oxigraph, Virtuoso, GraphDB, RDFox and AnzoGraph. **Datasets:** synthetic entities (10 M/100 M), Olympics, DBpedia 2022-12 core, Wikidata lexemes, YAGO 4.5 tiny; LUBM and BSBM generators (Rust ports); full Wikidata truthy only on a big server. **Metrics per system and dataset:** load time; store bytes/triple; peak RSS; restart time; latency of a per-dataset query mix (p50/p99); throughput at 1/8/32 clients; 1-triple write latency under read load; for M3, materialisation time and incremental maintenance. Results go to the git-ignored `results/`; only free-to-publish systems appear in committed docs (licence rules in `benches/competitors/README.md`). | tooling | Scorecard runs end to end for all free systems on all datasets that fit, reproducibly, on the reference machine |
| **Pf7 Toolchain, codegen and cheap wins** | **Toolchain:** the latest stable Rust (1.91 → 1.98.1 as of 2026-09-26), pinned in `rust-toolchain.toml` and bumped at every stable release after a scorecard check. **Codegen:** release profile (fat LTO, `codegen-units = 1`, `panic = "abort"` where safe); mimalloc; `x86-64-v3`/`v4` builds and `target-cpu=native` for self-built deployments, with runtime CPU dispatch for SIMD kernels in the portable build; PGO on the scorecard and reasoning mixes, then BOLT. **Machine code:** hot loops (merges, scans, dictionary lookups, rule joins) are checked in assembly with `cargo-show-asm` for vectorisation, no bounds checks and no hidden allocations, and profiled with `perf`/`samply` per benchmark query; allocations are profiled with dhat. **Data structures:** a loser tree for k-way merges wider than 4; software prefetch in merges; no term decoding for FILTERs over inline values; parallel scans for large ranges. **Write-path isolation:** commit capacity reserved against read load, since the scorecard shows write p99 of 0.2–2 s under 8 readers. Each change is kept only if the scorecard improves and nothing regresses. | all | Scorecard delta recorded per change |
| **E7 Loader v2** | Zero-copy N-Triples/N-Quads tokenizer (memchr/SIMD, no per-term allocation). Batch-local vocabularies merged by parallel sort, so there is no global dictionary lock; ids are assigned in sorted order, which enables Pf5's front coding. Radix sort for permutations. The checkpoint is the run files (after Pf2) and is written pipelined with index building. | L1 | 10 M entities ≤ 1.5 s end to end (now 3.9 s in Docker); 100 M ≤ 15 s (now 44 s) |
| **Pf1 Compressed runs** | Block layout (e.g. 64 KiB blocks, first-key directory, per-column delta + SIMD bit-packing / Stream VByte). Static search over the block directory (Eytzinger/S-tree or PGM-index). | L1 | Bytes/triple reported and compared with QLever; scan throughput not worse than uncompressed |
| **Pf2 On-disk runs** | Memory-mapped immutable run files; the base run is no longer RAM-resident; checkpoint = write run files; **restart maps files instead of rebuilding** (now 20 s at 100 M). | L1 | Datasets larger than RAM load and query; restart < 1 s at 100 M |
| **Pf3 Native BGP executor** | Per-query memory accounting and limits: server memory grows from 16 to 24 GiB at 67 M under 8 clients. Vectorised ID tables; merge joins on shared permutation order; worst-case-optimal joins (Leapfrog Triejoin / Free Join style) for cyclic patterns; adaptive choice between binary and WCOJ joins (Umbra-style). spareval handles the remaining operators; differential tests against spareval. | L2 | Query mix within the agreed factor of QLever; no result differences against the oracle |
| **Pf4 Statistics & optimiser** | Per-predicate counts, characteristic sets (Neumann & Moerkotte) for cardinality estimation, DP join ordering; `EXPLAIN` / profile output with operator timings and scan counts. | L1/L2 | Estimation error tracked; plans explained in the API |
| **Pf5 Dictionary at scale** | Sorted, front-coded or FSST-compressed main vocabulary (on disk); hash index only for the delta vocabulary; checkpoint rebuild reclaims unreferenced ids. | L1 | Dictionary bytes/term reported; lookups stay O(log n) or better |
| **Pf6 Storage profiles** | Named, validated presets over the storage knobs, each with its scorecard:<br>- **index set:** 6 permutations; 3 when named graphs aren't used; graph-first permutations optional<br>- **compression level**<br>- **sync policy:** every commit, **group commit** (batched fsync for concurrent writers), or OS-buffered<br>- **compaction:** fanout and budget<br>- **memory budget and cache sizes**<br>Presets: `interactive` (default: lowest write latency), `write-heavy` (group commit), `read-optimised` (maximum compression, aggressive compaction), `memory-constrained`, `bulk-analytics`. They compose with the reasoning profile (D7). | L1/L4 | Each preset documented with its measured trade-offs; invalid combinations rejected at startup |

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
| Memory before compression (224 B/quad with seven permutations) | Fine up to ~50 M quads on a 16 GB machine. Pf1/Pf2 are needed before RG-scale-plus-provenance growth exceeds that; the trigger is monitored. |
| Oxigraph crate API churn | Versions pinned; upgrades are explicit work items. |

## 8. Decisions

| # | Question | Decision |
|---|---|---|
| D1 | After M1, what comes first: governance (SHACL, FTS, RDF4J, ResearchSpace) or reasoning? | **Revised 2026-09-27: performance (M4) and reasoning (M3) first, in the phases of §3.1.** The user: make NRESE as strong as possible as its own package, with efficient basics, maximum performance and industry-leading reasoning, query and operations performance. M2 follows. Originally: **governance first** (decided 2026-09-25). DMW (D18) and ResearchSpace need it; reasoning builds on the same engine afterwards. The reasoning plan pulls only its storage part (E6) into M1, and M3 can start right after M1 if priorities change ([design/reasoner-v2.md](design/reasoner-v2.md) §9). |
| D2 | How to mark inferred data: separate asserted/inferred index stacks, or per-entry flags? | **Separate stacks** (decided 2026-09-25; see 1.1 above). |
| D3 | Full-text engine: tantivy, or our own inverted index? | **tantivy** (decided 2026-09-25). |
| D4 | RDF-star / RDF 1.2 triple terms: early (M2) or with reasoning (M3)? | **M3** (decided 2026-09-25), unless a concrete DMW/RS need appears earlier. |
| D5 | Is the v1 `rules-mvp` reasoner kept running through an adapter during M1–M2? | Default **yes**, then deleted in R5. Reversible; not yet explicitly confirmed. |
| D7 | Are reasoning behaviours fixed or configurable? | **Configurable** (decided 2026-09-26). Tuned defaults, other modes exposed per repository (ruleset, timing, consistency, placement, sameAs, maintenance) and per request (read model, explanations, freshness). Only the stack invariants are fixed. See [design/reasoner-v2.md](design/reasoner-v2.md) §2.2. |
| D9 | What do the benchmarks lead with? | **Ontology reasoning** (user, 2026-09-26): "the real measurements for benchmarking should be all about reasoning capabilities for ontologies, as that's the main target for our triplestore to shine in, the other things are the basics". The reasoning benchmark (Pf0-R) is the headline evidence, and the load/query scorecard guards the basics. Open follow-up for D1: does M3 (reasoning) now move ahead of M2 (governance)? |
| D10 | How good must the basics be? | **At least state of the art on every metric, ideally faster** (user, 2026-09-26). Reasoning is the headline (D9), but load, storage, query and write speed must match or beat the best system on each scorecard column: QLever for queries, the fastest loader for load, and so on. The means are machine-code-level optimisation (the latest Rust, codegen tuning, assembly inspection; Pf7), algorithm engineering and data structures (Pf1–Pf5, E7), and caches. "Good enough" isn't a target. |
| D11 | Do SPARQL and the reasoner get separate executors? | **No: one shared execution core, `nrese-exec`** (2026-09-27; §3.1). The native SPARQL executor (Pf3) and the reasoner's batch and delta executors (R2, R4) use the same ID tables, sorts, joins (merge, galloping, Leapfrog Triejoin, hash) and memory budgets. Every optimisation then speeds up both, and there's one join implementation to test. |
| D8 | How is performance judged, and which trade-offs are fixed? | **A multi-metric scorecard (Pf0); tuned defaults with configurable presets (Pf6)** (decided 2026-09-26). The benchmark suite is finished before further implementation. Competitor results follow the vendors' licences: GraphDB, RDFox and AnzoGraph results are never published without written permission. |
| D6 | May the stray build directories in the working copy be deleted (F5)? | **Yes** (decided 2026-09-25): 23 untracked `target-*` directories (about 50 GB) were removed. |
