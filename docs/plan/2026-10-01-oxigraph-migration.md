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
| `nrese-json` | `json-event-parser` | JSON: pull parser over text or a reader, tree, writer, RFC 8785 canonical form; shared by JSON-LD and the SPARQL JSON results |
| `nrese-rdf-io` | `oxrdfio`, `oxttl`, `oxrdfxml`, `oxjsonld` | N-Triples and N-Quads (parallel), Turtle, TriG, RDF/XML, JSON-LD; readers and writers |
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
   an optional feature). Optional parts sit behind features (formats, parallel parsing;
   RDF 1.2 is part of the model, see 3e). Behaviour that depends on the use case is an option with a tuned default
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
and branching on ties. RDF 1.2 triple terms and base direction (`@en--ltr`) are part of
the model (3e), not a feature. To measure: IRI parsing and resolution,
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
  with `nrese-sparql-results` in step 4. The design:
  - **`nrese-json`**, a crate of its own (`memchr` and `thiserror` only): a pull parser over a `&str`
    (strings without escapes borrowed, so no copy) or a reader (refilled, never splitting
    a token), strict RFC 8259 with a depth limit; a DOM (`Value`, borrowed where it can
    be, object entries in document order); a writer with fast escaping; the JSON
    Canonicalization Scheme (RFC 8785) with ECMAScript number formatting, for `@json`
    literals.
  - **Streaming where it is exact.** JSON-LD's algorithms work on whole documents, but a
    top-level array, and the `@graph` of a top-level object whose only other entry is
    `@context` (wherever it is written), are expanded and converted one element at a
    time: memory is the document's bytes plus one element, not a tree of all of it.
    Anything else is read whole. oxjsonld buffers the whole document unless its streaming
    profile is asked for, which requires a key order.
  - **Typed expanded form.** Expansion produces node, value and list objects as Rust types
    (IRIs shared as `Arc<str>` from the term definitions), not JSON maps; toRdf walks
    them directly. That gives the node map's triples without building the node map (the
    output is a set either way). The `expand` suite checks expansion through a writer of
    the typed form.
  - **Contexts** are cheap to clone (shared term table, copied on write); processing a
    scoped context against the same active context is memoised, as in data every node of
    a type or every value of a property applies the same one. Remote contexts and
    `@import` only through a loader the caller supplies, cached per document, with a
    depth limit against recursion.
  - **Blank nodes** need no table: a label that is a valid N-Triples label and doesn't
    start with `j` stays as written, any other one becomes `jx` and its bytes in
    hexadecimal, and nodes without `@id` get `jg` and a counter: three disjoint sets.
    Under `rename_blank_nodes` all of them get fresh names, as in the other formats.
  - **Options**: processing mode (1.0, 1.1), `rdfDirection` (none, `i18n-datatype`,
    `compound-literal`), an expand context, the loader, and the parser's base IRI.
  - **Writers**: a streaming one (subjects grouped, IRIs compacted with the given
    prefixes, plain strings as JSON strings) for CONSTRUCT and the graph store; and the
    spec's "Serialize RDF as JSON-LD" (expanded, lists as `@list`, native types and
    `rdf:type` options), which needs the whole dataset and so collects the quads until
    `finish`. oxjsonld has only the first, without lists.
  - Out of scope: compaction, flattening and framing as API algorithms, HTML script
    extraction, generalized RDF (blank-node predicates are dropped, as the spec does by
    default).
  - Suites: `toRdf`, `expand` and `fromRdf` from `w3c/json-ld-api`.
- **3e. RDF 1.2** in `nrese-rdf` and every reader and writer (see "The newest standards"
  below). Done 1 October with RDFC-1.0 (see Status).
- **3f. Comparison batch**: the conformance matrix and throughput against `oxttl`,
  `oxrdfxml` and `oxjsonld` (with Oxigraph's `rdf-12` feature) on generated documents (no
  third-party data), the end of step 3.

Suites (fetched by `scripts/fetch-w3c-tests.sh`): RDF 1.1 N-Triples, N-Quads, Turtle,
TriG, RDF/XML (positive and negative syntax, evaluation compared by isomorphism through
`nrese-rdf`'s canonicalisation), and JSON-LD 1.1 `toRdf` and `fromRdf`. N3 (a W3C
Community Group format oxttl reads) is not used here and stays out unless asked for.

