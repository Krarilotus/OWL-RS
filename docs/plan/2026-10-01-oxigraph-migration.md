# Migration away from Oxigraph

As of 1 October 2026 (owner's request: lose the dependency altogether, make what depends on
it independent and more efficient, and keep the replacement modular, so that any piece of
it stays easy to replace).

NRESE doesn't use Oxigraph's store: storage, snapshots, the write-ahead log and the query
executor are its own. What it still takes from the Oxigraph project are libraries:

| Crate | What for | Where |
|---|---|---|
| `oxrdf` | RDF terms, triples, quads, vocabularies, graph canonicalisation | 59 files, every crate |
| `oxiri` | IRI parsing and resolution | `IRI()`, `BASE` |
| `oxsdatatypes` (patched copy in `vendor/`) | XSD numbers, booleans, dates and times | expressions, datatype checks, SHACL |
| `oxrdfio`, `oxttl` | reading and writing RDF files | bulk load, graph store protocol, backups |
| `spargebra` | the SPARQL parser, algebra and writer | 26 files |
| `spareval` | the fallback evaluator (3 of 366 W3C queries, queries over a transaction), the result, error and cancellation types | `nrese-sparql` |
| `sparesults` | SPARQL results formats | federation, the server, tests |
| `oxigraph` | the test oracle (spareval over Oxigraph's store) | differential tests, W3C runner, bench harness |

## The bundle

The replacement is its own bundle of crates under `crates/rdf/`, each behind a small,
documented interface, so that each can later be replaced on its own:

| Crate | Replaces | Contents |
|---|---|---|
| `nrese-rdf` | `oxrdf`, `oxiri` | terms and their borrowed forms, triples, quads, graphs and datasets, vocabularies, IRI parsing and resolution, blank node canonicalisation |
| `nrese-xsd` | `oxsdatatypes` | the XSD datatypes SPARQL and OWL 2 use, their lexical forms kept |
| `nrese-rdf-io` | `oxrdfio`, `oxttl` | N-Triples and N-Quads (parallel), Turtle, TriG, RDF/XML, JSON-LD; readers and writers |
| `nrese-sparql-syntax` | `spargebra` | SPARQL 1.1 query and update: parser, algebra, writer |
| `nrese-sparql-results` | `sparesults` | JSON, XML, CSV, TSV: readers and writers |
| `nrese-sparql` (existing) | `spareval` | its own result, error and cancellation types; the native executor as the only one |
| `nrese-sparql-reference` (tests only) | the `oxigraph` oracle | an evaluator written straight from the spec's algebra, no optimisations, as the reference of the differential tests |

The interfaces follow the shape the code uses today (type and method names), so that
switching is mostly a change of paths, and a replaced crate needs no change elsewhere.

## The yardstick: better than what it replaces

Owner's addition (1 October 2026): each crate is compared with the Oxigraph crate it
replaces, and is done only when it is at least as good on every count:

1. **Standards coverage.** The same conformance suites run against both, side by side,
   and ours passes at least as many. Where a suite doesn't exist, the spec's own examples
   become tests.
2. **Speed.** The same inputs through both, single-threaded and, where it applies, in
   parallel. If ours is slower, profile, find out why and fix it until it isn't; a gap
   that remains is written down with its cause.
3. **Modern, optimised Rust on every level.** Edition 2024; no allocation per term or
   token on hot paths (borrowed terms out of parsers, `memchr` scanning, small `Copy`
   value types); no dynamic dispatch in inner loops; `#![forbid(unsafe_code)]` unless a
   measured gain needs `unsafe`, then with a `SAFETY` comment and a test; release builds
   with fat LTO and one codegen unit (already the workspace profile).
4. **Separation and configuration.** Each crate does one thing and depends only on the
   crates below it (`nrese-xsd` knows nothing of RDF; `nrese-rdf` connects to it through
   an optional feature). Optional parts sit behind features (formats, RDF 1.2, parallel
   parsing). Behaviour that depends on the use case is an option with a tuned default
   (strict or lenient validation, error recovery, input limits for the server).
5. **Fewer bugs over time.** Conformance suites in CI; round-trip properties
   (`parse(write(x)) == x`); fuzz targets for every parser; and, while the migration
   lasts, differential tests against the Oxigraph crate on the same corpora and on
   generated input, each difference either a fix or a documented deviation.

**Where the comparison lives.** `benches/oxigraph-comparison`, a crate outside the
workspace (like the bench harness), depends on both bundles: a conformance matrix (suite ×
implementation → passed/failed/skipped), the differential tests, and the throughput
benchmarks. The workspace itself stays free of Oxigraph crates after step 5, and the
comparison keeps working. Oxigraph is open source (MIT/Apache-2.0), so its numbers may
be published; the vendor-permission rule for GraphDB, RDFox and others does not apply.
Following the batch rule, the benchmarks run together at the end of each step, after
the implementation and its tests.

### Design decisions per crate

**`nrese-rdf`.** Terms as `oxrdf` has them (owned and borrowed forms, N-Triples `Display`),
plus: language tags checked against BCP 47's full grammar (RFC 5646), IRIs against RFC
3987 in one pass with the positions of their parts kept; graphs and datasets as ordered
sets with range lookups; canonicalisation by hashing neighbourhoods to a stable partition
and branching on ties. Later, behind a feature: RDF 1.2 triple terms and base direction
(`@en--ltr`), once the engine can store them. To measure: IRI parsing and resolution,
term formatting, canonicalisation against `oxiri`/`oxrdf`.

**`nrese-xsd`.** No RDF dependency. Values are `Copy`:
- `Integer` 64-bit, `Decimal` 128-bit fixed point with 18 fractional digits (the ranges
  XSD 1.1 asks of a minimal implementation, and what is in use today). Multiplication
  and division go through 256-bit intermediates, exact up to truncation, so the class of
  bugs the vendored patch fixed can't recur and no intermediate overflow is spurious.
- `Float` and `Double` read only the XSD lexical forms (`oxsdatatypes` hands them to
  Rust's parser, which also takes `inf`, `infinity` and `nan`).
- Output follows XPath 3.1's cast to string (what `STR()` yields: `10`, `1.0E20`,
  `1.0E-7`), with the XSD 1.1 canonical form as a separate method.
- Dates and times (`DateTime`, `Date`, `Time`, the `G…` types, the three durations,
  `TimezoneOffset`) with the XPath comparisons (timeline, implicit timezone) and
  arithmetic (`dateTime ± duration`, `dateTime − dateTime`, duration arithmetic). Today's
  executor lacks the arithmetic, so this adds features.
- Checked: XSD 1.1 and XPath F&O examples as tests; the differential tests against
  `oxsdatatypes` over generated lexical forms and operations.

**`nrese-rdf-io`.** A byte-level lexer over `&[u8]` with `memchr`, fed by a buffered
reader, so files stream. Two interfaces: a push interface handing out borrowed
`TripleRef`/`QuadRef` (no allocation unless an escape sequence needs one), and an
iterator of owned quads. N-Triples and N-Quads in parallel by splitting the input at line
boundaries; Turtle and TriG in parallel is a research item (a pre-scan for statement
boundaries and prefix state). Options: base IRI, strict or unchecked IRIs, error recovery
(skip a bad statement and report it), limits on sizes. Writers: N-Triples, N-Quads,
Turtle and TriG (prefixes, grouping), RDF/XML, JSON-LD. Checked by the W3C rdf-tests suites
(N-Triples, N-Quads, Turtle, TriG, RDF/XML, including negative and evaluation tests) and
the JSON-LD 1.1 toRdf/fromRdf suites. To measure: throughput per format in MB/s
and triples/s against `oxttl`, `oxrdfxml` and `oxjsonld`, and bulk load end to end.

**Step 3 in detail (`nrese-rdf-io`).** What the code needs today:
- the six formats, by file extension or media type;
- parsers with a base IRI, fresh blank nodes per payload, "no named graphs", a default
  graph, input from `Read` or a byte slice;
- N-Triples/N-Quads split into chunks for bulk load on all cores;
- streaming serialisers for CONSTRUCT results, the graph store protocol and backups;
- tests that read Turtle manifests and the OWL 2 RDF/XML file.

The crate is built in five parts, each with its W3C suites and tests before the next:
- **3a. Core.** `RdfFormat`; parse errors with line, column and byte offset; parser and
  serialiser options (base IRI, prefixes, default graph, blank-node renaming, unchecked
  IRIs, size limits); the input layer (a byte slice, or a buffered reader that refills
  without splitting a token). Then N-Triples and N-Quads: a line lexer over bytes with
  `memchr`; terms borrowed from the buffer unless an escape needs a copy; splitting at
  line boundaries for parallel parsing; writers.
- **3b. Turtle and TriG**, one lexer and parser (TriG is a superset): prefixes, base, the
  abbreviations (`;` `,` `[]` collections `a`), numeric and boolean literals, relative
  IRIs resolved into a reused buffer. Writers with prefixes, grouping by subject and
  predicate, and lists.
- **3c. RDF/XML** on `quick-xml` events (MIT; already in the dependency tree through
  `sparesults`, and kept in the replacement): the full grammar (property attributes,
  `rdf:parseType` Resource/Literal/Collection, containers `rdf:li`, `xml:base`,
  `xml:lang`, reification with `rdf:ID`). A writer that nests by subject.
- **3d. JSON-LD 1.1**: to RDF (context processing, expansion, remote contexts only through
  a caller-supplied loader, none by default, for the server's safety) and from RDF
  (expanded, or compacted with a given context). Own streaming JSON lexer; it is shared
  with `nrese-sparql-results` in step 4.
- **3e. Comparison batch**: the conformance matrix and throughput against `oxttl`,
  `oxrdfxml` and `oxjsonld` on generated documents (no third-party data), the end of
  step 3.

Suites (fetched by `scripts/fetch-w3c-tests.sh`): RDF 1.1 N-Triples, N-Quads, Turtle,
TriG, RDF/XML (positive and negative syntax, evaluation compared by isomorphism through
`nrese-rdf`'s canonicalisation), and JSON-LD 1.1 `toRdf` and `fromRdf`. RDF 1.2 (triple
terms, base direction) waits until the engine can store triple terms; Oxigraph has it
behind a feature, so until then it is a known gap. N3 (a W3C Community Group format
oxttl reads) is not used here and stays out unless asked for.

**`nrese-sparql-syntax`.** A syntax tree that keeps what the query says (prefixed names,
source positions for error messages) is separate from the algebra of SPARQL 1.1 §18, and
the translation is a module of its own. The algebra keeps `spargebra`'s variant names
where the meaning is the same, so porting the executor stays mechanical. The writer
prints the syntax tree. Later, behind a feature: SPARQL 1.2. Checked by the W3C SPARQL 1.1
syntax tests (positive and negative, query and update) and by round-trips of every query
in the repository. To measure: parse time over the W3C and benchmark query corpora
against `spargebra`.

**`nrese-sparql-results`.** Streaming readers and writers for JSON (no DOM), XML
(`quick-xml`), CSV and TSV. Checked by the W3C result-format tests and round-trips.
To measure: throughput against `sparesults`.

## Steps

**Baseline.** The branch `baseline/pre-oxigraph-migration` (commit `79983fe`) keeps the
last state with the Oxigraph libraries in place: the fallback if a replacement proves
harder to get right, and the baseline the migrated engine is measured against end to end.

Each step ends green (fmt, clippy, all tests with the W3C suites required) and committed.

| # | Step | Done when |
|---|---|---|
| 1 | **spareval out of the runtime.** Own `QueryResults`, solution and triple iterators, `QueryEvaluationError`, `CancellationToken`, `QueryDatasetSpecification`. The native executor takes every query: `BNODE(label)` per solution, unknown functions as errors, `SERVICE ?endpoint`, EXISTS by substitution where correlation isn't safe, paths under `GRAPH ?g`; queries over a transaction run on its pending snapshot. The reference evaluator replaces spareval as the differential tests' oracle. | `spareval` gone from every `Cargo.toml`; the differential and W3C tests pass |
| 2 | **`nrese-rdf`, `nrese-xsd`,** and `benches/oxigraph-comparison` with their differential tests and benchmarks. | Own tests; at least `oxrdf`'s, `oxiri`'s and `oxsdatatypes`' coverage and speed, or the gap documented; the vendored `oxsdatatypes` is gone |
| 3 | **`nrese-rdf-io`.** Readers and writers; N-Triples and N-Quads chunked and parallel, handing terms to the engine without intermediate strings where it can. | The W3C N-Triples, N-Quads, Turtle, TriG and RDF/XML test suites pass; bulk load at least as fast as before |
| 4 | **`nrese-sparql-syntax`, `nrese-sparql-results`.** | The W3C SPARQL 1.1 syntax tests (positive and negative) pass; results round-trip |
| 5 | **Switch-over.** Every crate on the bundle; the `oxigraph` oracle in the W3C runner and the bench harness replaced. | `cargo tree` lists no crate of the Oxigraph project; all tests pass; the comparison's conformance matrix and benchmarks published in `docs/`; end to end against the baseline branch: the same conformance results or better (W3C SPARQL 1.1, OWL 2 RL, GeoSPARQL, SHACL) and no performance regression on the perf-lab workloads (bulk load, query latency, reasoning), or each one explained and accepted by the owner |

Steps 3 and 4 are each done only when they also meet the yardstick above for their crates.

## Status

Updated as the steps land.

**Step 1 (spareval out of the runtime).** The native executor takes every query and
update; `spareval` is gone from the runtime. The reference evaluator
(`nrese-sparql-reference`) passes the W3C SPARQL 1.1 suite by itself and is now the oracle
of the differential tests. Switching the oracle found:
- a bug in the new EXISTS-by-substitution path: under `GRAPH ?g` it asked in every graph,
  not the row's own (fixed);
- test defects that spareval had hidden by agreeing by accident: a `SAMPLE` normalisation
  that never normalised, CONSTRUCT over a `LIMIT` without `ORDER BY`, and an update
  oracle that read only asserted statements while queries read inferred ones too (fixed);
- a deviation spareval shared with the executor, so no test could see it: inside
  `GRAPH ?g { P }` an expression saw `?g` bound, where §18.6 evaluates P first and binds
  `?g` after (`BIND(?g AS ?x)` inside gives an unbound `?x`). Fixed in the executor.
- bugs in the new reference itself, found by the executor: an EXISTS memo keyed by a
  graph's address (reused after the graph was dropped), a lookup by object that skipped
  the predicate check, and named graphs judged by the read model's statements (the read
  model filters statements, not graphs). Fixed, with a regression test for the index.

The whole differential suite runs in about 20 seconds (it was over 30 minutes before the
reference got hash joins, adjacency lists for closures and per-term indexes). W3C SPARQL
1.1: 491 of 495, the 4 failures one known deviation (zero-length paths from a term outside
the graph); the reference passes the same 491. Workspace: 535 tests, all green.

**Deviations kept from the spareval era, now fixed.** They sat in code the reference
shares, so no differential test could see them, and W3C doesn't test them:
- `STR` of a typed literal gave the canonical form (`STR("03"^^xsd:integer)` = `"3"`);
  §17.4.2.5 gives the lexical form, which storage keeps (only canonical literals are
  inlined). Now the lexical form; a cast to `xsd:string` stays canonical (XPath §19).
- `MIN`, `MAX` and `SAMPLE` returned canonicalised terms, derived integer types as
  `xsd:integer`; they now return the term itself (§18.5.1).

**Every generator carve-out is gone.** The differential tests had stayed out of what
spareval got wrong; with the reference as oracle they now cover these too:
- `DATATYPE` of derived integers;
- zero-length paths from variables bound to literals;
- paths of every kind with both ends bound;
- rows without statements (`BIND`, `VALUES`) under any graph name, including one outside
  the dataset;
- subqueries and `MINUS` under `GRAPH ?g`;
- EXISTS with subqueries under `GRAPH ?g`;
- `SUM(DISTINCT)` and an all-errors `COUNT(DISTINCT …)`.

Only where SPARQL leaves the choice open are integer literals compared by value: which of
equal values `MIN`/`MAX` give, and their order under `ORDER BY`. This found one more
divergence, resolved in the native executor's favour: an update template may put quads
in a graph named by a blank node (RDF 1.1 datasets allow it, the store holds such graphs,
and `GRAPH ?g` finds them), and the reference now does the same.

**Step 2 (`nrese-rdf`, `nrese-xsd`), first comparison with Oxigraph.** Both crates are
written and tested on their own. `benches/oxigraph-comparison` runs them against
`oxiri` and `oxsdatatypes` on generated input (200,000 IRI references × 6 bases;
300,000 numeric, 300,000 temporal lexical forms; 200,000 decimal operand pairs). Every
difference was one of these:
- **Oxigraph departs from its spec; nrese follows it, and the test checks that against
  the rule.**
  - `oxiri` keeps dot segments in a reference with an authority, loses the root `/`
    when `..` climbs above a path without an authority, and fails where a result path
    would start with `//`. That is 21,317 of 1.2 million resolutions; an independent
    transcription of RFC 3986 §5.2 decides each.
  - `oxsdatatypes` accepts `24:18:00` (XSD allows hour 24 only as `24:00:00`) and
    durations ending in `T` (`P9YT`).
  - It rejects `--02-29` with a timezone.
  - It prints the wrong minute for years up to 0 with fractional seconds.
  - It rounds `-8.52` to `-8`.
- **nrese's documented choices.**
  - Only the XSD lexical forms of floats (not Rust's `inf`, `nan`).
  - XPath's scientific notation outside [10⁻⁶, 10⁶).
  - Truncation, not rejection, past 18 fractional digits.
  - Decimal multiplication and division are exact to truncation, and every result the
    test could compute exactly matched it.

Acceptance of IRIs agrees completely.

**`nrese-xsd` in use.** `nrese-sparql`, `nrese-store` and `nrese-shacl` use it;
`oxsdatatypes` is in no manifest of the workspace and not in its lockfile, and the
workspace's `[patch]` of it is gone. `vendor/oxsdatatypes` stays only for the bench
harness, a workspace of its own that still uses `oxigraph` as an oracle store; it goes
with step 5. With the stricter lexical forms, XPath string forms, exact decimals and
XPath `ROUND`, the W3C suites and all 535 workspace tests pass unchanged.

**Step 2 done: speed.** [Results](../../benches/oxigraph-comparison/results/2026-10-01-step-2.md):
geometric mean 0.43 of Oxigraph's time over 21 cases. The first versions were up to 12×
slower in places; profiling-led rewrites made them faster:
- IRI parsing in one pass, and resolution that writes once;
- decimal division in decimal chunks, and output through a 64-bit fraction;
- dates of 32 bytes;
- canonicalisation by components with twin pruning.

What remains above 1 is within noise, except random invalid IRI text (about 10% slower).
Canonicalising a 1,000-node blank chain takes 47 ms here and 6.9 s in oxrdf, which also
overflows a 1 MiB stack on it.

**Step 3a, 3b (N-Triples, N-Quads, Turtle, TriG).**
- W3C RDF 1.1 suites, every test: N-Triples 70/70, N-Quads 87/87, Turtle 313/313, TriG
  357/357.
- Each document also goes through a reader that gives one byte per call (every token cut
  by the buffer); each document that parses is written back in its own format and as
  N-Quads, and read again.
- The suite manifests are read with this crate's own Turtle parser; oxttl is no longer
  needed.
- Nesting is limited (128 by default, configurable), so hostile input gets an error rather
  than a stack overflow.
