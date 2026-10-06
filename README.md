# NRESE

A hobby project: an RDF triple store with built-in OWL reasoning, written in Rust, to
learn and experiment with. It answers SPARQL, keeps inferred statements up to date on
every commit so they can be queried like asserted ones, and validates data with SHACL.

Many of its ideas come from the research literature and from systems such as QLever,
Konclude, HermiT and RDFox. Thanks to their authors.

## State

**`main`** (this branch):

- SPARQL 1.1 query and update, the Graph Store Protocol, the RDF4J protocol;
- reasoning in the `rdfs`, `owl2-rl`, `owl2-ql`, `owl-horst` and `custom` (Notation3
  rules) modes. What each mode computes and omits: [reasoning-semantics.md](docs/spec/reasoning-semantics.md);
- SHACL Core and SHACL-SPARQL, full-text search, and a small console.

Work in progress, OWL 2 DL among it, happens on
[`refactor/engine-v2`](https://github.com/Krarilotus/OWL-RS/tree/refactor/engine-v2).

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

- [Building, running and developing](docs/dev/getting-started.md)
- [HTTP interface](docs/ops/http-api.md) and [configuration](docs/ops/config-reference.md)
- [Architecture](docs/ARCHITECTURE.md) and the [document index](docs/README.md)

## Licence

Copyright (C) 2026 Krarilotus. Licensed under either of the
[Apache License, Version 2.0](LICENSE-APACHE) or the [MIT licence](LICENSE-MIT), at your
option. The v1 prototype published before October 2026 (`main` up to `8088e9a`) carried
`Apache-2.0` in its package metadata; that statement stands for those commits.
Third-party material keeps its own licence: the dependencies (MIT, Apache-2.0, BSD and
similar), the vendored vocabularies under `benches/nrese-bench-harness/fixtures/catalog-cache/`,
and the W3C test suites, which are fetched and not part of this repository.
Contributions come in under the same terms: [CONTRIBUTING.md](CONTRIBUTING.md).