**The newest standards: RDF 1.2 and SPARQL 1.2** (the owner's request, 2026-10-01: the
engine must be usable with, and optimised for, the newest versions). W3C status in October
2026: RDF 1.2 Concepts is a Candidate Recommendation (April 2026) and RDF 1.2 Semantics a
CR draft; N-Triples, N-Quads, Turtle, TriG and RDF/XML 1.2, and all of SPARQL 1.2 (query,
update, results formats, protocols, entailment), are Working Drafts revised through
September 2026; SHACL 1.2 and JSON-LD 1.2 are in drafts too. No 1.3 of any of them
exists or is chartered. Following the newest standards was one of the reasons to keep
the Oxigraph libraries, so 1.2 is part of the replacement itself, not a later feature: the
migration is finished only when step 6 is. The baseline's support was partial (triple
terms in queries through spareval's general evaluator, none in the store); step 1 removed
that evaluator, so such queries are errors until step 6, the one temporary regression of
the migration. Readers keep accepting 1.1 documents unchanged:
- **`nrese-rdf`**: triple terms as the fourth kind of term (in object position), and
  language-tagged strings with a base direction (`rdf:dirLangString`), through `Term`,
  `TermRef`, canonicalisation, `Graph` and `Dataset`.
- **`nrese-rdf-io`**, as part 3e before the comparison batch: RDF 1.2 in every reader and
  writer — triple terms `<<( s p o )>>`, reified triples `<< s p o ~ r >>`, annotations
  `{| … |}`, `VERSION`, directional literals `"x"@en--ltr`; in RDF/XML
  `rdf:parseType="Triple"`, `rdf:annotation` and `its:dir`; in JSON-LD, `@direction` as
  a directional literal. The W3C `rdf12` suites (already fetched) must pass. Writers
  write 1.2 syntax only when the data needs it.
- **`nrese-sparql-syntax` and `nrese-sparql-results`** (step 4): the SPARQL 1.2 grammar
  (triple terms and reifiers in patterns and templates, `TRIPLE`, `SUBJECT`,
  `PREDICATE`, `OBJECT`, `isTRIPLE`, `LANGDIR`, `hasLANG`, `hasLANGDIR`, `STRLANGDIR`,
  `VERSION`) with the W3C `sparql12` syntax tests, and triple terms in all four results
  formats.
- **The engine** (step 6, after the switch-over): triple terms and directional literals
  in the store's encoding, evaluation and functions (the W3C `sparql12` evaluation
  tests), SHACL 1.2 where its draft has settled.
- The drafts still change: every suite stays pinned to a commit (as the others are), and
  moving a pin is a deliberate change with its test results.

Oxigraph has RDF 1.2 behind its `rdf-12` feature; the comparisons turn it on.

**`nrese-sparql-syntax`: what is forced, what is ours** (revised 1 October, before
writing it, by rethinking the problem rather than porting `spargebra`).

*Forced.*
- **The SPARQL 1.1 and 1.2 grammars and the algebra translation of §18:** what is
  accepted, what is an error, and what each query means.
- **The engine's needs:**
  - an algebra to plan from;
  - SPARQL text from algebra, because federation sends the inner pattern of a `SERVICE`
    to the remote endpoint;
  - the parser options the engine and tests use (base IRI, prefixes, custom aggregate
    functions);
  - the dataset accessors.
- **Porting.** Variant names stay `spargebra`'s where the meaning is the same. That keeps
  the step-5 port mechanical; it doesn't constrain the design.

*Chosen, and why.*
- **One pass, hand-written.** A recursive-descent parser over the bytes, dispatching on
  the next byte or keyword instead of trying alternatives in order as a PEG does. It
  builds the algebra directly, with no token list and no intermediate tree.
  - The separate syntax tree planned earlier is dropped. Its only users would be editor
    tooling (formatting, diagnostics), which can later get source spans in a side table
    without touching the algebra.
- **Deterministic generated names.** Aggregate results, `GROUP BY` expressions, `DESCRIBE`
  IRIs and anonymous blank nodes get numbered names, renumbered past anything the query
  itself uses. `spargebra` uses random 128-bit names, so the same query parses to a
  different algebra each time. Deterministic names make the algebra reproducible: plans
  can be cached by its structure, tests can state the expected algebra, and logs compare.
- **Spec-exact where `spargebra` isn't** (each one checked against the spec and listed in
  the step's results):
  - left-associative `-` and `/` (`spargebra` reads `10 - 5 - 2` as `10 - (5 - 2)`);
  - `-1` in an expression is the literal −1, not a negation computed at run time.
