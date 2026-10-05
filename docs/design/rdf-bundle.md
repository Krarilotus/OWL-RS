# NRESE's own RDF bundle (`crates/rdf/`)

The RDF model, every RDF syntax, SPARQL syntax and results, XSD values and JSON, written
for NRESE in October 2026 to replace the Oxigraph project's libraries (`oxrdf`, `oxiri`,
`oxsdatatypes`, `oxrdfio`/`oxttl`, `spargebra`, `spareval`, `sparesults`; ADR-0001 as
amended). The engine, executor and reasoner never used Oxigraph's store. The branch
`baseline/pre-oxigraph-migration` (`79983fe`) keeps the last state on Oxigraph's
libraries; the suite runs it as the system `nrese-oxigraph`, and
`benches/oxigraph-comparison` measures each crate against the library it replaced.

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
