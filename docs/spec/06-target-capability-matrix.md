# Target Capability Matrix (QLever + GraphDB)

This is the single status source for parity. It replaces the Fuseki gap matrix, now in `docs/archive/`.

**Status values:**
- `v1`: exists in the Oxigraph-based v1 (with the limits noted)
- `planned`: scheduled in a roadmap work package
- `engine`: implemented in `nrese-engine` with its storage-level gate passing, but not yet serving requests (that needs Q1/P1)
- `done`: implemented on engine v2 *and* its evidence gate passed

The **Evidence** column names the test or benchmark that moves a row to `done`.

| Capability | QLever | GraphDB | NRESE status | WP | Evidence |
|---|---|---|---|---|---|
| SPARQL 1.1 query | yes (some deviations) | yes | **done**: W3C query evaluation 222/232, query syntax 111/111, results formats 3/3. All 10 failures are spareval behaviours that the Oxigraph oracle fails too; 7 of them are post-2013 suite additions. The oracle passes 477 of 495 overall, we pass 485 | Q1 | `nrese-sparql/tests/w3c_sparql11` (in CI) |
| SPARQL 1.1 update | yes (delta layer degrades) | yes | **done**: W3C update evaluation 94/94, update syntax 55/55. Write gate passed (1-triple insert at 10 M over HTTP: 0.22 ms in memory, 2.05 ms durable; v1: 3 s at 1 M) | Q1, P1 | `w3c_sparql11`; `write-scaling` < 5 ms at 10 M |
| Protocol dataset parameters | yes | yes | v2 serving for queries: `default-graph-uri` / `named-graph-uri` over GET, form POST and direct POST (`query_protocol_tests`). `using-graph-uri` for updates is engine-only | Q1 | protocol tests |
| Graph Store Protocol | yes | yes | v2 serving (server GSP test suite green) | Q1 | GSP tests |
| Query cancellation / real timeouts | yes | yes | **done**: a timed-out write never commits. A read query is answered 408 at its deadline and evaluation is cancelled, also on client disconnect. Known gap: spareval's in-memory join loops aren't interruptible between quad reads (0.7 s measured on a 10¹⁰-row cross product; Pf3 closes it) | Q1, P1 | timeout-commit test; `query_execution_tests`, `query_protocol_tests` |
| Streaming large results | yes | yes | **done**: results are serialised row by row into a bounded channel (64 KiB chunks, backpressure). 10 M rows (1 GB TSV) streamed in 8.5 s with +1.9 MiB server memory | Q1 | memory-bounded 10 M-row test (`benches/baselines/README.md`) |
| Federated `SERVICE` | yes | yes | missing | X2 | W3C federation tests |
| RDF4J protocol + transactions | no | yes | missing | X1 | RDF4J client test |
| Multi-repository | no | yes | missing | X1 | API tests |
| Crash-safe durability, persistent revision | yes | yes | **done**: WAL + checkpoints serving; crash-injection tests; a hard-kill restart at 10 M recovered exactly | E4, P2 | crash-injection tests |
| Online backup | — | yes | v1 export only | O2 | drill |
| Parallel bulk loader | yes (fast) | yes | v2 serving: `nrese-server load`, 100 M triples in 37 s (2.68 M t/s, durable); competitor runs on the same machine pending | E5 | load benchmark vs QLever / GraphDB loaders |
| Compressed / on-disk indexes | yes | yes | missing | Pf1, Pf2 | bytes/triple, larger-than-RAM test |
| Worst-case-optimal / merge joins | yes | partial | missing | Pf3 | query-mix benchmark vs QLever |
| Cost-based optimiser, explain/profile | yes | yes | missing | Pf4 | explain API tests |
| RDFS / OWL-Horst / OWL 2 RL / OWL 2 QL materialisation | no | yes | v1: bounded RDFS+OWL write gate only; inferences not queryable. v2 planned ([design/reasoner-v2.md](../design/reasoner-v2.md)) | E6, R1–R4 | LUBM/UOBM/SPB inferred sets equal GraphDB's; W3C OWL 2 RL tests; ≥ 10× GraphDB load + materialise |
| Incremental retraction (TMS) | no | yes | missing | R5 | incremental = rematerialisation property tests |
| Consistency checking with explanations | no | yes | v1 partial (IRI-only triples) | R6 | W3C RL consistency tests; v1 fixture suite; explanation ≤ 10 ms |
| Explicit/implicit pseudo-graphs | no | yes | engine: `ReadModel` (materialised/asserted/inferred) over separate stacks; not yet selectable per request | E6, R4 | query tests |
| SHACL Core + paths on commit | no | yes | missing | S1, S2 | W3C SHACL suite; incremental = full |
| SHACL-SPARQL | no | yes | missing | S3 | W3C SHACL-SPARQL tests |
| Full-text search | yes (integrated) | yes (connectors/Lucene) | missing | T1 | ResearchSpace search; relevance tests |
| Autocompletion | yes | partial | missing | T1 | latency benchmark |
| GeoSPARQL | partial | yes | missing | G1 | GeoSPARQL compliance subset |
| Vector similarity | no | yes (similarity/connectors) | missing | V1 | recall@k |
| RDF-star / RDF 1.2 triple terms | partial | yes | missing | R7 | RDF 1.2 tests |
| Graph-level access control | no | yes (Enterprise) | missing | O1 | security matrix |
| Metrics and tracing | partial | yes | v1 | O3 | dashboards |

**Evidence notes (2026-09-25):**
- v1 reasoning fixtures now run against vendored copies of the published vocabularies (`benches/nrese-bench-harness/fixtures/catalog-cache/`). Earlier SOSA evidence was invalid: the catalog URL served the SSN document, so the "SOSA" test asserted an SSN axiom. It now asserts SOSA's own `sosa:observes owl:inverseOf sosa:isObservedBy`.
- Merge gate: `.github/workflows/ci.yml`. A row moves to `done` only when its evidence runs in CI or as a recorded, reproducible benchmark under `benches/baselines/`.