- **A writer that round-trips exactly.** Writing algebra and parsing it back gives the same
  algebra for everything the parser produces. Where SPARQL can't express an
  engine-built shape exactly, the writer chooses an equivalent subquery. `spargebra`'s
  writer can move a `FILTER` over a following join (`Join(Filter(A), B)` comes back as
  `Filter(Join(A, B))`), which changes what a federated query means.
- **Limits instead of crashes.** Nesting depth is bounded and reported as an error. A
  recursive parser without a bound overflows its stack on deeply nested input, which a
  public endpoint can't allow.
- **Configurable.** Base IRI, prefixes, custom aggregates; SPARQL 1.2 and `LATERAL`
  (SEP-0006) can be switched off for strict 1.1 behaviour.
  - The algebra types carry the 1.2 and `LATERAL` variants unconditionally, as `Term`
    carries triple terms (see 3e), so no match splits into two builds.
- **Errors with line, column and what was expected.**

*Checked by.*
- the W3C syntax tests of SPARQL 1.0, 1.1 and 1.2 (positive and negative, query and
  update);
- a differential against `spargebra` over every query of those suites and of the
  repository, comparing algebra up to generated names, with each difference explained;
- write–parse round trips of the same corpus.

*Measured.* Parse time over the same corpus, against `spargebra`.

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
| 3 | **`nrese-rdf-io`.** Readers and writers, RDF 1.1 and 1.2; N-Triples and N-Quads chunked and parallel, handing terms to the engine without intermediate strings where it can. | The W3C N-Triples, N-Quads, Turtle, TriG and RDF/XML test suites, 1.1 and 1.2, and the JSON-LD suites pass; bulk load at least as fast as before |
| 4 | **`nrese-sparql-syntax`, `nrese-sparql-results`,** SPARQL 1.1 and 1.2. | The W3C SPARQL 1.1 and 1.2 syntax tests (positive and negative) pass; results round-trip, triple terms included |
| 5 | **Switch-over.** Every crate on the bundle; the `oxigraph` oracle in the W3C runner and the bench harness replaced. | `cargo tree` lists no crate of the Oxigraph project; all tests pass; the comparison's conformance matrix and benchmarks published in `docs/`; end to end against the baseline branch: the same conformance results or better (W3C SPARQL 1.1, OWL 2 RL, GeoSPARQL, SHACL) and no performance regression on the perf-lab workloads (bulk load, query latency, reasoning), or each one explained and accepted by the owner |
| 6 | **SPARQL 1.2 and RDF 1.2 in the engine.** Triple terms and directional literals in the store's encoding, evaluation and functions; SHACL 1.2 where its draft has settled. | The W3C `sparql12` syntax and evaluation tests pass; no regression on the perf-lab workloads |

Steps 3 and 4 are each done only when they also meet the yardstick above for their crates.
The migration is steps 1 to 6: it is finished when the engine runs on the bundle and
supports RDF 1.2 and SPARQL 1.2.

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

**A regression of step 1, found by the parity matrix and fixed (1 October).** The
baseline evaluated date, time and duration arithmetic, `ADJUST` and the g-type extractors
through spareval's `sep-0002` and `calendar-ext` features; the native executor only did
numbers, so such expressions were errors. Now in the executor (`native/calendar.rs`), by
XPath F&O 3.1, with `SUM` and `AVG` of durations besides.

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

**Step 3c (RDF/XML).**
- W3C RDF/XML 166/166 (the whole active suite; the manifest comments out 7). Each test
  also goes through the trickling reader, and round trips through our own RDF/XML writer
  and through N-Quads.
- The OWL 2 test-case file (over 10,000 statements, DTD entities in namespace
  declarations) reads and round trips. That needed namespaces resolved by the parser
  itself: quick-xml's resolver takes `xmlns:rdf="&rdf;"` unexpanded.
- XML literals in exclusive canonical form.

