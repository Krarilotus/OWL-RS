# NRESE

An RDF triple store with built-in OWL reasoning, written in Rust. Its own storage engine
answers SPARQL; the reasoning is materialised and kept up to date on every commit, so
inferred statements are queried like asserted ones. SHACL validation, full-text search and
the RDF4J protocol are built in.

The aim: query speed like [QLever](https://github.com/ad-freiburg/qlever), reasoning like
the dedicated OWL reasoners, and data governance like GraphDB, in one server.

## State

**`main`** (this branch) is the stable version:

- SPARQL 1.1 query and update, the Graph Store Protocol, the RDF4J protocol;
- reasoning in the `rdfs`, `owl2-rl`, `owl2-ql`, `owl-horst` and `custom` (Notation3
  rules) modes. Commits that make the data inconsistent are rejected, with an explanation.
  What each mode computes and omits: [reasoning-semantics.md](docs/spec/reasoning-semantics.md);
- SHACL Core and SHACL-SPARQL (W3C suites 98 of 98 and 22 of 22), on request or as a
  commit gate;
- full-text search, users, workspaces and graph policies, an operator UI and a console.

Measured over HTTP at 10 M triples ([baselines](benches/baselines/README.md)): a one-triple
insert takes 0.2 ms in memory and 2.0 ms on disk with fsync, and loading runs at about
400,000 triples/s.

**[`refactor/engine-v2`](https://github.com/Krarilotus/OWL-RS/tree/refactor/engine-v2)** is
where development happens, merged into `main` when it is complete and benchmarked:

- OWL 2 DL: a hypertableau, consequence-based classification, consistency on commit, and
  query answers as lower and upper bounds with a completeness status;
- OWL 2 QL query rewriting; a faster rule engine and query executor;
- comparisons with QLever, Oxigraph, Nemo, Konclude, HermiT, Openllet and ELK, each on its
  own strengths.

What is done and what is open: [docs/STATUS.md](docs/STATUS.md) on that branch.

## Quick start

With Docker:

```bash
docker build -t nrese .
docker run -p 8080:8080 -v nrese-data:/var/lib/nrese/data -e NRESE_REASONING_MODE=owl2-rl nrese
```

From source, with a Rust toolchain:

```bash
cargo run --release -p nrese-server
```

Then open `http://localhost:8080/console`, or send SPARQL to `/dataset/sparql` and data to
`/dataset/data`. The server has no authentication switched on by default; set
`NRESE_AUTH_MODE` before exposing it.

## Documentation

- [Building, running and developing](docs/dev/getting-started.md): the console, the CLI,
  where to start in the code
- [HTTP interface](docs/ops/http-api.md) and [configuration](docs/ops/config-reference.md)
- [Architecture](docs/ARCHITECTURE.md), [roadmap](docs/ROADMAP.md),
  [decisions](docs/adr/), [document index](docs/README.md)
- Integrations: [ResearchSpace](docs/integration/researchspace.md),
  [the Datamodel Workflow](docs/integration/datamodel-workflow.md)

## Licence

Copyright (C) 2026 Krarilotus. Licensed under either of the
[Apache License, Version 2.0](LICENSE-APACHE) or the [MIT licence](LICENSE-MIT), at your
option. The v1 prototype published before October 2026 (`main` up to `8088e9a`) carried
`Apache-2.0` in its package metadata; that statement stands for those commits.
Contributions come in under the same terms: [CONTRIBUTING.md](CONTRIBUTING.md).
