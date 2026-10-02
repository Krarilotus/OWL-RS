# Coverage-guided fuzzing of the parsers

The parsers of untrusted input (RDF in every format, SPARQL queries and updates, SPARQL results) as libFuzzer targets. The targets themselves are in [`crates/nrese-fuzz`](../crates/nrese-fuzz/src/lib.rs). Each one checks two things:

- no input makes the parser panic;
- what parses is written back and read back the same.

On stable Rust they run from corpora and mutations as a test (`cargo test -p nrese-fuzz`, and in `scripts/fuzz-campaign.sh`). Here they run guided by coverage, on nightly.

```sh
cargo install cargo-fuzz
cargo +nightly fuzz list
# Seeds: the W3C suites (scripts/fetch-w3c-tests.sh), read from .cache besides the target's own corpus.
cargo +nightly fuzz run turtle fuzz/corpus/turtle ../.cache/rdf-tests/rdf -- -max_total_time=3600
```

| Target | Parser |
|---|---|
| `ntriples`, `nquads`, `turtle`, `trig`, `n3`, `rdfxml`, `jsonld` | `nrese_rdf_io::RdfParser` |
| `sparql_query`, `sparql_update` | `nrese_sparql_syntax::SparqlParser` |
| `results_json`, `results_xml`, `results_tsv` | `nrese_sparql_results::QueryResultsParser` (what a `SERVICE` answers) |

A crash leaves its input in `fuzz/artifacts/<target>/`. Each one becomes a regression test before it is fixed. `corpus/`, `artifacts/` and `target/` stay out of the repository.

libFuzzer needs Linux or macOS. On Windows, `cargo fuzz` builds with the MSVC AddressSanitizer, which may not be installed, so there the stable test is the way.
