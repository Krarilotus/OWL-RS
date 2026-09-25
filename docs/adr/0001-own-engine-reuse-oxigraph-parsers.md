# ADR-0001: Own storage engine; reuse Oxigraph's parsers and SPARQL evaluator

Status: accepted (2026-09-25)

## Context

NRESE v1 wrapped `oxigraph::store::Store`. That caused three problems:
- Every write cloned the whole store to validate it. That's O(dataset) per write: a one-triple insert took 2.9 s at 1 M triples.
- The reasoner could only see triples made of IRIs.
- Durability needed RocksDB, which needs libclang on Windows.

The target is QLever/GraphDB-class performance and semantics. That needs control over the storage layout, the commit protocol and join execution.

Oxigraph is also published as separate, well-tested crates:
- `oxrdf`: the term model
- `oxrdfio`/`oxttl`: parsers and serialisers
- `spargebra`: the SPARQL parser and algebra
- `sparesults`: result formats
- `spareval`: a SPARQL 1.1 evaluator over any `QueryableDataset`, with cancellation and dataset specifications

## Decision

- Replace `oxigraph::store::Store` with our own `nrese-engine`.
- Keep the standards layer from Oxigraph's crates: `oxrdf`, `oxrdfio`, `spargebra`, `sparesults`, `spareval`. Parsing RDF and SPARQL isn't where performance or differentiation is won, and these crates pass the W3C test suites.
- `nrese-sparql` implements `spareval::QueryableDataset` over engine snapshots. Native operators replace spareval operators one at a time, and spareval stays the correctness oracle in differential tests.

## Consequences

- Full SPARQL 1.1 query/update and the protocol dataset parameters are available immediately on the new engine.
- Performance work concentrates where it matters: indexes, the commit path, joins.
- We depend on the Oxigraph crate family's release cadence. Versions are pinned and upgraded deliberately.
- The `durable-storage` Cargo feature and the RocksDB dependency are removed.