**Step 3d (JSON-LD 1.1).**
- W3C JSON-LD 1.1 suites, every test a JSON-LD 1.1 processor runs: toRdf 455/455,
  expand 376/376, fromRdf 53/53. Not run: the 21 tests for JSON-LD 1.0 processors only,
  and one of generalized RDF (blank-node predicates, which RDF can't hold).
- Each toRdf document is also read through a reader, and its quads round trip through
  both writers. Every dataset of the RDF 1.1 suites (Turtle, TriG, N-Quads, RDF/XML…)
  now round trips through the streaming JSON-LD writer too.
- `nrese-json` is the JSON layer: strict RFC 8259, strings borrowed unless escaped, a
  depth limit, and RFC 8785 canonical JSON for `@json` literals.
- Streaming as designed: a top-level array, or the `@graph` of a top-level object with
  only a context besides (in any key order), is converted one element at a time; quads
  of earlier elements come out before an error in a later one.
- Remote contexts only through a caller's loader; none by default (an error, not a
  network access).
- The two writers: streaming (prefix-compacted, the streaming profile's key order; an IRI
  that looks like a compact IRI turns its prefix off locally rather than being misread)
  and expanded by the specification's algorithm (lists, native types). Lists of lists
  needed the conversion done innermost first: the specification shares values by
  reference, which owned values don't.
- Found by the suites on the way: an `@id` with the form of a keyword is kept as `null`
  (no triples) rather than dropped; `@included` values are expanded so that non-nodes are
  rejected; a term with a slash is expanded without defining terms; `@base` accepts an
  absolute IRI that isn't well formed, and what resolves against it is dropped.

