# NRESE

An [RDF](https://www.w3.org/TR/rdf11-primer/) store with built-in
[OWL](https://www.w3.org/TR/owl2-primer/) reasoning, written in Rust. A hobby project, developed in
the open; not yet for parallel writers, failover or data split across machines
([limits, and when to choose another system](docs/limits.md)).

NRESE stores RDF datasets (statements: subject,
predicate, object) and answers [SPARQL 1.1](https://www.w3.org/TR/sparql11-query/) queries
and updates. Given an [ontology](docs/glossary.md#ontology), it derives the statements that
follow from the stored (asserted) ones and keeps these inferred statements up to date on
every commit (a write, applied whole or not at all), so you query them like asserted ones.
Reasoning goes from [RDFS](https://www.w3.org/TR/rdf11-schema/) to
OWL 2 DL; in the OWL 2 QL, RL and DL
[profiles](docs/glossary.md#reasoning-mode-profile) a commit that would make the data
inconsistent is rejected, with the reason. Every SPARQL answer carries a status that says what it promises
(complete, or sound with rows possibly missing, and why) and warns where it can promise
neither. [SHACL](https://www.w3.org/TR/shacl/) validation, full-text and vector search,
graph-level access control and the SPARQL, Graph Store and RDF4J protocols are built in.
It runs as one server binary with a browser console, or as a set of Rust crates.

It may suit you if you:

- query data together with its ontology and want the inferences to follow every change;
- need to know when an answer under OWL reasoning may be missing rows;
- want a store you can read and take apart: small crates in clear layers.

To try it: [Quick start](#quick-start), below. What it does today:
[What it covers](docs/what-it-covers.md); what it leaves out, and when to
choose another system: [limits](docs/limits.md); what is planned:
[ROADMAP](docs/ROADMAP.md); how it works: [How it is built](#how-it-is-built).

## Quick start

You need:

- git ([install](https://git-scm.com/downloads));
- Docker Engine 23 or later ([install](https://docs.docker.com/engine/install/)) or Docker
  Desktop ([install](https://docs.docker.com/desktop/)): the build needs BuildKit, their
  default builder;
- curl 7.71 or later (`curl --version`; [download](https://curl.se/download.html));
- for the build: an amd64 CPU of level [x86-64-v3](https://en.wikipedia.org/wiki/X86-64#Microarchitecture_levels)
  (AVX2 and FMA: Intel from Haswell, 2013; AMD from Excavator, 2015; on Linux,
  `/lib64/ld-linux-x86-64.so.2 --help | grep x86-64-v3` says `supported`), or an arm64 CPU
  from Neoverse N1 on (AWS Graviton2, Ampere Altra); for an older one, add
  `--build-arg NRESE_TARGET_CPU=portable` to `docker build`; and about 3 GB of free
  memory and 3 GB of free disk, besides the base images Docker downloads;
- port 8080 free on this machine.

Rust and Node.js aren't needed: the image builds the server and its console. The commands
below fetch the code, build the [image](docs/glossary.md#image-container-volume) `nrese`
and start it as the container `nrese`:

- its data on disk (the image sets `NRESE_STORE_MODE=on-disk`; its other settings:
  [container](docs/ops/server-setup.md#72-example-docker-run)), on the volume `nrese-data`,
  so it outlives the container;
- its port bound to this machine only, since no authentication is on (set
  `NRESE_AUTH_MODE` before you expose it: [access control](docs/what-it-covers.md#access-control-and-operations));
- reasoning under OWL 2 RL, and the [SHACL commit gate](docs/glossary.md#shape-shapes-graph-shacl-gate)
  on, which the first answer below and the [tutorial](docs/tutorial.md) use.

Other words these pages use: [glossary](docs/glossary.md).

```bash
git clone https://github.com/Krarilotus/OWL-RS.git
cd OWL-RS
docker build -t nrese .
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-rl -e NRESE_SHACL_GATE=enforce nrese
curl -sSf --max-time 5 --retry 60 --retry-delay 1 --retry-max-time 60 --retry-all-errors http://localhost:8080/readyz
```

- The build compiles the server: about 5 minutes on 16 cores, 7 on 12, longer on fewer.
- The `curl` line waits for the server and prints `{"status":"ready"}`. It tries every
  second, each try at most 5 s, and gives up with an error after 60 s. A `curl:` error
  line before it is a try made before the server was up. A large store takes longer to
  start: raise `--retry` and `--retry-max-time` to match.
- If it never gets ready, `docker logs nrese` shows why ([startup errors](docs/ops/server-setup.md#startup-errors)).
  If `docker run` says the port is already allocated, something else holds port 8080.

The browser console is at `http://localhost:8080/console`.

### A first answer: inference

Add a [class hierarchy and an instance](docs/glossary.md#class-instance-subclass), then ask
for the instances of the superclass:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
  INSERT DATA { ex:Cat rdfs:subClassOf ex:Animal . ex:tom a ex:Cat }'

curl -i http://localhost:8080/dataset/query --data-urlencode 'query=
  PREFIX ex: <http://example.org/>
  SELECT ?x WHERE { ?x a ex:Animal }'
```

The answer: the [HTTP](docs/glossary.md#http-request-response-status) status and headers,
then the rows as [JSON](docs/glossary.md#json):

```text
HTTP/1.1 200 OK
content-type: application/sparql-results+json
nrese-completeness: sound-only; regime=owl2-rl; reasons="rules: answers over the owl2-rl closure: …"

{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"uri","value":"http://example.org/tom"}}]}}
```

Other headers are left out here, and `…` marks shortened text: compare the status, the
`regime` and the rows.

The answer has `ex:tom`, because `ex:tom a ex:Animal` was inferred. The
`NRESE-Completeness` header says `sound-only`: every row is entailed, and the rules are
complete only for ontologies inside the OWL 2 RL profile, which isn't checked per query
([what an answer promises](docs/what-it-covers.md#what-an-answer-promises)).

The [tutorial](docs/tutorial.md) goes on from here: retraction, validation, your own data, a changed
setting, and starting over.

Without Docker, from source with Rust: [getting started](docs/dev/getting-started.md#run-the-server).

## How it is built

The workspace is split into crates by what they own. Dependencies point down only: a
crate may use a crate in a lower layer, or one placed below it in its own layer, never
one above. Each crate's `lib.rs` opens with what
it owns. [ARCHITECTURE.md](docs/ARCHITECTURE.md) follows a write and a query from
request to answer, names for each concept its page and its code, and says at which layer
each kind of problem is fixed; the decisions behind it, with their reasons, are in
[adr/](docs/adr/README.md). Which crates can be used on their own, and what each one uses:
[crates](docs/ARCHITECTURE.md#24-dependencies-and-using-a-crate-on-its-own).

## Documentation

| To | Read |
|---|---|
| Go on from the quick start | [tutorial](docs/tutorial.md) |
| Know what it does, with each part's limits | [what it covers](docs/what-it-covers.md) |
| Build, run and develop | [getting-started.md](docs/dev/getting-started.md), [CONTRIBUTING.md](CONTRIBUTING.md) |
| Deploy, load data, back up | [server-setup.md](docs/ops/server-setup.md) |
| Look up an endpoint or a setting | [http-api.md](docs/ops/http-api.md), [config-reference.md](docs/ops/config-reference.md) |
| Know what reasoning computes | [reasoning-semantics.md](docs/spec/reasoning-semantics.md) |
| Understand the design | [ARCHITECTURE.md](docs/ARCHITECTURE.md), [design/](docs/design/), the decisions in [adr/](docs/adr/) |
| Find any other document | [docs/README.md](docs/README.md) |

## State and limits

These pages describe the code of the commit they are in, package version 0.1.0, which is
not a release. Which commit that is: `git log -1` in your clone, or the commit GitHub shows
beside the file. A running server names its build at `/version`: `version`, the package version, and with reasoning on `reasoning_semantics`,
the ruleset with its semantics version and a fingerprint of what it derives
([reasoning-semantics.md](docs/spec/reasoning-semantics.md)).

Changes that affect stored data, answers or clients, with their dates and what to do:
[dated changes](docs/STATUS.md#dated-changes). What is done and what is open, item by item:
[docs/STATUS.md](docs/STATUS.md); the order of work: [docs/ROADMAP.md](docs/ROADMAP.md); the main
limits and deviations, and when to choose another system: [docs/limits.md](docs/limits.md).

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
