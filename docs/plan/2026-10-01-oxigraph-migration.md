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

**Deviations kept from the spareval era, to fix for conformance** (they sit in code the
reference shares, so no differential test can see them; W3C doesn't test them):
- `STR` of a typed literal gives the canonical form (`STR("03"^^xsd:integer)` = `"3"`);
  §17.4.2.5 gives the lexical form. It was changed to match spareval on purpose (ROADMAP).
- `MIN`, `MAX` and `SAMPLE` return canonicalised terms, derived integer types as
  `xsd:integer`; the specification returns the term itself.
- The generator carve-outs of the differential tests that exist only because spareval
  departed from the specification (DATATYPE of derived integers, subqueries and VALUES
  under `GRAPH`, a filter moved into a subquery): re-enable them now that the oracle is
  the specification.

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

Acceptance of IRIs agrees completely. Still to do in step 2: the throughput benchmarks
(run in bulk at the end of the step), then the switch of `nrese-sparql`'s value code to
`nrese-xsd` and the removal of `vendor/oxsdatatypes`.
