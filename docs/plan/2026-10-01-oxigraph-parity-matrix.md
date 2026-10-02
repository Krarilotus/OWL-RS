# Oxigraph parity matrix

> A record of its date. Its status columns and lines are not kept up to date: the
> current status is in [STATUS.md](../STATUS.md).

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
| Key encoding: prefix compression, varints | 64-bit term ids, bit-packed runs (peak memory on LUBM(100) 5.5 → 2.5 GB); checkpoints are memory-mapped and used in place, at restart and once written (a restart opens DBpedia's 67 M quads in milliseconds at 13 MiB resident) | **have** |
| Term dictionary, two-way | Dictionary encoding to 64-bit `TermId`s | **have** |
| Inlined values (numbers, booleans, dates) | Canonical `xsd:integer`, `xsd:boolean`, `xsd:decimal`, `xsd:date`, `xsd:dateTime` and the integer-derived types (`xsd:int`, `long`, ..., 2 October) inlined, only canonical forms, so `"01"` and `"1"` stay distinct terms (RDF-correct); ids sort by value, so ranges, ORDER BY and aggregates read ids; doubles and floats not inlined | **have** (doubles: open) |
| 128-bit hashes for interned strings | A dictionary with exact lookup; no hash-identity risk | **have** (different design) |
| Snapshot isolation, non-blocking readers | MVCC: a snapshot is an `Arc` of the run list; readers never block | **have** |
| Single writer, atomic batches | Single writer slot; mutation pipeline plan → validate (reasoning, SHACL) → commit, delta-proportional | **have** (see §7 for write concurrency) |
| Crash consistency | WAL replay, torn tails truncated, crash-injection tests, hard-kill restart at 10 M exact | **have** |

## 2. Standards

| Oxigraph | NRESE | Status |
|---|---|---|
| RDF 1.1 Concepts and abstract syntax | `nrese-rdf` | **have** |
| RDF 1.1 Semantics, literal value spaces | `nrese-xsd` (strict XSD lexical forms, XPath operations) | **have** |
| RDF 1.2 (`rdf-12` feature): triple terms | Part of the model, not a feature: terms and every syntax (3e, all W3C `rdf12` syntax suites pass); SPARQL (4), engine (6) | **have** (model, syntaxes); SPARQL and engine **planned** |
| RDF Dataset Canonicalization (RDFC-1.0) | `nrese_rdf::rdfc` (feature `rdfc`, on by default): SHA-256 and SHA-384, the issued-label map, a work limit against poison graphs that grows with the input; W3C `rdf-canon` 86/86. Faster than `oxrdf`'s RDFC-1.0: 0.70 of its time with distinct blank nodes, 0.38 on symmetric cycles, 0.03 on a long chain of ties (0.2 s against 6.9 s), and it needs no large caller stack where oxrdf's overflows | **have** (beyond `oxrdf`) |
| SPARQL 1.1 query, patterns, paths, aggregates, modifiers, functions (hashes included) | Native executor: W3C 505/505 (2 October 2026: query, update, syntax, result formats and federation) | **have** |
| SPARQL 1.1 Update (all operations) | W3C update 94/94 | **have** |
| SPARQL 1.1 Federated Query | `SERVICE` to allow-listed endpoints (off by default), bind joins, `SILENT` | **have** |
| SPARQL 1.2 / SPARQL-star | Results formats (4a), grammar (4b) and evaluation (6): all 269 W3C `sparql12` tests pass on the native executor and on the reference | **have** |
| SPARQL 1.1 Protocol | GET, POST form and direct, dataset parameters | **have** |
| Graph Store HTTP Protocol | GET/HEAD/PUT/POST/DELETE, W3C status codes, six formats | **have** |
| XSD date/time and duration arithmetic and ordering | `nrese-xsd`, used by the executor (see §5) | **have** |
| GeoSPARQL (WKT, simple features, relations) | Filter functions over WKT, GeoJSON and GML, DE-9IM, geodesic measures, relations as triple patterns, **with an R-tree** | **have** (beyond Oxigraph's `spargeo`) |

## 3. Query pipeline

| Oxigraph | NRESE | Status |
|---|---|---|
| Recursive descent parser to algebra (`spargebra`) | `nrese-sparql-syntax`: one pass, no backtracking, deterministic generated names, bounded nesting, errors with line and column; all 1,289 W3C syntax tests of SPARQL 1.0–1.2 pass with exact write–parse round trips; 1,189 of 1,197 files parse to spargebra's algebra, the 8 others are spargebra errors (`results/2026-10-01-step-4b-syntax.md`); parsing in 0.35–0.66 and writing in 0.53–0.79 of spargebra's time. The engine parses with it since step 5 | **have** |
| Heuristic optimiser (`sparopt`): filter pushdown, join reordering by bound variables, constant folding | Cost-based: distinct statistics, dynamic-programming join order within a basic graph pattern, filter pushdown below joins, OPTIONAL, UNION, MINUS, BIND and into subqueries, `explain` with estimated and actual rows | **have** within a BGP; **partial** across BGPs, paths and subqueries (Pf4) |
| Volcano pull pipeline | Vectorised id tables, streaming results in 64 KiB chunks with backpressure | **have** |
| Nested-loop, hash and merge joins | Merge, hash and index joins on permutation order, worst-case-optimal (leapfrog) joins for cyclic patterns, parallel joins | **have** |
| Property path automata | Closures over adjacency in `nrese-exec` | **have** |
| Memory-bounded operators | Per-query memory budgets; a GROUP BY over a basic graph pattern that outgrows them streams instead (morsels of its smallest pattern grouped into partial aggregates and merged, `native/stream.rs`), and a GROUP BY over a cross product is grouped in chunks | **partial**: `DISTINCT`, `ORDER BY` without `LIMIT` and results without aggregation still hold their whole input; spilling them to disk is a **gap** |

## 4. I/O

| Oxigraph | NRESE | Status |
|---|---|---|
| Throughput against oxttl, oxrdfxml, oxjsonld | Faster in every case measured (`benches/oxigraph-comparison/results/2026-10-01-step-3-io.md`): parsing 1.1–3.3×, writing 1.1–2×, N-Triples on 16 threads 3.2×; Turtle and TriG split exactly for parallel parsing (oxttl's splitter is a heuristic) | **have** |
| Streaming parsers with bounded memory | `nrese-rdf-io`: borrowed quads (`next_ref`, no allocation per term), readers that refill without splitting tokens; JSON-LD streams by top-level element | **have** |
| Exact error positions | Line, column, byte offset | **have** |
| Error recovery (skip a bad statement, go on) | Line formats go on after a bad line; Turtle and TriG with `RdfParser::recovering` (skip to the `.` ending the statement, or the `}` ending its graph block); `nrese-server load --skip-errors` logs, counts and skips; RDF/XML and JSON-LD stop at the first error | **have** (RDF/XML, JSON-LD: open) |
| N-Triples, N-Quads, Turtle, TriG, RDF/XML | Every W3C 1.1 test passes (70, 87, 313, 357, 166); N-Triples and N-Quads in parallel chunks | **have** |
| JSON-LD 1.1 | toRdf 455/455, expand 376/376, fromRdf 53/53; streaming and expanded writers. (The list says `oxjsonld` does compaction; version 0.2.4 has expansion and toRdf, and a streaming writer, but no compaction or fromRdf algorithm.) | **have** (beyond `oxjsonld`) |
| N3 (lexing and structure) | Read and written (`nrese_rdf_io::n3`): formulas, quick variables, paths, `has`/`is … of`/`<-`, `=`/`=>`/`<=`, any term anywhere; plain RDF in N3 loads like Turtle. W3C N3 parser suites 1,084 of 1,085 (the one: cwm's own literal canonicalisation). N3 rules are the reasoner's user rules (`NRESE_REASONING_RULES`): datalog rules, `=> false` consistency rules, `log:equalTo`/`log:notEqualTo`, facts; materialised and maintained on commits like the built-in rulesets; the W3C N3 reasoner tests in that scope pass (13/13) | **have** (syntax, datalog rules); builtins **planned** |
| SPARQL results: XML, JSON, CSV, TSV, booleans | `nrese-sparql-results`: streaming readers (JSON, XML, TSV) and writers (all four), the W3C result files read as `sparesults` reads them (372/372), faster in every case (writing 0.29–0.91 of its time, reading 0.45–0.90; `benches/oxigraph-comparison/results/2026-10-01-step-4a-results.md`); CSV quotes IRIs with commas and writes triple terms as §2.2 says, where `sparesults` doesn't. The engine writes every results term through it (`write_term`) since step 5 | **have** |

## 5. Modern and draft standards

| Oxigraph | NRESE | Status |
|---|---|---|
| RDF 1.2 triple terms, `rdf:reifies`, `<<( … )>>`, `<< … ~ r >>`, `{| … |}`, directional literals | 3e: terms and all syntaxes (N-Triples, N-Quads, Turtle, TriG, RDF/XML with `parseType="Triple"`, `rdf:annotation` and `its:dir`); all 336 W3C `rdf12` syntax tests pass | **have** |
| Triple terms interned by hash-derived ids, so the permutations stay as they are | R7: a triple term's dictionary key is its three component ids (25 bytes); the seven permutations are unchanged; a lazily built index of triple terms by component | **have** (step 6) |
| SPARQL 1.2 matching of triple terms and reifiers, `BIND(<<( … )>> AS ?t)`, `TRIPLE`, `SUBJECT`, `PREDICATE`, `OBJECT`, `isTRIPLE`, update over triple terms | Step 6: triple-term patterns rewritten to scans and `SUBJECT`/`PREDICATE`/`OBJECT` (every operator applies), triple terms in `VALUES`, CONSTRUCT and update templates; `LANGDIR`, `hasLANG`, `hasLANGDIR`, `STRLANGDIR`; `=` on triple terms by value part by part. W3C `sparql12` 269/269; a differential test (2,400 queries, ordered and not) against the reference | **have** |
| SEP-0006 / SEP-0007 `LATERAL` | Parsed (a parser option, on by default) and evaluated natively (2 October): the right side once per distinct value of the left variables it mentions, by substitution (as `EXISTS` with correlation), so a `LIMIT`, `ORDER BY` or aggregate inside applies per left solution; a subquery sees a left variable where it projects it (`nrese-sparql/tests/lateral_tests.rs`) | **have** |
| SEP-0002: date, time and duration arithmetic, `ADJUST`, `SUM`/`AVG` of durations, g-types | A regression found while writing this matrix (the baseline ran these through spareval's `sep-0002` and `calendar-ext`; step 1 removed spareval), **fixed**: XPath F&O 3.1 arithmetic on dates, times and the two duration subtypes, `ADJUST` for dateTime, date and time, the duration partial order in comparisons and `ORDER BY`, the extractors on `xsd:time` and the g-types, casts to all of them, and `SUM`/`AVG` of durations, which spareval 0.2.6 didn't have. What XPath leaves undefined (arithmetic on `xsd:duration` itself, `ADJUST` of g-types) is an error, where spareval answered | **have** (`nrese-sparql/tests/calendar_tests.rs`, the F&O examples in `native/calendar.rs`) |
| Extractors `YEAR` … `TZ` across all calendar types | `YEAR`, `MONTH`, `DAY` for dates and the g-types that have the part; `HOURS`, `MINUTES`, `SECONDS` for `dateTime` and `time`; `TIMEZONE`, `TZ` for all | **have** |
| SPARQL 1.2 Protocol and Graph Store Protocol drafts (4xx for bad requests, 5xx for failures; RDF 1.2 media types) | 4xx/5xx already differentiated; 1.2 media types and profiles with step 6 | **partial** |
| Results formats with triple terms (JSON `"type": "triple"`, XML, TSV escaping) | `nrese-sparql-results`: triple terms and directional literals (`its:dir`) in all four formats, read and written; the engine writes them through it | **have** |

**Feature flags.** As Oxigraph does with `sep-0002` and `sep-0006`: draft features sit
behind Cargo features of the crates that implement them (`sparql-12`, `sep-0002`,
`sep-0006`), on by default in the server once their W3C suites pass, so that an embedder
can build without a draft. RDF 1.2 itself is not one (decided with 3e, see the migration
plan's status): a gated `Term` variant would split every match into two builds, RDF 1.2
Concepts is a Candidate Recommendation, and 1.1 documents read and write unchanged.

**Draft volatility.** Every suite is pinned to a commit. A change of a draft that breaks
syntax or behaviour is a deliberate update of the pin, with its test results, and a
version bump of the crates it changes (minor while they are 0.x).

## 6. Crates and delivery

| Oxigraph | NRESE | Status |
|---|---|---|
| `oxrdf`, `oxsdatatypes`, `oxttl`, `oxrdfxml`, `oxjsonld`, `oxrdfio` | `nrese-rdf`, `nrese-xsd`, `nrese-rdf-io` (+ `nrese-json`) | **have** |
| `sparesults` | `nrese-sparql-results` | **have** (step 4a; the engine uses it since step 5) |
| `spargebra` | `nrese-sparql-syntax` | **have** (step 4b; the engine uses it since step 5) |
| `sparopt`, `spareval` | `nrese-sparql` (planner, native executor), `nrese-exec` | **have** |
| `spargeo` | GeoSPARQL in `nrese-sparql` with an R-tree | **have** |
| `oxigraph` (`Store`, transactions, bulk load) | `nrese-engine` and `nrese-store` | **have** |
| CLI: load, convert, validate, dump, batch queries | `nrese-server load` (100 M triples in 37 s), `query` (one query on the store, results to standard output), `convert` (one RDF file into another format, streamed), configuration check, backup and restore through the API | **have** |
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
- **N3**: decided 1 October (owner: as much compatibility as can be optimised): read
  and written; N3 rules as user-defined rule sets of the reasoner.
- **Python bindings** (PyO3, an RDFLib store) and **WASM/npm**: the bundle crates could be
  published for both cheaply; the full engine in WASM needs a single-threaded in-memory
  build.
- **Scale-out**: read replicas from the WAL, or sharding.

## 9. What this matrix added to the plan

- Done: the SEP-0002 regression (calendar arithmetic, `ADJUST`, duration aggregates) fixed
  in the native executor.
- 3e: RDFC-1.0 next to RDF 1.2 in `nrese-rdf` (done 1 October); 3f: an error-recovery mode
  for Turtle and TriG.
- Step 4: `LATERAL` (SEP-0006/7) in the grammar; step 6: its evaluation.
- Engine backlog: spilling for sorts, `DISTINCT` and grouping; group commit; more inlined
  types; the CLI's `convert` and `query`.
- Feature flags for the drafts and the versioning rule for draft changes (§5).
- N3 (owner's decision, 1 October): the syntax is done; next, N3 rules (datalog form,
  `=> false`, `log:notEqualTo` and the common comparison and arithmetic builtins) compiled
  into the reasoner's rule IR as user-defined rule sets, which GraphDB's custom rulesets
  are also a parity item for. Full EYE-level N3 (backward rules, scoped negation, the
  whole builtin library) only if a use case needs it.
- Benchmark targets (owner, 1 October): NRESE on the Oxigraph libraries (the baseline
  branch) is a system of its own in the suite, next to NRESE on the bundle and the other
  engines.
