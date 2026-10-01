# Oxigraph parity matrix

The owner's yardstick for the Oxigraph replacement (1 October 2026): reach parity with
everything Oxigraph offers that our use cases need, where parity means the **intended
behaviour of the standards**, not a copy of Oxigraph's bugs or departures; do it with our
own, faster implementations; and keep the result one module made of replaceable
submodules. The feature lists come from a colleague's overview of the Oxigraph stack and
of its modern and draft standards; this file answers them item by item. It complements the
[migration plan](2026-10-01-oxigraph-migration.md) (the libraries being replaced) and the
[capability matrix](../spec/06-target-capability-matrix.md) (the product's status against
QLever and GraphDB).

**Status values:** **have** (implemented and tested), **partial** (what is missing is
named), **planned** (the plan step that delivers it), **gap** (not planned yet: what to
do), **decide** (the owner decides whether the use case needs it).

## 1. Storage engine

| Oxigraph | NRESE | Status |
|---|---|---|
| RocksDB backend (LSM, WAL, SSTables) | Own LSM of immutable sorted runs, redo WAL with CRC and checkpoints, size-tiered compaction; pure Rust, no C++ toolchain ([ADR-0002](../adr/0002-engine-storage-lsm-permutations.md)) | **have** |
| In-memory backend | The same engine in memory (writes 0.22 ms at 10 M) | **have** |
| WebAssembly build (memory-backed) | None; the bundle crates (`nrese-rdf`, `-xsd`, `-json`, `-rdf-io`) are pure Rust without threads or files in their core and could build for WASM | **decide** (see §8) |
| Permutation indexes: SPO, POS, OSP, GSPO, GPOS, GOSP, SPOG | Seven permutations: SPOG, POSG, OSPG, GSPO, GPOS, GOSP, and GPSO for star joins | **have** |
| Key encoding: prefix compression, varints | 64-bit term ids, bit-packed runs (peak memory on LUBM(100) 5.5 → 2.5 GB); memory-mapped runs for data larger than RAM missing (Pf2) | **partial** |
| Term dictionary, two-way | Dictionary encoding to 64-bit `TermId`s | **have** |
| Inlined values (numbers, booleans, dates) | Canonical `xsd:integer` and `xsd:boolean` inlined, and only canonical forms, so `"01"` and `"1"` stay distinct terms (RDF-correct); decimals, doubles and dates not inlined | **partial**: inline more types, measured in the perf lab |
| 128-bit hashes for interned strings | A dictionary with exact lookup; no hash-identity risk | **have** (different design) |
| Snapshot isolation, non-blocking readers | MVCC: a snapshot is an `Arc` of the run list; readers never block | **have** |
| Single writer, atomic batches | Single writer slot; mutation pipeline plan → validate (reasoning, SHACL) → commit, delta-proportional | **have** (see §7 for write concurrency) |
| Crash consistency | WAL replay, torn tails truncated, crash-injection tests, hard-kill restart at 10 M exact | **have** |

## 2. Standards

