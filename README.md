# NRESE

NRESE is an RDF database with OWL reasoning, written in Rust. Its inferences are committed with
the write they follow from and kept apart from what you stored, and every query answer carries a
status that says whether rows may be missing.

Most work with data comes down to organising and standardising it, and an ontology is where
that is written down. We wanted a store that follows the ontology itself, so that a write is
checked against it, an inference changes with the data, and an answer says how far it can be
trusted. NRESE is a hobby project, built part by part from the research literature with the W3C
test suites as its floor, and where a standard could go further for a use case, we want to help
move it.

## What it does

- **Reasoning from RDFS to OWL 2 DL.** Off until you choose a mode: RDFS, OWL 2 QL, RL and DL, a
  few rule sets in between, or your own rules ([modes](docs/spec/reasoning-semantics.md#modes)).
- **Inferences are part of the commit.** A write and what follows from it become visible in the
  same revision, and a delete takes along the inferences that no longer follow. Bulk loads and
  imports are the exception: their inferences follow in the next revision
  ([what a commit does](docs/what-it-covers.md#what-a-commit-does)).
- **Asserted and inferred are kept apart.** A query reads both, or only one of them
  ([queries](docs/ops/http-api.md#queries)).
- **Writes that break the data are refused.** Under the OWL 2 modes, a SPARQL update, Graph Store
  write or transaction is refused as a whole when the consistency check finds it would make the
  data inconsistent, and with the SHACL gate set to `enforce`, when it violates a shape. The
  response gives the reason, as far as the writer may read the data behind it.
- **Every answer carries a status.** `complete`, `sound-only` (rows may be missing), `unsound`
  (rows may be missing or wrong) or `not-certified`, in a header on every query answer over HTTP
  ([what an answer promises](docs/what-it-covers.md#what-an-answer-promises)).
- **Ask why.** For anything the rules derived, you get its derivation from your statements, rule
  by rule, within a [size limit](docs/limits.md#reasoning) ([explanations](docs/ops/http-api.md#engine-api-apiv1)).
- **Inferences respect read rights.** A reader who may see only some named graphs can be shown
  just the inferences that follow from those graphs
  ([access control](docs/ops/http-api.md#graph-level-access-control)).
- **OWL 2 DL engines of its own.** Under `owl2-dl`, a hypertableau and a consequence-based
  reasoner check consistency, classify the ontology and prove query answers beyond the rules
  ([design](docs/design/owl2-dl.md)).

Around that, what a database needs:

- its own storage engine, in memory or on disk with a write-ahead log
- named graphs, with read and write rights per graph
- sign-in by token, OIDC, or a client certificate checked by your TLS proxy
- full-text, vector and geospatial search inside SPARQL
- backups with point-in-time restore, and read replicas
- several repositories per server

It follows the W3C standards for RDF, SPARQL, SHACL and OWL 2, and runs their test suites in its
checks. Everything it does, with the [standards and their tests](docs/what-it-covers.md#standards),
is in [features and standards](docs/what-it-covers.md), and where it stops is in
[limits](docs/limits.md).

## How it fits together

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/img/overview-dark.svg">
  <img src="docs/img/overview-light.svg" width="720" alt="Your application tells NRESE something with SPARQL Update or the Graph Store protocol, and asks with a SPARQL query. A write goes through one commit, where the reasoner infers and checks consistency, SHACL validates when its gate is on, and under owl2-dl the OWL 2 DL engines check it too; if a check fails, the write is refused with the reason. Storage keeps asserted and inferred statements apart, one revision per commit, logged to the data directory. A query runs on a snapshot, the DL engines prove answers under owl2-dl, and the answer comes back with its status.">
</picture>

## Quick start

You need git, Docker with BuildKit, curl, a POSIX shell (Linux, macOS, or Git Bash or WSL on
Windows) and a free port 8080. The image builds the server and its browser console, so you need
no Rust or Node.js, and the first build takes several minutes. These pages describe the branch
`refactor/engine-v2`, where version 1.5 is prepared, and `main` still holds the earlier version.

```bash
git clone --branch refactor/engine-v2 https://github.com/Krarilotus/OWL-RS.git
cd OWL-RS
docker build -t nrese .
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-rl -e NRESE_SHACL_GATE=enforce nrese
until curl -sf http://localhost:8080/readyz; do sleep 1; done
```

The server now listens on this machine only, without sign-in, reasons under OWL 2 RL, refuses
writes that violate a SHACL shape, and keeps its data on the volume `nrese-data`. The console is at
<http://localhost:8080/console>. If `docker run` itself fails, its name or port is taken; if the
last line never returns, `docker logs nrese` shows why
([startup errors](docs/ops/server-setup.md#startup-errors)). The image targets x86-64-v3 and
Neoverse N1 CPUs, and on an older CPU you build with `--build-arg NRESE_TARGET_CPU=portable`.

### A first answer

Store that every cat is an animal, and that Tom is a cat:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
  INSERT DATA { ex:Cat rdfs:subClassOf ex:Animal . ex:tom a ex:Cat }'
```

Ask for every animal:

```bash
curl -i http://localhost:8080/dataset/query --data-urlencode 'query=
  PREFIX ex: <http://example.org/>
  SELECT ?x WHERE { ?x a ex:Animal }'
```

```text
HTTP/1.1 200 OK
content-type: application/sparql-results+json
nrese-completeness: sound-only; regime=owl2-rl; reasons="rules: answers over the owl2-rl closure: …"

{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"uri","value":"http://example.org/tom"}}]}}
```

Tom is an animal because the update's commit also stored the inferred statement
`ex:tom a ex:Animal`. The status is `sound-only`: every row is right, but the answer
comes from the OWL 2 RL rules, which may miss answers that full OWL 2 DL would give. Ask again with `--data infer=false` and you
read only what you stored: no rows, and the status is `complete`.

The [tutorial](docs/tutorial.md) goes on from here with a delete, a refused write, your own data
and a changed setting. To build from source instead, see
[getting started](docs/dev/getting-started.md#run-the-server).

## Documentation

| To | Read |
|---|---|
| Learn it step by step | the [tutorial](docs/tutorial.md) |
| See all it does, and the standards it follows | [features and standards](docs/what-it-covers.md) |
| Know where it stops, and what to do instead | [limits](docs/limits.md) |
| Know what reasoning computes, and what an answer promises | [reasoning](docs/spec/reasoning-semantics.md) |
| Load your own data | [your own data](docs/tutorial.md#your-own-data), and large files [offline](docs/ops/server-setup.md#loading-data) |
| Call it over HTTP | [the HTTP interface](docs/ops/http-api.md) |
| Change a setting | [every setting](docs/ops/config-reference.md) |
| Run it for others | [running a server](docs/ops/server-setup.md), [backups](docs/ops/backups.md), [read replicas](docs/ops/replication.md) |
| Understand an error | [status codes](docs/ops/http-api.md#status-codes), [startup errors](docs/ops/server-setup.md#startup-errors) |
| Look up a word | the [glossary](docs/glossary.md) |
| Build it from source, or use a crate on its own | [getting started](docs/dev/getting-started.md), [the crates](docs/ARCHITECTURE.md#24-dependencies-and-using-a-crate-on-its-own) |
| See what changed | the [release notes](docs/releases/v1.5.md) |
| Work on NRESE | [contributing](CONTRIBUTING.md), the [architecture](docs/ARCHITECTURE.md) and the [contributors' map](docs/README.md) |

## Licence

Copyright (C) 2026 Krarilotus. Licensed under either of the
[Apache License, Version 2.0](LICENSE-APACHE) or the [MIT licence](LICENSE-MIT), at your
option. The v1 prototype published before October 2026 (`main` up to `8088e9a`) carried
`Apache-2.0` in its package metadata; that statement stands for those commits.
Third-party material keeps its own licence: the dependencies (MIT, Apache-2.0, BSD and
similar; the accepted licences are in [deny.toml](deny.toml)), the vendored vocabularies
(sources and terms in [their README](benches/nrese-bench-harness/fixtures/catalog-cache/README.md)),
the rule file under `benches/reasoning/nemo/sparq/` ([its licence](benches/reasoning/nemo/sparq/LICENSE)),
and the W3C test suites, which are fetched and not part of this repository.
Contributions come in under the same terms: [CONTRIBUTING.md](CONTRIBUTING.md).
