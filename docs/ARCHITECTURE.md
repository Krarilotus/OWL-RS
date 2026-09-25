# NRESE Architecture (Engine v2)

This is the canonical architecture document. It says which layer owns which concern, which way dependencies may point, and — most importantly for day-to-day work — **at which layer a given kind of problem must be fixed**. Specs under `docs/spec/` refine individual concerns; if they contradict this file, this file wins and the spec is updated.

Decisions with lasting consequences are recorded as ADRs in [`docs/adr/`](adr/).

## 1. Product target

NRESE is a single-node RDF database that combines:

- **QLever-class query performance and scale** — dictionary-encoded, sorted permutation indexes, merge/worst-case-optimal joins, integrated text search, fast parallel bulk loading.
- **GraphDB-class semantics and data governance** — materialised, incrementally maintained RDFS / OWL 2 RL reasoning with asserted vs inferred separation, built-in SHACL validation on commit, full-text connectors, transactional writes.

Fuseki is no longer the parity target. QLever and GraphDB are, measured with the harness in `benches/nrese-bench-harness` (see [ADR-0004](adr/0004-parity-targets-qlever-graphdb.md)).

## 2. Layer model

Dependencies point **downwards only**. A crate may use the public API of any crate in a lower layer, never a sibling's internals and never an upper layer.

```
L5  apps/nrese-console            browser UI + CLI (TypeScript), talks HTTP only
L4  nrese-server                  HTTP transport, auth, policy, posture, UI hosting, AI assistant
L3  nrese-store                   application service: operations, mutation pipeline, validation gates, backup
L2  nrese-sparql  nrese-reasoner  nrese-shacl
                                  query/update evaluation · materialisation & consistency · shape validation
L1  nrese-engine                  term dictionary, quad indexes, MVCC snapshots, transactions, durability
L0  nrese-core                    shared vocabulary: capability/report contracts, error kinds
```

Tooling outside the layer model: `benches/nrese-bench-harness` (black-box HTTP comparison against reference engines).

### 2.1 Crate ownership

| Crate | Owns | Must not own |
|---|---|---|
| `nrese-core` | Report and capability contracts shared by several L2+ crates (reasoning run reports, validation report shapes). | Anything with an algorithm or I/O. |
| `nrese-engine` | Term model and dictionary encoding (`TermId`), quad indexes in all needed permutations, LSM runs and compaction, MVCC snapshots, the commit protocol, the write-ahead log and checkpoints, bulk load, cardinality statistics. | SPARQL semantics, RDF syntax parsing, validation or reasoning rules, HTTP. |
| `nrese-sparql` | SPARQL 1.1 query and update evaluation over an engine snapshot: the spareval adapter, query cancellation, dataset specification (`FROM`, protocol `default-graph-uri`), update-to-delta planning, result serialisation, and (later) the native join executor. | Commit decisions, validation, persistence, HTTP. |
| `nrese-reasoner` | Reasoning profiles, rule sets, materialisation, incremental maintenance, consistency checks, explanations. | Storage layout, HTTP, SHACL. |
| `nrese-shacl` | Shapes-graph compilation, SHACL Core (later SHACL-SPARQL) validation, incremental validation scoped to a delta, validation reports. | Storage layout, reasoning, HTTP. |
| `nrese-store` | The operations the product offers (query, update, graph store, tell, backup/restore, stats), the **mutation pipeline** (plan → validate → commit), gate ordering, revision reporting, preload. | HTTP status codes, auth, request parsing. |
| `nrese-server` | Routes, content negotiation, auth backends, policy (limits, timeouts, rate limits), deployment posture, operator/console hosting, AI suggestions, mapping store errors to HTTP. | Any data semantics. It never touches the engine directly. |

### 2.2 Data flow

**Read:** `server` parses and authorises → `store::query` takes a snapshot (lock-free `Arc` clone) → `sparql` evaluates against the snapshot with a cancellation token → results are serialised → `server` sends the response.

**Write (mutation pipeline, owned by `nrese-store`):**
1. Acquire the single writer slot and a snapshot of the latest revision.
2. **Plan:** turn the request into a `Delta` (exact inserts and deletes relative to the snapshot). For SPARQL Update this evaluates `WHERE` clauses against *snapshot + pending delta*, so later operations in a request see earlier ones. Nothing is written.
3. **Validate:** gates run against `(snapshot, delta)`: reasoner consistency, then SHACL. Gates are *delta-aware*; a gate that can't be incremental says so explicitly in its capability report.
4. **Deadline check:** if the request's deadline has passed or it was cancelled, abort. **Nothing is committed after a timeout.**
5. **Commit:** the engine writes the delta to the WAL (durable mode), appends it as a new immutable run, and publishes the new version atomically. Revisions are persistent and monotonic.
6. **Post-commit:** background compaction; reasoning materialisation maintenance.

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
- **Quads** are `[TermId; 4]` in **six permutations**: `SPOG POSG OSPG` answer patterns with an unbound graph, and `GSPO GPOS GOSP` answer patterns with a bound graph (including the default graph, `TermId::DEFAULT_GRAPH`). Every triple pattern is a contiguous range scan in exactly one permutation.
- **Log-structured runs:** the index is a stack of immutable sorted runs, oldest (largest) first. A commit appends one small run containing inserts and tombstones. Reads k-way merge the runs covering a range (newest wins). A size-tiered compaction policy keeps O(log N) runs, with O(log N) amortised merge work per quad. Large merges run off the write path.
- **MVCC:** a snapshot is an `Arc` of the run list plus the revision. Readers never block writers or each other. A snapshot stays valid for as long as it's held.
- **Durability:** a redo-only write-ahead log (length + CRC32 per record, fsync policy configurable) and periodic checkpoints (atomic write-rename). Recovery = latest checkpoint + WAL replay; a torn tail is truncated. There's no RocksDB, so there's no native toolchain dependency.

## 5. Evaluation, reasoning, validation (L2)

- **SPARQL:** full SPARQL 1.1 semantics come from `spargebra` + `spareval` running over the engine's `QueryableDataset` implementation ([ADR-0001](adr/0001-own-engine-reuse-oxigraph-parsers.md)). Native operators (merge join on permutation order, worst-case-optimal joins for cyclic patterns, vectorised ID tables) replace spareval's generic ones incrementally, with spareval as the correctness oracle in tests.
- **Reasoning** ([ADR-0003](adr/0003-materialised-reasoning.md)): semi-naive datalog materialisation of RDFS / OWL 2 RL rule sets over `TermId`s, `owl:sameAs` handled by union-find rewriting, incremental maintenance for deletes. Inferred quads live in a dedicated inferred layer, so asserted and inferred data stay distinguishable and both are queryable.
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