| Oxigraph | NRESE | Status |
|---|---|---|
| RDF 1.1 Concepts and abstract syntax | `nrese-rdf` | **have** |
| RDF 1.1 Semantics, literal value spaces | `nrese-xsd` (strict XSD lexical forms, XPath operations) | **have** |
| RDF 1.2 (`rdf-12` feature): triple terms | Part of the migration: terms (3e), syntaxes (3e), SPARQL (4), engine (6) | **planned** |
| RDF Dataset Canonicalization (RDFC-1.0) | `nrese-rdf` canonicalises blank nodes for comparison (fast, our own algorithm), but not by the W3C algorithm, whose output is a standard | **gap** → RDFC-1.0 in `nrese-rdf` with the W3C `rdf-canon` test suite, in 3e |
| SPARQL 1.1 query, patterns, paths, aggregates, modifiers, functions (hashes included) | Native executor: W3C 491/495 (one known deviation, zero-length paths from a term outside the graph) | **have** |
| SPARQL 1.1 Update (all operations) | W3C update 94/94 | **have** |
| SPARQL 1.1 Federated Query | `SERVICE` to allow-listed endpoints (off by default), bind joins, `SILENT` | **have** |
| SPARQL 1.2 / SPARQL-star | Step 4 (syntax, results formats), step 6 (evaluation) | **planned** |
| SPARQL 1.1 Protocol | GET, POST form and direct, dataset parameters | **have** |
| Graph Store HTTP Protocol | GET/HEAD/PUT/POST/DELETE, W3C status codes, six formats | **have** |
| XSD date/time and duration arithmetic and ordering | `nrese-xsd`, used by the executor (see §5) | **have** |
| GeoSPARQL (WKT, simple features, relations) | Filter functions over WKT, GeoJSON and GML, DE-9IM, geodesic measures, relations as triple patterns, **with an R-tree** | **have** (beyond Oxigraph's `spargeo`) |

## 3. Query pipeline

| Oxigraph | NRESE | Status |
|---|---|---|
| Recursive descent parser to algebra (`spargebra`) | `spargebra` still parses; `nrese-sparql-syntax` replaces it (syntax tree separate from the algebra, source positions) | **planned** (step 4) |
| Heuristic optimiser (`sparopt`): filter pushdown, join reordering by bound variables, constant folding | Cost-based: distinct statistics, dynamic-programming join order within a basic graph pattern, filter pushdown below joins, OPTIONAL, UNION, MINUS, BIND and into subqueries, `explain` with estimated and actual rows | **have** within a BGP; **partial** across BGPs, paths and subqueries (Pf4) |
| Volcano pull pipeline | Vectorised id tables, streaming results in 64 KiB chunks with backpressure | **have** |
| Nested-loop, hash and merge joins | Merge, hash and index joins on permutation order, worst-case-optimal (leapfrog) joins for cyclic patterns, parallel joins | **have** |
| Property path automata | Closures over adjacency in `nrese-exec` | **have** |
| Memory-bounded operators | Per-query memory budgets (a query over budget stops with an error) | **partial**: spilling of sorts, `DISTINCT` and grouping to disk is a **gap** (plan it with Pf2) |

## 4. I/O

| Oxigraph | NRESE | Status |
|---|---|---|
| Streaming parsers with bounded memory | `nrese-rdf-io`: borrowed quads (`next_ref`, no allocation per term), readers that refill without splitting tokens; JSON-LD streams by top-level element | **have** |
| Exact error positions | Line, column, byte offset | **have** |
| Error recovery (skip a bad statement, go on) | Line formats go on after a bad line; Turtle, TriG and RDF/XML stop at the first error | **partial**: a recovery mode for Turtle and TriG (skip to the next `.` at statement level) belongs to 3f |
| N-Triples, N-Quads, Turtle, TriG, RDF/XML | Every W3C 1.1 test passes (70, 87, 313, 357, 166); N-Triples and N-Quads in parallel chunks | **have** |
| JSON-LD 1.1 | toRdf 455/455, expand 376/376, fromRdf 53/53; streaming and expanded writers. (The list says `oxjsonld` does compaction; version 0.2.4 has expansion and toRdf, and a streaming writer, but no compaction or fromRdf algorithm.) | **have** (beyond `oxjsonld`) |
| N3 (lexing and structure) | Not read | **decide** (see §8) |
| SPARQL results: XML, JSON, CSV, TSV, booleans | Written natively from id tables (JSON, TSV, CSV); XML and the readers through `sparesults` until `nrese-sparql-results` | **planned** (step 4) |

## 5. Modern and draft standards

| Oxigraph | NRESE | Status |
|---|---|---|
| RDF 1.2 triple terms, `rdf:reifies`, `<<( … )>>`, `<< … ~ r >>`, `{| … |}` | 3e (terms and all syntaxes), with the W3C `rdf12` suites | **planned** |
| Triple terms interned by hash-derived ids, so the permutations stay as they are | The roadmap's R7: a triple term's dictionary key is its three component ids; the seven permutations are unchanged | **planned** (step 6) |
| SPARQL 1.2 matching of triple terms and reifiers, `BIND(<<( … )>> AS ?t)`, `TRIPLE`, `SUBJECT`, `PREDICATE`, `OBJECT`, `isTRIPLE`, update over triple terms | Step 4 (grammar), step 6 (evaluation), with the W3C `sparql12` tests | **planned** |
| SEP-0006 / SEP-0007 `LATERAL` | Never supported (the baseline's `spargebra` didn't enable it) | **gap** → step 4 (grammar, behind a feature) and step 6 (evaluation: per-row correlation, as Jena does) |
| SEP-0002: date, time and duration arithmetic, `ADJUST`, `SUM`/`AVG` of durations, g-types | A regression found while writing this matrix (the baseline ran these through spareval's `sep-0002` and `calendar-ext`; step 1 removed spareval), **fixed**: XPath F&O 3.1 arithmetic on dates, times and the two duration subtypes, `ADJUST` for dateTime, date and time, the duration partial order in comparisons and `ORDER BY`, the extractors on `xsd:time` and the g-types, casts to all of them, and `SUM`/`AVG` of durations, which spareval 0.2.6 didn't have. What XPath leaves undefined (arithmetic on `xsd:duration` itself, `ADJUST` of g-types) is an error, where spareval answered | **have** (`nrese-sparql/tests/calendar_tests.rs`, the F&O examples in `native/calendar.rs`) |
| Extractors `YEAR` … `TZ` across all calendar types | `YEAR`, `MONTH`, `DAY` for dates and the g-types that have the part; `HOURS`, `MINUTES`, `SECONDS` for `dateTime` and `time`; `TIMEZONE`, `TZ` for all | **have** |
| SPARQL 1.2 Protocol and Graph Store Protocol drafts (4xx for bad requests, 5xx for failures; RDF 1.2 media types) | 4xx/5xx already differentiated; 1.2 media types and profiles with step 6 | **partial** |
| Results formats with triple terms (JSON `"type": "triple"`, XML, TSV escaping) | Step 4 | **planned** |

**Feature flags.** As Oxigraph does with `rdf-12`, `sep-0002`, `sep-0006`: draft features
sit behind Cargo features of the crates that implement them (`rdf-12`, `sparql-12`,
`sep-0002`, `sep-0006`), on by default in the server once their W3C suites pass, so that
an embedder can build without a draft.

**Draft volatility.** Every suite is pinned to a commit. A change of a draft that breaks
syntax or behaviour is a deliberate update of the pin, with its test results, and a
version bump of the crates it changes (minor while they are 0.x).

## 6. Crates and delivery

| Oxigraph | NRESE | Status |
|---|---|---|
| `oxrdf`, `oxsdatatypes`, `oxttl`, `oxrdfxml`, `oxjsonld`, `oxrdfio` | `nrese-rdf`, `nrese-xsd`, `nrese-rdf-io` (+ `nrese-json`) | **have** |
| `spargebra`, `sparesults` | `nrese-sparql-syntax`, `nrese-sparql-results` | **planned** (step 4) |
| `sparopt`, `spareval` | `nrese-sparql` (planner, native executor), `nrese-exec` | **have** |
| `spargeo` | GeoSPARQL in `nrese-sparql` with an R-tree | **have** |
| `oxigraph` (`Store`, transactions, bulk load) | `nrese-engine` and `nrese-store` | **have** |
| CLI: load, convert, validate, dump, batch queries | `nrese-server load` (100 M triples in 37 s), configuration check, backup and restore through the API; no format conversion or batch query subcommands | **partial**: add `convert` and `query` subcommands (small; reuse `nrese-rdf-io` and the store) |
| HTTP server with SPARQL and Graph Store Protocols | `nrese-server` | **have** |
| Web UI (YASGUI) | `nrese-console` (browser UI and CLI) | **have** |
| Docker images, multi-arch | A `Dockerfile`; multi-arch builds not set up | **partial** (packaging, E5) |
| Python bindings (`pyoxigraph`), RDFLib store | None | **decide** |
| WASM / npm package | None | **decide** |
| TLS client for `SERVICE` (rustls) | `reqwest` with rustls, bundled roots; no OpenSSL | **have** |

## 7. Oxigraph's limits, and how NRESE avoids them

These are requirements, not just comparisons:

1. **Heuristic optimiser.** NRESE plans by cost (statistics, DP join order) and shows it
   (`explain`). Still to do so that no query shape falls back to written order: join
   ordering across BGPs, paths and subqueries, paths from their bound end,
   characteristic sets, and estimation error tracked in the perf lab (Pf4).
2. **Single writer.** NRESE also commits one writer at a time, but a commit costs time
   proportional to its delta (2 ms durable at 10 M), and reads never wait. For write-heavy
   ingestion: group commit (several requests in one WAL fsync) is a **gap** worth planning;
   clustering, sharding and replication are a product decision (**decide**): read replicas
   fed by the WAL fit the design best.
3. **RocksDB coupling.** Avoided: the storage is our own and pure Rust (musl and
   cross-compilation work without a C++ toolchain). Write amplification of seven
   permutations remains: bulk loads write sorted runs directly, and compaction is
   size-tiered; WAL and compaction metrics (O3) are missing to watch it in production.
4. **No reasoning.** NRESE materialises RDFS, OWL-Horst, OWL 2 RL and QL, maintains them
   incrementally on every commit (DRed), checks consistency with explanations, and
   classifies OWL 2 EL; SHACL Core validates on request (on commit planned, C2).
5. **No full-text search.** NRESE has `bds:search` with relevance, but in memory and built
   at the first search; a persisted index (tantivy behind our trait), Jena's `text:query`
   and GraphDB-style connectors are planned (T1, F1). **No spatial index**: NRESE has an
   R-tree for GeoSPARQL.

## 8. For the owner to decide

Each is outside the current use cases (DMW, ResearchSpace, the HisQu workload) as far as
the plan knows; parity on them is a choice:
- **N3** (a W3C Community Group format, not a Recommendation): read it, or leave it out.
- **Python bindings** (PyO3, an RDFLib store) and **WASM/npm**: the bundle crates could be
  published for both cheaply; the full engine in WASM needs a single-threaded in-memory
  build.
- **Scale-out**: read replicas from the WAL, or sharding.

## 9. What this matrix added to the plan

- Done: the SEP-0002 regression (calendar arithmetic, `ADJUST`, duration aggregates) fixed
  in the native executor.
- 3e: RDFC-1.0 next to RDF 1.2 in `nrese-rdf`; 3f: an error-recovery mode for Turtle and
  TriG.
- Step 4: `LATERAL` (SEP-0006/7) in the grammar; step 6: its evaluation.
- Engine backlog: spilling for sorts, `DISTINCT` and grouping; group commit; more inlined
  types; the CLI's `convert` and `query`.
- Feature flags for the drafts and the versioning rule for draft changes (§5).
