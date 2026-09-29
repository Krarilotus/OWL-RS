# Target Capability Matrix (QLever + GraphDB)

This is the single status source for parity. It replaces the Fuseki gap matrix, now in `docs/archive/`.

**Status values** (as of 30 September 2026):
- `done`: implemented, serving, and its evidence gate passed
- `delivered`: implemented and serving, with the listed exceptions; the full evidence gate is still open
- `missing`: not implemented (the WP and the [parity plan](../plan/2026-09-30-graphdb-parity-plan.md) schedule it)

What reasoning computes is stated precisely in [reasoning-semantics.md](reasoning-semantics.md).

The **Evidence** column names the test or benchmark that moves a row to `done`.

| Capability | QLever | GraphDB | NRESE status | WP | Evidence |
|---|---|---|---|---|---|
| SPARQL 1.1 query | yes (some deviations) | yes | **done**: W3C query evaluation 222/232, query syntax 111/111, results formats 3/3. All 10 failures are spareval behaviours that the Oxigraph oracle fails too; 7 of them are post-2013 suite additions. The oracle passes 477 of 495 overall, we pass 485 | Q1 | `nrese-sparql/tests/w3c_sparql11` (in CI) |
| SPARQL 1.1 update | yes (delta layer degrades) | yes | **done**: W3C update evaluation 94/94, update syntax 55/55. Write gate passed (1-triple insert at 10 M over HTTP: 0.22 ms in memory, 2.05 ms durable; v1: 3 s at 1 M) | Q1, P1 | `w3c_sparql11`; `write-scaling` < 5 ms at 10 M |
| Protocol dataset parameters | yes | yes | delivered: `default-graph-uri` / `named-graph-uri` for queries (GET, form and direct POST) and `using-graph-uri` for updates; queries with a dataset run on the general evaluator | Q1 | protocol tests |
| Graph Store Protocol | yes | yes | delivered; exception: status codes for missing named graphs differ from the W3C spec (parity plan D5) | Q1 | GSP tests |
| Query cancellation / real timeouts | yes | yes | **done**: a timed-out write never commits; queries are cancelled at their deadline, including inside the native executor and while results stream. Exception: cancellation doesn't yet reach commit-path reasoning (parity plan A8) | Q1, P1 | timeout-commit test; `query_execution_tests`, `query_protocol_tests` |
| Streaming large results | yes | yes | **done**: native results are written straight from id tables in JSON, TSV and CSV (byte-identical to the reference serialiser), in 64 KiB chunks with backpressure; a revision-keyed result cache answers repeated queries | Q1 | memory-bounded 10 M-row test (`benches/baselines/README.md`) |
| Federated `SERVICE` | yes | yes | missing | X2 | W3C federation tests |
| RDF4J protocol + transactions | no | yes | missing | X1 | RDF4J client test |
| Multi-repository | no | yes | missing | X1 | API tests |
| Crash-safe durability, persistent revision | yes | yes | **done**: WAL + checkpoints serving; crash-injection tests; a hard-kill restart at 10 M recovered exactly | E4, P2 | crash-injection tests |
| Online backup | — | yes | delivered: snapshot-consistent export and restore through the validated pipeline; missing: versioned manifests and point-in-time restore (O2, parity plan E3) | O2 | drill |
| Parallel bulk loader | yes (fast) | yes | v2 serving: `nrese-server load`, 100 M triples in 37 s (2.68 M t/s, durable); competitor runs on the same machine pending | E5 | load benchmark vs QLever / GraphDB loaders |
| Compressed / on-disk indexes | yes | yes | delivered: bit-packed runs (Pf1, peak memory on LUBM(100) 5.5 → 2.5 GB); checkpoints store the packed runs (restart 0.27 s on LUBM(100)); missing: memory-mapped runs for data larger than RAM (Pf2 step 2) | Pf1, Pf2 | bytes/triple, larger-than-RAM test |
| Worst-case-optimal / merge joins | yes | partial | **done**: native executor with merge, hash and index joins, a worst-case-optimal join with leapfrog intersection for cyclic patterns, parallel joins, filters and aggregates; differential-tested against spareval | Pf3 | query-mix benchmark vs QLever |
| Cost-based optimiser, explain/profile | yes | yes | delivered: distinct statistics, DP join ordering, `explain=true` (operators with estimated and actual rows and times); missing: characteristic sets, estimation error in the perf lab | Pf4 | explain API tests |
| RDFS / OWL-Horst / OWL 2 RL / OWL 2 QL materialisation | no | yes | delivered for `rdfs` (6-rule subset) and `owl2-rl` (exceptions in [reasoning-semantics.md](reasoning-semantics.md)): inferences are queryable and maintained on every commit; inferred sets equal the owlrl oracle or Nemo on LUBM and OWL2Bench(1). Missing: full RDFS, OWL-Horst, OWL 2 QL (parity plan B1) | E6, R1–R4 | LUBM/UOBM/SPB inferred sets equal GraphDB's; W3C OWL 2 RL tests; ≥ 10× GraphDB load + materialise |
| Incremental retraction (TMS) | no | yes | **done**: DRed with backward/forward proofs; incremental = rematerialisation on 84 k random changes and 3 k engine commits | R5 | incremental = rematerialisation property tests |
| Consistency checking with explanations | no | yes | delivered: rejecting commits with explanations (premises, rule, trigger); inconsistent baselines are quarantined. Missing: W3C RL consistency test run, datatype consistency (B3) | R6 | W3C RL consistency tests; v1 fixture suite; explanation ≤ 10 ms |
| Explicit/implicit pseudo-graphs | no | yes | **done**: `infer=false`, `FROM onto:explicit` / `onto:implicit`, per request, on both executors (differential-tested) | E6, R4 | query tests |
| SHACL Core + paths on commit | no | yes | missing | S1, S2 | W3C SHACL suite; incremental = full |
| SHACL-SPARQL | no | yes | missing | S3 | W3C SHACL-SPARQL tests |
| Full-text search | yes (integrated) | yes (connectors/Lucene) | missing | T1 | ResearchSpace search; relevance tests |
| Autocompletion | yes | partial | missing | T1 | latency benchmark |
| GeoSPARQL | partial | yes | missing | G1 | GeoSPARQL compliance subset |
| Vector similarity | no | yes (similarity/connectors) | missing | V1 | recall@k |
| RDF-star / RDF 1.2 triple terms | partial | yes | missing | R7 | RDF 1.2 tests |
| Graph-level access control | no | yes (Enterprise) | missing | O1 | security matrix |
| Metrics and tracing | partial | yes | delivered: readiness, revision, counts, modes, query-cache metrics; request ids and `tracing` spans. Missing: request outcomes and latencies, WAL/compaction/backup metrics (O3, parity plan E2) | O3 | dashboards |

**Evidence notes (2026-09-25; the v1 notes are historical):**
- v1 reasoning fixtures now run against vendored copies of the published vocabularies (`benches/nrese-bench-harness/fixtures/catalog-cache/`). Earlier SOSA evidence was invalid: the catalog URL served the SSN document, so the "SOSA" test asserted an SSN axiom. It now asserts SOSA's own `sosa:observes owl:inverseOf sosa:isObservedBy`.
- Merge gate: `.github/workflows/ci.yml`. A row moves to `done` only when its evidence runs in CI or as a recorded, reproducible benchmark under `benches/baselines/`.
