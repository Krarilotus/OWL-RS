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

## Steps

Each step ends green (fmt, clippy, all tests with the W3C suites required) and committed.

| # | Step | Done when |
|---|---|---|
| 1 | **spareval out of the runtime.** Own `QueryResults`, solution and triple iterators, `QueryEvaluationError`, `CancellationToken`, `QueryDatasetSpecification`. The native executor takes every query: `BNODE(label)` per solution, unknown functions as errors, `SERVICE ?endpoint`, EXISTS by substitution where correlation isn't safe, paths under `GRAPH ?g`; queries over a transaction run on its pending snapshot. The reference evaluator replaces spareval as the differential tests' oracle. | `spareval` gone from every `Cargo.toml`; the differential and W3C tests pass |
| 2 | **`nrese-rdf`, `nrese-xsd`.** | Own tests; the vendored `oxsdatatypes` is gone |
| 3 | **`nrese-rdf-io`.** Readers and writers; N-Triples and N-Quads chunked and parallel, handing terms to the engine without intermediate strings where it can. | The W3C N-Triples, N-Quads, Turtle, TriG and RDF/XML test suites pass; bulk load at least as fast as before |
| 4 | **`nrese-sparql-syntax`, `nrese-sparql-results`.** | The W3C SPARQL 1.1 syntax tests (positive and negative) pass; results round-trip |
| 5 | **Switch-over.** Every crate on the bundle; the `oxigraph` oracle in the W3C runner and the bench harness replaced. | `cargo tree` lists no crate of the Oxigraph project; all tests pass |

## Status

Updated as the steps land.
