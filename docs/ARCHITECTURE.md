# NRESE Architecture (Engine v2)

This is the canonical architecture document. It says which layer owns which concern, which way dependencies may point, and — most importantly for day-to-day work — **at which layer a given kind of problem must be fixed**. Specs under `docs/spec/` refine individual concerns; if they contradict this file, this file wins and the spec is updated.

Decisions with lasting consequences are recorded as ADRs in [`docs/adr/`](adr/).

## 1. Product target

NRESE is an RDF database and ontology platform, hardware-aware on one machine (every core, the memory, CPU features and GPUs it has) and scaling out to clusters where a use case needs it, each derived setting configurable. It combines:

- **QLever-class query performance and scale** — dictionary-encoded, sorted permutation indexes, merge/worst-case-optimal joins, integrated text search, fast parallel bulk loading.
- **GraphDB-class semantics and data governance** — materialised, incrementally maintained RDFS / OWL 2 RL reasoning with asserted vs inferred separation, built-in SHACL validation on commit, full-text connectors, transactional writes.
- **Reasoning beyond the materialisable profiles** — OWL 2 DL (classification, consistency, explanation) is a goal: reasoning is where NRESE means to lead, drawing on the research on tableaux, hypertableaux, consequence-based calculi and their combination with materialisation.
- **One engine API with a thin connector surface** — every capability is an operation of the core; the protocols (SPARQL, Graph Store, RDF4J, GraphDB compatibility) and the one frontend only translate to it.

Fuseki is no longer the parity target. QLever and GraphDB are, measured with the harness in `benches/nrese-bench-harness` (see [ADR-0004](adr/0004-parity-targets-qlever-graphdb.md)).

## 2. Layer model

Dependencies point **downwards only**. A crate may use the public API of any crate in a lower layer, never a sibling's internals and never an upper layer.

```
L5  apps/nrese-console            browser UI + CLI (TypeScript), talks HTTP only
L4  nrese-server                  HTTP transport, auth, policy, posture, UI hosting, AI assistant
L3  nrese-store                   application service: operations, mutation pipeline, validation gates, backup
L2  nrese-sparql  nrese-reasoner  nrese-shacl
                                  query/update evaluation · materialisation & consistency · shape validation
L1  nrese-engine  nrese-exec  nrese-vector  nrese-owl  nrese-dl
                                  term dictionary, quad indexes, MVCC snapshots, transactions, durability
                                  · id tables, joins, grouping, closures, memory budgets · vector indexes
                                  · the OWL 2 structural model and normaliser · the DL engines
L0  crates/rdf/: nrese-rdf  nrese-rdf-io  nrese-sparql-syntax  nrese-sparql-results  nrese-xsd  nrese-json
                                  the RDF model, syntaxes, SPARQL syntax and results, XSD and OWL datatypes
```

Outside the layer model: `nrese-sparql-reference` (the SPARQL evaluator written from the specification, the oracle of the differential tests; used by tests only), `nrese-fuzz` (fuzz targets for every parser of untrusted input), and `benches/` (the benchmark kits).

### 2.1 Crate ownership

