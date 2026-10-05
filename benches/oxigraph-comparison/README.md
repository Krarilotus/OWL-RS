# NRESE's RDF bundle against Oxigraph

The migration away from the Oxigraph libraries (`docs/design/rdf-bundle.md`)
requires each replacement to be at least as good as what it replaces: standards coverage,
speed, and fewer bugs. This crate is where that is measured. It is a workspace of its own,
so the NRESE workspace never depends on an Oxigraph crate.

- `cargo test --release` runs the differential tests: the same inputs, from corpora and
  from seeded generators, through both implementations. Every difference is a bug in one
  of them, or a documented deviation (listed in each test with its reason).
- `cargo run --release --bin bench` times both implementations on the same inputs and
  prints a table (median of repeated runs). Following the project's rule, benchmarks run
  together at the end of a migration step.

Oxigraph is MIT/Apache-2.0 licensed open source; its numbers may be published.