**N3 (1 October, the owner's decision).** Notation3 as a dialect of the Turtle lexer and
parser (no second copy of input, IRI and error handling; the Turtle path is unchanged),
with its own term type for formulas and variables, a writer that nests formulas back, and
`RdfFormat::N3` for plain RDF in N3. W3C N3 parser suites (w3c-cg/N3, pinned): 1,084 of
1,085, each document also through the trickling reader and a round trip.

**Found on the way: blank node canonicalisation could take unbounded time.** N3 documents
whose formulas share a blank node made `nrese-rdf`'s canonicalisation branch factorially
(over a minute, without end in sight, for 908 statements). It now prunes by automorphisms
(as nauty and bliss do) and canonicalises parts that only touch already distinguished
nodes on their own (component recursion), with foldhash instead of SipHash in refinement:
the same file in 6.7 ms in release; the existing tests run in half the time.

**Test data byte-exact.** The fetch script checked the suites out with Git's line-end
conversion on Windows; now `core.autocrlf=false`, and existing checkouts are rewritten from
local objects. Every suite still passes on the exact files.

**Comparison batch, first round (1 October; `benches/oxigraph-comparison/results/2026-10-01-step-3-io.md`).**
Every reader and writer against oxttl, oxrdfxml and oxjsonld on the same generated
documents, and the engine against the baseline branch. Three gaps found and closed in the
same batch: RDF/XML parsing (was up to 1.3× slower, now 1.1–1.2× faster), Turtle and TriG in
parallel (new, exact; 1.5× oxttl's heuristic splitter on 16 threads), N-Triples and N-Quads
writing (was 1.1× slower, now 1.7–1.9× faster). The engine end to end is at parity with the
baseline, as expected before the switch-over (load through oxttl until step 5), commits
about 20% faster.

**3e: RDF 1.2 in the model and every syntax (1 October).** All W3C RDF 1.2 syntax suites
pass, 336 tests: N-Triples, N-Quads and their canonical forms, Turtle, TriG, RDF/XML. The
RDF 1.1 suites still pass all 993. The runner reads both, writes every document back
through nrese's own writers, and reads Turtle and TriG in 2, 3 and 8 parallel chunks.
- **Model.** `Term::Triple` (object position, nested) and directional language-tagged
  strings (`rdf:dirLangString`). Canonicalisation turns each triple term that holds blank
  nodes into a node with three marker edges, which no data can have, so the existing
  search covers nested terms unchanged.
- **Not a Cargo feature** (deciding §8 of the parity matrix for RDF 1.2):
  - A feature-gated variant of `Term` would split every match in the bundle and the
    engine into two builds.
  - RDF 1.2 Concepts is a Candidate Recommendation.
  - RDF 1.1 documents read unchanged, and writers write 1.2 syntax only for 1.2 data.
  - The drafts' later changes are handled by the pinned suites.
- **Syntaxes.**
  - N-Triples and N-Quads: `VERSION`, `<<( … )>>`, `LANG_DIR`, whitespace inside a
    literal.
  - Turtle and TriG: reified triples `<< s p o ~ r >>`, annotations `~ r {| … |}`,
    `@version` and `VERSION`.
  - RDF/XML: `rdf:version`, `its:dir`, `rdf:parseType="Triple"` (its content's statements
    collected in its frame; ignored without RDF 1.2 in scope), `rdf:annotation` and
    `rdf:annotationNodeID`.
  - N3 terms can hold triple terms (written as RDF 1.2 writes them).
  - JSON-LD writes a directional literal as `@direction`, and refuses triple terms with a
    clear error: JSON-LD 1.1 has none, and JSON-LD 1.2 hasn't defined them yet.
- **Stricter, as the 1.2 suites require.** Language tags must be well-formed BCP 47 (now
  allocation-free, so the checked parsers lose nothing); a direction is exactly `ltr` or
  `rtl`; `^^rdf:langString` and `^^rdf:dirLangString` are errors. The lenient
  (`unchecked`) mode skips the tag check.
- **The exact parallel splitter** counts `<<`/`>>` as nesting and never takes the `|}` of
  an annotation for the end of a TriG block.
- **Throughput unchanged.** A check with the I/O comparison: nrese reads N-Triples in
  0.42 of Oxigraph's time, Turtle in 0.61 and RDF/XML in 0.93 (owned quads), as before.

**3e: RDFC-1.0 (1 October).** `nrese_rdf::rdfc` implements the W3C Recommendation as
written:
- first-degree hashes, canonical labels for unique hashes, N-degree hashes over
  permutations for the rest;
- SHA-256 or SHA-384;
- the label map;
- a work limit against poison graphs, by default 100,000 plus 2,000 per blank node, so long
  chains of ties (quadratic work) pass and crafted cliques stop.

W3C `rdf-canon` (pinned in the fetch script): 86 of 86, the two poison graphs included.

Against `oxrdf`'s RDFC-1.0 (feature `rdfc-10`, same graphs, `bench RDFC`), nrese takes
0.70 of its time with 2,000 distinct blank nodes, 0.38 on five 8-cycles, and 0.03 on a
chain of 1,000 ties (0.2 s against 6.9 s). That needed a second pass: the first version,
written straight from the specification, was 2.7× slower than `oxrdf` on the chain, because
every permutation copied an issuer holding blank-node strings in a SipHash map. Blank nodes
are now numbered once, issuers are small integer tables, and first-degree lines are
written without building quads.

The recursion goes one level deeper per tie along a chain, so inputs over 128 blank nodes
run on a thread with a stack sized to them. A test on a 256 KiB caller stack shows the
need (it overflows without that thread). `oxrdf` overflows the 1 MiB main-thread stack on
the same chain.

**4a: `nrese-sparql-results` (1 October).** Readers and writers for SPARQL results in
JSON, XML, CSV (written only: it is lossy) and TSV, booleans included, with RDF 1.2 triple
terms and directional literals in all four. The API follows `sparesults` (format, parser
for a slice or a reader, serializer), so step 5 can port its users mechanically.
- **How it reads and writes.**
  - Readers stream: JSON on `nrese-json` events, without building a tree. A document
    with `results` before `head` is buffered and still read.
  - XML is read from `quick-xml` events by local name, so a prefixed results namespace
    reads too.
  - Writers fill a reused buffer that goes to the writer in 8 KiB pieces. Their bytes are
    `sparesults`' bytes.
- **Conformance.**
  - Every result file of the W3C SPARQL 1.1 and 1.2 suites reads as `sparesults` reads
    it: 372 of 372.
  - Round trips through a reader that gives one byte per call.
  - Two corrections of `sparesults` in CSV:
    - a triple term is written as SPARQL 1.2 CSV/TSV §2.2 says;
    - an IRI with a comma is quoted.
- **Faster than `sparesults` in every case** (`benches/oxigraph-comparison/results/2026-10-01-step-4a-results.md`):
  - writing JSON and XML takes 0.3 of its time, TSV and CSV 0.76–0.91;
  - reading JSON 0.45–0.59, TSV 0.61–0.68, XML 0.80–0.90.

  Four cases were slower at first and were fixed: CSV and TSV writing, reading XML, and
  the copies in reading JSON. `nrese-json`'s string scan became a single pass over 8-byte
  words, which also serves the JSON-LD reader.

**4b: `nrese-sparql-syntax` (1 October).** The SPARQL 1.1 and 1.2 parser, algebra and
writer, designed as the "forced vs chosen" note above says. It is one pass of hand-written
recursive descent: about 3,800 lines of parser and 6,200 in all.
- **Conformance.**
  - All 1,289 W3C syntax tests of SPARQL 1.0, 1.1 and 1.2 pass: queries and updates,
    positive and negative, plus the queries of every evaluation test. Each one written
    back parses to the same algebra.
  - Against `spargebra`: 1,189 files parse to the same algebra and both reject 198. The
    8 differences are each a `spargebra` error against the specification or a W3C test:
    - it accepts four W3C negative tests and rejects two valid queries;
    - it makes `+`, `-`, `*` and `/` right-associative, so `10 - 5 - 2` is 7;
    - it keeps dot segments in absolute IRIs.
- **Faster than `spargebra`.** Parsing takes 0.35–0.66 of its time, writing 0.53–0.79
  (`benches/oxigraph-comparison/results/2026-10-01-step-4b-syntax.md`).
- **Found along the way.**
  - `SELECT *` removed duplicate variables with a list search, which is quadratic. It
    does so in `spargebra` too.
  - `nrese-rdf` quoted literals one character at a time. It now writes plain runs in one
    piece, which every N-Triples-style output gains from.
- **Choices recorded in tests:**
  - deterministic generated names;
  - nesting bounded at 128: about 330 KiB of stack in a release build, safe on a 1 MiB
    thread;
  - aggregates only in SELECT, HAVING and ORDER BY;
  - SPARQL 1.2 and `LATERAL` as parser options;
  - a writer that keeps a `FILTER`'s scope.

Step 4 is complete. Next is step 5: moving the engine onto the bundle.

**5: the engine on the bundle (1 October).** Every crate of the workspace and the
benchmark harness now runs on `nrese-rdf`, `nrese-xsd`, `nrese-rdf-io`,
`nrese-sparql-syntax` and `nrese-sparql-results`. `cargo tree` lists no crate of the
Oxigraph project, and the vendored `oxsdatatypes` copy is gone (git history keeps it).
- **Mostly a rename.** The bundle keeps the item names the engine used. Where an API
  differed, the bundle gained what is useful in general rather than the call site working
  around it:
  - `Graph::triples_for_predicate`, and lookups whose arguments may be borrowed for less
    long than the graph;
  - `QuerySolution::get` by name, `Variable` or index;
  - the results serialiser taking a solution directly;
  - `nrese_sparql_results::write_term`.
- **RDF 1.2 terms reach the engine.** `oxrdf`, as the baseline built it, had no triple
  terms or base directions; the bundle always has them. The term dictionary now keys
  directional strings and triple terms losslessly: a tag of their own, with N-Triples text
  for triple terms, until R7 keys them by their components' ids in step 6. Other changes:
  - triple terms sort after literals;
  - `STR` of a triple term is an error;
  - an `INSERT DATA` renames blank nodes inside triple terms;
  - a `DELETE` template may hold triple terms;
  - federation sends triple terms without blank nodes as `VALUES`.
- **One results writer.** The engine's direct JSON/TSV/CSV writer had its own copy of the
  term serialisation, which would have dropped a base direction. Each term now goes
  through `nrese_sparql_results::write_term`, about 200 fewer lines.
- **Parallel load of Turtle and TriG.** `nrese-rdf-io` splits these exactly at statement
  ends, which `oxttl` could not, so the bulk loader now parses Turtle and TriG on every
  thread as it does N-Triples and N-Quads.
- **A runaway query could exhaust the machine, before the port too.** In a release build,
  a three-way cross product with no memory budget committed over 200 GB in the 200 ms
  before cancellation reached it: joins never looked at the cancellation flag. Now:
  - the joins' row limit carries the flag and checks it every 1,024 output rows;
  - the cross product checks it after each left row;
  - the test query stops 43 ms after cancelling.

  (The server always had a budget: 4 GiB per query and half the memory for all queries by
  default. The embedded API's default stays unlimited, see the gaps below.)
- **Tests.** All 87 test binaries of the workspace pass in release, 644 tests. The W3C
  results are unchanged:
  - SPARQL 1.1: 491 of 495, the 4 failures shared with the reference evaluator;
  - SHACL Core: 98 of 98;
  - OWL 2 RL: 70 passed, the 16 expected failures, 3 skipped;
  - the bundle's own suites, as in steps 2–4.
- **Still to do for step 5:** the end-to-end comparison with the baseline branch on the
  perf-lab workloads (bulk load, query latency, reasoning).
- **Gap noted:** the embedded `SparqlQueryRequest` has no memory budget unless one is set.
  A default derived from the machine's memory (as the server's) would protect embedders
  too.