| Crate | Owns | Must not own |
|---|---|---|
| `crates/rdf/` | The RDF model (`nrese-rdf`: IRIs, terms, quads, canonicalisation), every RDF syntax (`nrese-rdf-io`), SPARQL syntax and algebra (`nrese-sparql-syntax`), the results formats (`nrese-sparql-results`), XSD values and the OWL 2 datatype map (`nrese-xsd`), JSON (`nrese-json`). | Storage, evaluation, reasoning. |
| `nrese-engine` | Term model and dictionary encoding (`TermId`), quad indexes in all needed permutations, LSM runs and compaction, MVCC snapshots, the commit protocol, the write-ahead log and checkpoints, bulk load, cardinality statistics. | SPARQL semantics, RDF syntax parsing, validation or reasoning rules, HTTP. |
| `nrese-exec` | Id-level tables, joins, grouping, graph closures and capacity accounting; physical worker handles and bounded batch dispatch shared by participating query and DL paths. It never decodes a term and depends on no NRESE crate. | Term values, storage, SPARQL or rule semantics; server-wide admission policy. |
| `nrese-vector` | Vector literals' distances and indexes (exact scan, HNSW). | Terms, storage, SPARQL. |
| `nrese-owl` | The OWL 2 structural model read from RDF or the functional syntax, its diagnostics, and the normaliser into DL-clauses (automata for role inclusions, lazy definitions). | Search, storage, rule evaluation. |
| `nrese-dl` | The OWL 2 DL engines over `nrese-owl`'s clauses: context saturation, tableau search and immutable compiled probes with fresh search state, portfolio, classification and realisation. | Storage, SPARQL, HTTP; when to run (the store and reasoner decide), query-to-assumption adaptation. |
| `nrese-sparql` | SPARQL 1.1 query and update evaluation over an engine snapshot: the native executor (the only one) and its planner (EXPLAIN), the result cache of plan parts (its keys are plan parts, its entries id tables, and only the executor knows what a part's result depends on), query cancellation, dataset specification (`FROM`, protocol `default-graph-uri`), update-to-delta planning, result serialisation. | Commit decisions, validation, persistence, HTTP; the cache's instance and budget (the store's, as with the query memory budget). |
| `nrese-reasoner` | Reasoning profiles, rule sets, materialisation, incremental maintenance, consistency checks, explanations. | Storage layout, HTTP, SHACL. |
| `nrese-shacl` | Shapes-graph compilation, SHACL Core (later SHACL-SPARQL) validation, incremental validation scoped to a delta, validation reports. It reads through `nrese-sparql`'s `ReadView` and uses its value semantics, because SHACL defines its constraints through SPARQL's operators; `nrese-sparql` never depends on it ([design/shacl.md](design/shacl.md)). | Storage layout, reasoning, HTTP, when to validate and what a failure means for a commit. |
| `nrese-store` | Product operations and the **mutation pipeline** (plan → validate → commit), gates, revision reporting and preload; one result cache per store; catalog `Runtime` ownership of physical workers and the shared native query budget; DL bound orchestration, candidate adaptation and completeness. | HTTP status codes, auth, request parsing; what a cached result is keyed by; generic execution kernels. |
| `nrese-server` | Routes, content negotiation, auth backends, policy (limits, timeouts, rate limits), deployment posture, operator/console hosting, AI suggestions, mapping store errors to HTTP. | Any data semantics. It never touches the engine directly. |

The runtime covers native query computation, output encoding, update WHERE and participating
DL work. Writers and callbacks stay with the caller; rule materialisation, bulk spill and
background storage keep their existing owners. This is neither a universal worker cap nor
a whole-request memory ceiling. SPARQL owns retained ID results, their dictionary/computed
value domain and reservations; the store decides which bound candidates require exact work.
See [execution-core.md](design/execution-core.md) for resource lifetimes and
[owl2-dl.md](design/owl2-dl.md#bound-results-and-exact-test-reuse-9-october) for batch reuse.

### 2.2 Data flow

**Read:** `server` parses and authorises → `store::query` takes a snapshot (lock-free `Arc` clone) → `sparql` evaluates against the snapshot with a cancellation token → results are serialised → `server` sends the response.

**Write (mutation pipeline, owned by `nrese-store`):**
1. Acquire the single writer slot and a snapshot of the latest revision.
2. **Plan:** turn the request into a `Delta` (exact inserts and deletes relative to the snapshot). For SPARQL Update this evaluates `WHERE` clauses against *snapshot + pending delta*, so later operations in a request see earlier ones. Nothing is written.
3. **Validate:** gates run against `(snapshot, delta)`: the reasoner first derives the inferred delta and checks consistency, then SHACL validates. Gates are *delta-aware*; a gate that can't be incremental says so explicitly in its capability report.
4. **Deadline check:** if the request's deadline has passed or it was cancelled, abort. **Nothing is committed after a timeout.**
5. **Commit:** the engine writes the asserted and inferred deltas to the WAL (durable mode), appends it as a new immutable run, and publishes the new version atomically. Revisions are persistent and monotonic.
6. **Post-commit:** background compaction and checkpoints. Reasoning is *not* post-commit: inferences are committed with the write that causes them, under the same revision ([design/reasoner-v2.md](design/reasoner-v2.md) §2.1).

Cost of steps 2–5 is proportional to the **delta**, not the dataset. Any change that reintroduces work proportional to the dataset on the write path must be justified in an ADR.

## 3. Where to fix what

Fix a problem at the **lowest layer that owns the concept**, and nowhere else. Workarounds in a higher layer are defects.

| Symptom | Owning layer | Not here |
|---|---|---|
| Wrong result of a triple pattern, a missing quad after commit, a lost write after restart | `nrese-engine` | Filtering in SPARQL or server code |
| Wrong SPARQL semantics (joins, `OPTIONAL`, `FILTER`, update operation behaviour, dataset clauses) | `nrese-sparql` | Rewriting queries in the server |
| A query is slow because of the access path or join order | `nrese-sparql` (planner/executor) or `nrese-engine` (index, statistics) | Caching responses in the server |
| A write is slow | `nrese-engine` (commit, runs) or the offending gate | Raising timeouts |
| Wrong or missing inference, a false consistency reject | `nrese-reasoner` | Post-filtering in the store |
| Wrong SHACL result | `nrese-shacl` | Special-casing in the pipeline |
| Gate order, "what counts as a commit", timeout-vs-commit semantics, revision numbering | `nrese-store` (mutation pipeline) | HTTP handlers |
| HTTP status, media types, auth, limits, rate limiting | `nrese-server` | Store or engine |
| Operator-visible configuration knob | Typed config struct in the owning crate; env/file parsing only in `nrese-server/src/config/` | Ad hoc `std::env::var` anywhere else |

Each crate's `lib.rs` starts with a short module doc restating its ownership. When a PR touches two layers for one bug, the description must say which layer the root cause was in.

## 4. Engine design (L1)

See [ADR-0002](adr/0002-engine-storage-lsm-permutations.md) for the full rationale.

- **Terms** are encoded as 64-bit `TermId`s. The top bits carry the term kind: IRI, blank node, literal, and **inline** values for canonical `xsd:integer` and `xsd:boolean`. A value is only inlined if its lexical form is canonical, so `"01"^^xsd:integer` and `"1"^^xsd:integer` stay distinct terms. This is RDF-correct, unlike QLever's value folding. Everything else goes through a dictionary (SwissTable hash index over one string arena).
- **Quads** are `[TermId; 4]` in **seven permutations**: `SPOG POSG OSPG` answer patterns with an unbound graph, and `GSPO GPOS GOSP` answer patterns with a bound graph (including the default graph, `TermId::DEFAULT_GRAPH`). Every triple pattern is a contiguous range scan in one of them. The seventh, `GPSO`, gives executors `(?s p ?o)` sorted by subject for star joins (XC1). The inferred stack keeps `SPOG POSG OSPG PSOG`. `Snapshot::count_in` is exact: range bounds suffice for tombstone-free runs; tombstones add bitmap counts, and equality expansion or post-filtered access paths can require scanning. `Snapshot::scan_sorted_in` returns a scan in a requested order.
- **Log-structured runs:** the index is a stack of immutable sorted runs, oldest (largest) first. A commit appends one small run containing inserts and tombstones. Reads k-way merge the runs covering a range (newest wins). A size-tiered compaction policy keeps O(log N) runs, with O(log N) amortised merge work per quad. Large merges run off the write path.
- **MVCC:** a snapshot is an `Arc` of the run list plus the revision. Readers never block writers or each other. A snapshot stays valid for as long as it's held.
- **Durability:** a redo-only write-ahead log (length + CRC32 per record, fsync policy configurable) and periodic checkpoints (atomic write-rename). Recovery = latest checkpoint + WAL replay; a torn tail is truncated. There's no RocksDB, so there's no native toolchain dependency.

## 5. Evaluation, reasoning, validation (L2)

- **SPARQL:** the native executor evaluates every query and update (merge, hash and index joins on permutation order, worst-case-optimal joins for cyclic patterns, vectorised ID tables); `nrese-sparql-syntax` parses. Its correctness oracle in tests is `nrese-sparql-reference`, an evaluator written straight from the specification's algebra. Since step 5 of the [migration plan](design/rdf-bundle.md) the engine runs on the RDF bundle of our own under `crates/rdf/` (terms, XSD values, all RDF syntaxes, SPARQL syntax and results formats) and no longer depends on any Oxigraph library ([ADR-0001](adr/0001-own-engine-reuse-oxigraph-parsers.md) as amended).
- **Reasoning** ([ADR-0003](adr/0003-materialised-reasoning.md)): semi-naive datalog materialisation of RDFS / OWL 2 RL rule sets over `TermId`s, `owl:sameAs` handled by union-find rewriting, incremental maintenance for deletes. Inferred quads live in a dedicated inferred layer, so asserted and inferred data stay distinguishable and both are queryable. Design and plan: [design/reasoner-v2.md](design/reasoner-v2.md).
- **SHACL** ([ADR-0005](adr/0005-builtin-shacl.md)): shapes are compiled once per shapes-graph revision. On commit, only focus nodes reachable from the delta are revalidated. Full validation is available on demand.

## 6. Design principles

These apply to every change. Reviews check them explicitly.

### 6.1 Clear ownership

- Every concept has exactly one owning module (section 2.1 and 3). If you can't name the owner, the design isn't finished.
- A module exposes the smallest public surface its callers need. Topic-folder facades re-export only the boundary types.
- Cross-layer calls go through public APIs only. A fix in a higher layer that compensates for a lower layer's defect is itself a defect.

### 6.2 Functional, modular design

- **Functional core, imperative shell.** Planning, validation, reasoning rules, shape checks and access-plan selection are pure functions of their inputs (snapshot, delta, config) that return values. Side effects — locking, I/O, publishing a version — are confined to a thin shell: the engine's commit path, the WAL writer, the HTTP adapters.
- **Immutable data, explicit versions.** Snapshots, runs, compiled rule sets and compiled shapes are immutable once built and shared via `Arc`. Change means building a new value, never mutating a shared one.
- **Explicit data flow.** Inputs arrive as parameters and results leave as return values. No hidden globals, no ambient configuration lookups (`std::env::var` only in `nrese-server/src/config/`).
- **Parse, don't guess.** External input (config, requests) is parsed into typed values at the edge. Unknown values are errors, never silent defaults.
- **Small modules with one reason to change.** Split a file before adding a second responsibility, not after.

### 6.3 DRY and single source of truth

- One implementation per rule, mapping, parser or projection. Tests, CLI, HTTP and UI reuse it instead of re-implementing it.
- Constants (vocabulary IRIs, endpoint paths, env names, limits) are defined once and referenced.
- Test fixtures and configs are built through shared constructors or helpers (for example `StoreConfig::in_memory().with_ontology(..)`), not copy-pasted struct literals.
- When a new path makes an old one redundant, the old one is deleted in the same work package. No "for now" duplicates survive a milestone gate.

### 6.4 Further rules

- Configuration: typed in the owning crate, parsed only in `nrese-server/src/config/`.
- Errors: each crate has one error enum. Upper layers wrap and classify by variant; they never re-parse error strings.
- Performance-sensitive code states its complexity in a doc comment (for example `/// O(log n + k)`), and has a test or benchmark that would catch a regression to a worse class.
- Tests: unit tests next to the module (or in `src/tests/` once large), black-box tests in `crates/*/tests`. The engine has property-style tests against a naive `BTreeSet` model; incremental algorithms are tested against their from-scratch equivalents.
