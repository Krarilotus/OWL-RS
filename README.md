# NRESE

NRESE is an [RDF](https://www.w3.org/TR/rdf11-primer/) database with
[OWL](https://www.w3.org/TR/owl2-primer/) reasoning, written in Rust. It runs as one server
with SPARQL endpoints, and it is a hobby project.

The point of it is that you can see what an answer rests on, which statements were asserted and
which were inferred from them, and whether something may be missing. With reasoning on, the
inferences are computed as part of committing your write, so the new statements and what the
rules derive from them become visible together, and when you delete a statement, the inferences
that no longer follow go with it. Asserted and inferred statements are kept apart, and a query
can read both or only one kind.

Every SPARQL answer over HTTP carries a status. It says `complete` when nothing is missing or
wrong, `sound-only` when rows may be missing, `unsound` when rows may be wrong or missing, and
`not-certified` when there is no assessment.

Under OWL 2 QL, RL and DL, NRESE checks each write for consistency and refuses it as a whole
when the check finds that it would make the data inconsistent. If you set the SHACL gate to
`enforce`, it also refuses a write that introduces a SHACL violation. By a write we mean a SPARQL
update, a Graph Store write or a transaction's commit. A bulk load and the ontology file loaded at
start-up both skip these checks. After a bulk load through the load command or the import
endpoint, the inferences are recomputed if reasoning is on, and the rules' consistency checks run
with them.

In the `owl2-dl` mode, NRESE classifies the ontology and proves query answers beyond what the
rules derive, with OWL 2 DL engines of its own, a hypertableau and a consequence-based one. For
anything the rules derived, you can ask why it holds and get its derivation from the asserted
statements, rule by rule.

What you are allowed to read decides what an answer can hold, and the server's limits on time
and memory can stop a query before it answers (a stopped query gives no partial answer).
[Limits and deviations](docs/limits.md) describes
those limits and what NRESE doesn't do. To try it, start with the [quick start](#quick-start)
below.

## Architecture overview

```mermaid
flowchart TB
  C["Clients<br/>your applications, the browser console"]
  subgraph SRV["One NRESE server"]
    E["Endpoints<br/>SPARQL, Graph Store, RDF4J, engine API"]
    Q["Query engine<br/>SPARQL, with text, vector and geo search"]
    W["Write pipeline<br/>runs the chosen checks, then commits or refuses"]
    R["Rule reasoner<br/>RDFS to OWL 2 RL, or your own rules"]
    V["SHACL validation<br/>shapes, on request or on each write"]
    D["DL engines<br/>OWL 2 DL consistency, classification, answers"]
    T["Storage engine<br/>asserted and inferred statements, named graphs"]
  end
  L[("Disk<br/>write-ahead log and checkpoints")]
  C -- "HTTP" --> E
  E -- "queries" --> Q
  E -- "writes" --> W
  E -- "validation requests" --> V
  W -- "derive and check" --> R
  W -- "validate the change" --> V
  W -- "check consistency (owl2-dl)" --> D
  Q -- "prove answers (owl2-dl)" --> D
  E -- "classification requests" --> D
  W -- "commit one revision" --> T
  Q -- "read a snapshot" --> T
  T -- "log each commit (on-disk mode)" --> L
```

Everything in the frame runs as one process.
[What a commit does](docs/what-it-covers.md#what-a-commit-does) follows a write through it.

## What it can do

| Area | Details |
|---|---|
| [Reasoning](docs/what-it-covers.md#reasoning) | None, or RDFS, RDFS-Plus, OWL-Horst, OWL 2 QL, RL or DL, and your own rules, alone or on top of one of those |
| [Inferred statements](docs/ops/http-api.md#queries) | Kept apart from the asserted ones, so a query reads both, only the asserted or only the inferred |
| [Consistency checks](docs/what-it-covers.md#what-a-commit-does) | On each write under OWL 2 QL, RL and DL (the OWL 2 DL engines' check can be switched off) |
| [Answer status](docs/what-it-covers.md#what-an-answer-promises) | On every SPARQL answer over HTTP, and under OWL 2 DL you can ask for an answer proven complete, or an error instead |
| [Explanations](docs/ops/http-api.md#engine-api-apiv1) | Why an inferred statement holds, as its derivation rule by rule |
| [Classification](docs/ops/http-api.md#classification) | The ontology's class hierarchy, and named individuals' named classes |
| [SHACL](docs/what-it-covers.md#shacl) | SHACL Core and SHACL-SPARQL, with inferred statements included by default, on request or on each write as a report or a refusal |
| [SPARQL and search](docs/what-it-covers.md#sparql) | SPARQL 1.1 query and update (with the deviations listed in Limits), full-text and vector search, and part of GeoSPARQL |
| [Named graphs](docs/ops/config-reference.md#store.default_graph) | Statements in named graphs, and a query's default graph is either the default graph or the union of all graphs |
| [Storage](docs/what-it-covers.md#storage-transactions-and-durability) | Its own storage engine, in memory or on disk, with backups, restores and read replicas |
| [Access control](docs/what-it-covers.md#access-control-and-operations) | Sign-in by tokens, OIDC, or a client certificate's subject passed on by a trusted proxy, and read and write rights per named graph |
| [Interfaces](docs/what-it-covers.md#interfaces) | SPARQL 1.1 and Graph Store protocols, the RDF4J REST protocol (without its `SNAPSHOT` and `SERIALIZABLE` isolation), an engine API with an OpenAPI description, a browser console in the Docker image, and several repositories per server |

## Quick start

To run it, you need:

- git
- Docker with BuildKit
- curl
- a free port 8080
- a POSIX shell (Linux, macOS, or Git Bash or WSL on Windows)

The image builds the server and its console, so you don't need Rust or Node.js. Its server runs
on an x86-64-v3 (amd64) or Neoverse N1 level (arm64) CPU, and for an older one you build with
`--build-arg NRESE_TARGET_CPU=portable`.

```bash
git clone https://github.com/Krarilotus/OWL-RS.git
cd OWL-RS
docker build -t nrese .
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-rl -e NRESE_SHACL_GATE=enforce nrese
until curl -sf http://localhost:8080/readyz; do sleep 1; done
```

On an AMD Ryzen 9 5950X, compiling the server in `docker build` took us under 6 minutes with 8
build jobs. A first build also downloads the base images and builds the console, so it takes
longer.

The last line waits until the server answers `{"status":"ready"}`. If that never happens,
`docker logs nrese` shows what the server printed ([startup errors](docs/ops/server-setup.md#startup-errors)).

The data stays on the volume `nrese-data`. The server listens on this machine only, has no
authentication on, reasons under OWL 2 RL and has the SHACL gate set to `enforce`, which the
[tutorial](docs/tutorial.md) uses. The browser console is at `http://localhost:8080/console`.

### A first answer: inference

Store that every cat is an animal, and that Tom is a cat. The response is empty:

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

- `ex:tom` is in the answer because the update's commit also stored the inferred statement
  `ex:tom a ex:Animal`.
- `sound-only` says rows may be missing. The reason here: the answers come from the OWL 2 RL
  rules, not from every answer OWL 2 DL entails
  ([what an answer promises](docs/what-it-covers.md#what-an-answer-promises)).

Ask again, for asserted statements only:

```bash
curl -i http://localhost:8080/dataset/query --data-urlencode 'query=
  PREFIX ex: <http://example.org/>
  SELECT ?x WHERE { ?x a ex:Animal }' --data infer=false
```

```text
HTTP/1.1 200 OK
content-type: application/sparql-results+json
nrese-completeness: complete

{"head":{"vars":["x"]},"results":{"bindings":[]}}
```

No animal was asserted, so there are no rows. No reasoning was involved, so the answer is
`complete`. In both outputs other headers are left out, and `…` marks shortened text.

The [tutorial](docs/tutorial.md) goes on from here: retraction, validation,
[your own data](docs/tutorial.md#your-own-data), a changed setting, and starting over.

Without Docker, from source with Rust: [getting started](docs/dev/getting-started.md#run-the-server).

## How it is built

The workspace is split into crates by what they own. Dependencies, dev-dependencies aside,
point down only: a crate may use a crate in a lower layer, or one placed below it in its own
layer, never one above. [ARCHITECTURE.md](docs/ARCHITECTURE.md) follows a write and a query from
request to answer, names for each concept its page and its code, and says at which layer
each kind of problem is fixed; the decisions behind it, with their reasons, are in
[adr/](docs/adr/README.md). Which crates can be used on their own, and what each one uses:
[crates](docs/ARCHITECTURE.md#24-dependencies-and-using-a-crate-on-its-own).

## Documentation

| To | Read |
|---|---|
| Go on from the quick start | [tutorial](docs/tutorial.md) |
| Know what it does, with each part's limits | [what it covers](docs/what-it-covers.md) |
| Know what changed since `main`, and what is slower | [release notes, 1.5 (prerelease)](docs/releases/v1.5.md) |
| Build, run and develop | [getting-started.md](docs/dev/getting-started.md), [CONTRIBUTING.md](CONTRIBUTING.md) |
| Deploy, load data, back up | [server-setup.md](docs/ops/server-setup.md) |
| Look up an endpoint or a setting | [http-api.md](docs/ops/http-api.md), [config-reference.md](docs/ops/config-reference.md) |
| Fix an error | [startup errors](docs/ops/server-setup.md#startup-errors), [status codes](docs/ops/http-api.md#status-codes) |
| Know what reasoning computes | [reasoning-semantics.md](docs/spec/reasoning-semantics.md) |
| Understand the design | [ARCHITECTURE.md](docs/ARCHITECTURE.md), [design/](docs/design/), the decisions in [adr/](docs/adr/) |
| Find any other document | [docs/README.md](docs/README.md) |

## State and limits

Which commit these pages describe: `git log -1` in your clone, or the commit GitHub shows
beside the file. A running server names its build at `/version`: `version`, the package version, and with reasoning on `reasoning_semantics`,
the ruleset with its semantics version and a fingerprint of what it derives
([reasoning-semantics.md](docs/spec/reasoning-semantics.md)).

Changes that affect stored data, answers or clients, with their dates and what to do:
[dated changes](docs/STATUS.md#dated-changes). What is in, what is left out and when to choose
another system, and what is planned: the [scope](docs/what-it-covers.md#scope). The
contributors' record of what is done and open, item by item: [docs/STATUS.md](docs/STATUS.md).

Platforms: the image for `linux/amd64` and `linux/arm64`; the merge checks also build and
test on Windows ([scripts/check.sh](scripts/check.sh)). Stopping, and what a crash or a kill
keeps: [stopping](docs/ops/server-setup.md#64-stopping),
[durability](docs/spec/02-storage-and-transactions.md#durability-on-disk-mode); backups and
restores: [server setup](docs/ops/server-setup.md#10-backup-and-recovery-setup),
[drills](docs/ops/backup-restore-drills.md).

## Thanks

Many of NRESE's ideas come from the research literature and from systems such as QLever,
Konclude, HermiT and RDFox. Its first prototype was built on Oxigraph. Thanks to their
authors.

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
