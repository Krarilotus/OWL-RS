# NRESE

NRESE is an RDF database with OWL reasoning, written in Rust as a hobby project. Most work with
data comes down to organising and standardising it, and an ontology is where that is written
down, so NRESE is built to follow the ontology itself. Its inferences are committed with the
write they follow from and kept apart from what you stored, and every query answer carries a
status that says whether rows may be missing.

- You get one server, built from source or as a Docker image: SPARQL 1.1, the Graph Store
  Protocol and RDF4J's protocol over HTTP, and a browser console.
- It takes one writer at a time, and the data must fit one machine: read replicas follow it,
  but nothing fails over or partitions the data ([limits](docs/limits.md)).
- These pages describe version 1.5, not yet released, which the [quick start](#quick-start)
  builds from the branch `refactor/engine-v2`; `main` holds the earlier version.

## What it does

- **Reasoning from RDFS to OWL 2 DL.** Off until you choose a mode: RDFS, OWL 2 QL, RL and DL, a
  few rule sets in between, or your own rules ([modes](docs/spec/reasoning-semantics.md#modes)).
- **Inferences are part of the commit.** A write and what follows from it become visible in the
  same revision, and a delete takes along the inferences that no longer follow. Bulk loads and
  imports are the exception: their inferences follow in the next revision
  ([what a commit does](docs/what-it-covers.md#what-a-commit-does)).
- **Asserted and inferred are kept apart.** A query reads both, or only one of them
  ([queries](docs/ops/http-api.md#queries)).
- **Writes that break the data are refused.** With reasoning on, a SPARQL update, Graph Store
  write or transaction is refused as a whole when the reasoner finds it would make the data
  inconsistent, and with the SHACL gate set to `enforce`, when it introduces a violation. The
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

We wanted a store in which a write is checked against the ontology, an inference changes with
the data, and an answer says how far it can be trusted. NRESE is built part by part from the
research literature, with the W3C test suites as its floor, and where a standard could go
further for a use case, we want to help move it.

## Quick start

You need git, Docker with BuildKit, curl, a POSIX shell (Linux, macOS, or Git Bash or WSL on
Windows) and a free port 8080. The image builds the server and its browser console, so you need
no Rust or Node.js, and the first build takes several minutes. The image targets x86-64-v3 and
Neoverse N1 CPUs; on an older CPU, add `--build-arg NRESE_TARGET_CPU=portable` to `docker build`.

```bash
git clone --branch refactor/engine-v2 https://github.com/Krarilotus/OWL-RS.git
cd OWL-RS
docker build -t nrese .
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-dl -e NRESE_SHACL_GATE=enforce nrese
until curl -sf http://localhost:8080/readyz; do sleep 1; done
```

The server now listens on this machine only, without sign-in, reasons under OWL 2 DL, refuses
writes that violate a SHACL shape, and keeps its data on the volume `nrese-data`. The console is at
<http://localhost:8080/console>. If `docker run` itself fails, Docker says why (often the
name or the port is taken); if the last line never returns, `docker logs nrese` shows why
([startup errors](docs/ops/server-setup.md#startup-errors)).

### A first answer

Store that cats and dogs are animals, that every pet is a cat or a dog, that Tom is a cat and
Rex a pet:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
  PREFIX owl: <http://www.w3.org/2002/07/owl#>
  INSERT DATA {
    ex:Cat rdfs:subClassOf ex:Animal . ex:Dog rdfs:subClassOf ex:Animal .
    ex:Pet rdfs:subClassOf [ owl:unionOf (ex:Cat ex:Dog) ] .
    ex:tom a ex:Cat . ex:rex a ex:Pet }'
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
nrese-completeness: complete; regime=owl2-dl

{"head":{"vars":["x"]},"results":{"bindings":[
  {"x":{"type":"uri","value":"http://example.org/tom"}},
  {"x":{"type":"uri","value":"http://example.org/rex"}}]}}
```

Four more headers are left out here, and the body, which comes as one line, is broken to fit the
page.

Tom is an animal by a rule: the update's commit also stored `ex:tom a ex:Animal`. Rex is one
only by cases, as a pet is a cat or a dog and either is an animal. No rule draws that, but the
OWL 2 DL engines prove from the ontology that every pet is an animal, so Rex is in the answer,
and its `nrese-completeness` header says the answer is `complete`: no row is missing. The rows
may come in either order. Ask again with `--data infer=false` and you read only what you stored: no rows.

The [tutorial](docs/tutorial.md) goes on from here with a delete, a refused write, your own data
and a changed setting. To build from source instead, see
[getting started](docs/dev/getting-started.md#run-the-server).

## How it fits together

<p align="center">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/img/overview-dark.svg">
  <img src="docs/img/overview-light.svg" width="480" alt="You tell NRESE statements with SPARQL Update, the Graph Store Protocol, RDF4J's protocol or an RDF document, over HTTP; a write is all of it or nothing, and a refusal says why. You ask it with SPARQL queries and, with reasoning on, why a statement holds; each SPARQL answer says if rows may be missing or wrong. A bulk load skips the checks below. Reasoning is off by default (disabled, rdfs, owl2-rl, owl2-dl and more): when on, it infers with every write and refuses a write found to make the data inconsistent. SHACL validation is off by default: report logs the results a write introduces, and enforce refuses a write that introduces a violation. Storage keeps what you told it (asserted) apart from what reasoning derived (inferred). It is in memory by default, which is lost when the server stops; on-disk saves each commit before confirming it, and the container image uses it.">
</picture>
</p>

Reasoning and the SHACL gate are off by default, as the picture says; the quick start turns them
on with `NRESE_REASONING_MODE=owl2-dl` and `NRESE_SHACL_GATE=enforce`.

## Documentation

| To | Read |
|---|---|
| Learn it step by step | the [tutorial](docs/tutorial.md) |
| See all it does, and the standards it follows | [features and standards](docs/what-it-covers.md) |
| Know where it stops, and what to do instead | [limits](docs/limits.md) |
| Choose how it reasons, and know what an answer promises | [reasoning modes](docs/what-it-covers.md#reasoning), [what an answer promises](docs/what-it-covers.md#what-an-answer-promises) |
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
