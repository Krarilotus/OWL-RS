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
[What it covers](#what-it-covers); what it leaves out, and when to
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

- its data on the volume `nrese-data`;
- its port bound to this machine only, since no authentication is on (set
  `NRESE_AUTH_MODE` before you expose it: [access control](#access-control-and-operations));
- reasoning under OWL 2 RL, and the [SHACL commit gate](docs/glossary.md#shape-shapes-graph-shacl-gate)
  on, which the example below uses.

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

### A first example: inference, retraction, validation

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

The answer has `ex:tom`, because `ex:tom a ex:Animal` was inferred. The
`NRESE-Completeness` header says `sound-only`: every row is entailed, and the rules are
complete only for ontologies inside the OWL 2 RL profile, which isn't checked per query
([what an answer promises](#what-an-answer-promises)).

Delete the premise, and the inference goes with it, in the same commit:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  DELETE DATA { ex:tom a ex:Cat }'
```

The same query now answers no rows: `{"head":{"vars":["x"]},"results":{"bindings":[]}}`.

Now a [shape, stored in the shapes graph](docs/glossary.md#shape-shapes-graph-shacl-gate): every animal needs a name. A cat without one is
an animal by inference, so the gate rejects it:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  PREFIX sh: <http://www.w3.org/ns/shacl#>
  INSERT DATA { GRAPH <http://rdf4j.org/schema/rdf4j#SHACLShapeGraph> {
    ex:AnimalShape a sh:NodeShape ; sh:targetClass ex:Animal ;
      sh:property [ sh:path ex:name ; sh:minCount 1 ] } }'

curl -i http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  INSERT DATA { ex:felix a ex:Cat }'
```

```text
HTTP/1.1 400 Bad Request
content-type: application/problem+json

{"detail":"the commit introduces 1 SHACL result(s) and 0 failure(s); first: focus node <http://example.org/felix>, <http://www.w3.org/ns/shacl#MinCountConstraintComponent>","reasoner_reject":null,"request_id":"…","status":400,"title":"SHACL Reject","type":"https://nrese.dev/problems/shacl-reject"}
```

Nothing of the rejected commit remains: the query still answers no rows. With a name
(`INSERT DATA { ex:felix a ex:Cat ; ex:name "Felix" }`) the commit passes, and the query
answers `ex:felix`. When each part of a commit becomes visible, and what the checks see:
[What a commit does](#what-a-commit-does). The shape stays in the store: to run the example
again, [start over](#change-a-setting-start-over) first.

Optional: this example is also a test.
[readme-first-example.toml](scripts/verify/scenarios/readme-first-example.toml) sends each step
above and checks its answer. To run it you also need [Python](https://www.python.org/downloads/) 3.11
or later; start a fresh server as above, then:
`python3 scripts/verify/verify.py run --url http://127.0.0.1:8080 scripts/verify/scenarios/readme-first-example.toml`.
Last run: 13 November 2026, on commit `78b4a49b7`, all 9 steps passed; what each step sends,
expects and got: [the record](docs/dev/readme-example-record.md).

### Your own data

Your own file, `my.ttl` in [Turtle](https://www.w3.org/TR/turtle/), into the same
server, counted, and still there after a restart:

```bash
curl -X PUT -H 'Content-Type: text/turtle' --data-binary @my.ttl 'http://localhost:8080/dataset/data?default'
curl http://localhost:8080/dataset/query --data-urlencode infer=false \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'
docker restart nrese
curl -sSf --max-time 5 --retry 60 --retry-delay 1 --retry-max-time 60 --retry-all-errors http://localhost:8080/readyz
curl http://localhost:8080/dataset/query --data-urlencode infer=false \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'
```

- The `PUT` answers `204 No Content` and replaces the [default graph](docs/glossary.md#graph) with your file.
- `infer=false` reads the asserted statements only, so the count is your file's number of
  statements (for a file of three: `"value":"3"`). Without it, the inferred ones count too;
  under `owl2-rl` they include 32 that every store holds, an empty one too: each datatype the
  profile supports is an `rdfs:Datatype` ([why](docs/spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).
- After the restart the count is the same. On Docker's stop signal the server lets the
  requests in flight finish, closes the store and exits
  ([stopping](docs/ops/server-setup.md#64-stopping)). Every commit is synced to the log
  before it is acknowledged, so not even a kill loses one
  ([durability](docs/spec/02-storage-and-transactions.md#durability-on-disk-mode)).
- Other syntaxes take their media type as `Content-Type`. Large files load offline with
  `nrese-server load` ([server setup](docs/ops/server-setup.md)).
- Queries are standard SPARQL ([SPARQL 1.1 Query Language](https://www.w3.org/TR/sparql11-query/#basicpatterns)).
  The names of the animals older than 5, in order, sent as `query=` like the count:
  `PREFIX ex: <http://example.org/> SELECT ?name WHERE { ?a a ex:Animal ; ex:age ?age ; ex:name ?name . FILTER(?age > 5) } ORDER BY ?name`.

### Change a setting, start over

Settings come from environment variables (or a file) when the server starts
([every setting](docs/ops/config-reference.md)). To change one, recreate the container
with the same volume: the data stays. For example, to cap the memory that running queries
and updates may take together at 2 GiB:

```bash
docker rm -f nrese
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-rl -e NRESE_SHACL_GATE=enforce \
  -e NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES=2GiB nrese
curl -sSf --max-time 5 --retry 60 --retry-delay 1 --retry-max-time 60 --retry-all-errors http://localhost:8080/readyz
curl http://localhost:8080/version
```

`/version` shows the settings in force, here `"total_query_memory":2147483648` under
`budgets`. Which memory limit bounds what: [budgets](docs/ops/config-reference.md#budgets).

A [repository](docs/glossary.md#repository)'s reasoning changes while the server runs, without a restart:

```bash
curl -X PATCH -H 'Content-Type: application/json' --data '{"reasoning":"rdfs"}' \
  http://localhost:8080/api/v1/repositories/nrese
```

`nrese` is the default repository. The inferences are recomputed at once. The change is
kept in the data directory and holds after a restart, over `NRESE_REASONING_MODE`;
`{"reasoning":null}` goes back to the configured mode
([repositories](docs/ops/http-api.md#engine-api-apiv1)).

To start over with no data: `docker rm -f nrese && docker volume rm nrese-data`, then
`docker run` as above.

### From source

With Rust 1.98 or later (`rust-version` in [Cargo.toml](Cargo.toml); the builds here use the
version [rust-toolchain.toml](rust-toolchain.toml) names):

```bash
cargo run --release -p nrese-server
```

With only Rust, you get the server and its API on `127.0.0.1:8080`, from memory (nothing
is written to disk) and with reasoning off; set `NRESE_STORE_MODE=on-disk` and
`NRESE_REASONING_MODE` to change that ([configuration](docs/ops/config-reference.md)).
With reasoning off, the first query of the example above answers no rows, as `complete`:
nothing is inferred. Start the server with `NRESE_REASONING_MODE=owl2-rl` and
`NRESE_SHACL_GATE=enforce` for the example. The operator page `/ops` is built in. The
browser console needs Node.js: build it in `apps/nrese-console` (`npm ci && npm run build`)
before the server, and the server embeds it; without it, `/console` says how
([getting started](docs/dev/getting-started.md)).

### From the command line

| Task | Needs a running server | How |
|---|---|---|
| Version and capabilities | yes | `curl http://localhost:8080/version`, `curl http://localhost:8080/api/v1/capabilities`; or the console CLI's `capabilities` |
| Check a configuration, print its JSON Schema | no | `nrese-server check-config --config FILE`, `nrese-server config-schema` |
| Query the server | yes | `curl` as above; or the console CLI's `query --text "…"`, which with `--require-complete` exits 3 when the answer isn't shown complete |
| Load files, or query the stored data, offline | no: not on a data directory a server is using | `nrese-server load FILE…`, `nrese-server query "SELECT …"` |

`nrese-server` is the image's entrypoint (`docker run --rm nrese config-schema`), or
`cargo run --release -p nrese-server -- …` from source; every command takes
`--set KEY=VALUE` ([configuration](docs/ops/config-reference.md#precedence),
[offline commands](docs/ops/server-setup.md#54-bulk-loading)). The console CLI is
`npm run cli -- …` in `apps/nrese-console` ([console CLI](docs/dev/getting-started.md#frontend-cli)).

## What it covers

What is in today. Each item links to the specification, document or test that backs it,
with its limits beside it; nothing here is a plan. What is left out on purpose, and every
deviation from a standard: [limits](docs/limits.md). What is planned:
[ROADMAP](docs/ROADMAP.md).

### SPARQL

| Capability | Limits and notes |
|---|---|
| [SPARQL 1.1](https://www.w3.org/TR/sparql11-overview/) Query, Update and federated `SERVICE` | The W3C suites run in the checks and in CI, with no known failure listed ([evaluation](crates/nrese-sparql/tests/w3c_sparql11), [syntax](crates/rdf/nrese-sparql-syntax/tests/w3c_syntax)). Deviations (`LOAD`, `SERVICE`, `HAVING` aliases, merged blank nodes), each with what to do: [limits](docs/limits.md#sparql) |
| SPARQL 1.2 and RDF 1.2 drafts: triple terms, directional strings | Results announce `version 1.2` when they may use them ([HTTP interface](docs/ops/http-api.md#sparql)) |
| [SPARQL 1.1 Protocol](https://www.w3.org/TR/sparql11-protocol/), [Graph Store Protocol](https://www.w3.org/TR/sparql11-http-rdf-update/), service description | [HTTP interface](docs/ops/http-api.md) |
| Results in JSON, XML, CSV and TSV; graphs in every RDF syntax below | Chosen by `Accept` ([formats](docs/ops/http-api.md#sparql)) |
| `EXPLAIN`: the plan with estimated rows, or the run with actual rows and times | A reader of only some graphs gets no plan steps (they would reveal statistics of the others) |
| Full-text search (BM25) through the `bds:` and Jena `text:query` vocabularies, with phrases, prefixes, fuzzy words and stemming in 18 languages | One index over every string literal, built at the first search; an on-disk store keeps it across restarts ([full-text](docs/ops/http-api.md#sparql)) |
| Vector search: vectors as literals (`"[…]"^^nrv:vector`), `SERVICE nrv:search` with cosine, dot product or Euclidean distance, exact or through HNSW, filtered by the rest of the query | You bring the vectors; the store computes no embeddings. Approximate search trades recall for speed through `nrv:searchBudget` ([vector search](docs/ops/http-api.md#sparql)) |
| [GeoSPARQL 1.1](https://docs.ogc.org/is/22-047r1/22-047r1.html), in part: functions and relations over WKT, GeoJSON and simple GML, with an R-tree | Measures in metres only in CRS84 and a listed set of projected systems; no GML arcs or 3D solids ([GeoSPARQL](docs/ops/http-api.md#sparql), [compliance runner](crates/nrese-sparql/tests/geosparql_compliance)) |

### RDF syntaxes

| Capability | Limits and notes |
|---|---|
| Read and write [N-Triples](https://www.w3.org/TR/n-triples/), [N-Quads](https://www.w3.org/TR/n-quads/), [Turtle](https://www.w3.org/TR/turtle/), [TriG](https://www.w3.org/TR/trig/), [RDF/XML](https://www.w3.org/TR/rdf-syntax-grammar/) | W3C RDF 1.1 and 1.2 suites, each document also read one byte at a time and written back ([runner](crates/rdf/nrese-rdf-io/tests/w3c_rdf)) |
| Read and write [JSON-LD 1.1](https://www.w3.org/TR/json-ld11/) | Remote contexts aren't fetched: a library caller supplies a document loader. W3C `toRdf`, `expand`, `fromRdf` suites; tests for JSON-LD 1.0 processors only and for generalised RDF are not run ([runner](crates/rdf/nrese-rdf-io/tests/w3c_jsonld)) |
| Read [Notation3](https://w3c.github.io/N3/spec/) data, in the library | Only what RDF can hold; rules are read by the reasoner. Not offered over HTTP. W3C N3 group's parser tests, one known failure listed ([runner](crates/rdf/nrese-rdf-io/tests/w3c_n3)) |
| RDF4J Binary RDF, versions 1 and 2 | Read in both versions, written as version 2; as a payload and a result format |
| [RDF Dataset Canonicalization](https://www.w3.org/TR/rdf-canon/) (RDFC-1.0) | W3C suite ([runner](crates/rdf/nrese-rdf-io/tests/w3c_rdfc)) |
| Bulk loading, N-Triples and N-Quads on all cores ([server setup](docs/ops/server-setup.md)); statements that don't parse are quarantined, not fatal | Strict mode stops at the first error instead ([quarantine](docs/design/quarantine.md)) |

### Reasoning

Inferences are materialised: computed when data is loaded, kept up to date by every commit
in the same revision, and stored apart from asserted statements. A query reads both by
default, or only the asserted ones with `infer=false`. The contract for every mode is
[reasoning-semantics.md](docs/spec/reasoning-semantics.md).

| Mode | What it computes | Limits |
|---|---|---|
| `disabled` (default) | Nothing | |
| `rdfs` | Six RDFS rules: domain, range, sub-class and sub-property | No axiomatic triples; `rdfs-full` has every RDFS rule and the axioms |
| `rdfs-plus`, `owl-horst` | RDFS with equality, inverse, symmetric, transitive and functional properties; `owl-horst` adds `hasValue`, `someValuesFrom`, `allValuesFrom` | Named subsets of the OWL 2 RL rules ([rules](docs/spec/reasoning-semantics.md#modes)) |
| `owl2-ql` | The OWL 2 QL rules, plus answers through existential axioms by query rewriting ([design](docs/design/ql-rewriting.md)); a commit that violates a disjointness, irreflexivity or asymmetry axiom is rejected | Answers say `sound-only` where an axiom outside QL could add an answer the query reads, where a pattern reaches a rewriting bound or runs without the rewriting (a variable predicate or class, a property path, a pattern inside an aggregate), and for queries the rewriting doesn't apply to, such as those with a dataset of their own |
| `owl2-rl` | The W3C OWL 2 RL/RDF rules; a commit that violates a consistency rule is rejected, naming the rule and its premises | Answers are `sound-only`, since the rules are complete only for ontologies inside the profile, unless `reasoner.ql_rewriting = "on"`, which checks each query as under `owl2-ql`. `owl:sameAs` reflexivity isn't stored; datatype rules in part ([omissions](docs/spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)) |
| `owl2-dl` | `owl2-rl` as a lower bound, plus OWL 2 DL engines (a hypertableau and a consequence-based core): consistency checked on commit (`dl.consistency`, on by default; an undecided check lets the commit through and the status says so), classification, realisation, and certain answers where they can be proven ([design](docs/design/owl2-dl.md)) | Some answers are `sound-only`, for example because a budget ran out or a candidate stayed undecided. A query that uses `OPTIONAL`, `MINUS`, `NOT EXISTS` or another operator that isn't monotone is `unsound` instead wherever its answers aren't complete, as rows may be wrong as well as missing. A reader of only some graphs, or a query with a dataset of its own, gets the lower bound as `sound-only` |
| `custom` | Your rules alone, in Notation3 or the `.pie` rule format; they can also be added to any mode above | No built-ins but `log:equalTo` and `log:notEqualTo`; no new blank nodes in conclusions ([user rules](docs/spec/reasoning-semantics.md#user-rules-notation3)) |

Also:

- **Explanations,** with reasoning on: why a statement holds, as a derivation or as
  minimal sets of asserted statements it follows from; under `owl2-dl`, minimal sets of
  axioms (`/explain`, [HTTP interface](docs/ops/http-api.md#engine-api-apiv1)).
- **Classification and realisation** of the ontology by the DL engines, in any mode
  ([HTTP interface](docs/ops/http-api.md#classification)).
- **Incremental maintenance,** tested against full recomputation on random changes
  ([maintenance](docs/spec/reasoning-semantics.md#maintenance)).
- **Conformance:** the W3C OWL 2 test cases for RL ([runner](crates/nrese-store/tests/w3c_owl2_rl)),
  QL ([runner](crates/nrese-store/tests/w3c_owl2_ql), 9 failures listed with reasons) and DL
  ([runner](crates/nrese-dl/tests/tableau_w3c), the undecided and disputed cases listed).
- **Upgrades:** stored inferences carry a fingerprint of the rules that made them; a build
  that changes what a ruleset derives recomputes them once at start
  ([freshness](docs/spec/reasoning-semantics.md#freshness)).
- **Limit:** rules read every graph, and inferences go to the default graph. A named graph
  isn't a reasoning boundary yet ([graph scope](docs/spec/reasoning-semantics.md#graph-scope)).

#### What an answer promises

Every SPARQL query answer, of every query form and over every protocol, carries an
`NRESE-Completeness` header; `EXPLAIN` shows the same status, and classification and
realisation carry it too ([details](docs/ops/http-api.md#sparql)). Graph and statement
downloads (Graph Store, RDF4J `/statements`) carry none.

| Status | Means |
|---|---|
| `complete; regime=…` | Every answer the regime entails is there. Under `owl2-dl`, `owl2-ql` and `owl2-rl` that is every certain answer under the OWL 2 Direct Semantics; under `owl2-ql` and `owl2-rl` it is given only where the QL rewriting ran on the query. Without reasoning, or with `infer=false`, the header says `complete` with no regime |
| `sound-only; regime=…; reasons="…"` | Every row is entailed, but rows may be missing; the reasons say why. Without the QL rewriting, every answer over a ruleset's closure is `sound-only`, because the rules are complete only for ontologies inside their profile. Where the rewriting runs, each query is checked against the schema, and `complete` is withheld when an axiom outside the profile, or an individual the axioms imply, could add an answer the query reads |
| `unsound` | Rows may be wrong as well as missing: inside a transaction whose pending changes delete statements (their inferences stay until the commit), or where answers that may be missing sit under `MINUS`, `NOT EXISTS` or another operator that can turn a missing row into a wrong one |
| `not-certified` | No status can be given, so rows may be missing or wrong. This is the case for a reader of only some graphs, with inferred statements visible, where the QL rewriting runs |

Under `owl2-dl` you can ask for exact answers only (`dl-answers=exact`): an answer that
can't be proven complete is then a 422 error naming the undecided candidates. With user
rules, no answer is claimed complete.

### SHACL

| Capability | Limits and notes |
|---|---|
| [SHACL](https://www.w3.org/TR/shacl/) (W3C Recommendation, 2017): Core and SHACL-SPARQL | W3C suite for both, with no known failure listed ([runner](crates/nrese-shacl/tests/w3c_shacl)). Data is validated with its inferred statements unless `infer=false`. Not supported: SHACL Advanced Features (rules), and `sh:entailment`, which is reported as a failure |
| Validation on request (`/dataset/shacl`), against stored shapes or shapes sent with the request | Sent shapes live in a transaction that is never committed, and count against the request's memory ([HTTP interface](docs/ops/http-api.md#shacl-validation)) |
| Validation on commit (`shacl.gate`: `report` or `enforce`), only for the focus nodes the commit can affect | Off by default ([configuration](docs/ops/config-reference.md)) |

### Storage, transactions and durability

| Capability | Limits and notes |
|---|---|
| Its own storage engine: dictionary-encoded terms, seven sorted index orders, log-structured runs, snapshots that readers never wait for ([architecture](docs/ARCHITECTURE.md#4-engine-design-l1)) | |
| In memory or on disk: a checksummed write-ahead log and checkpoints; recovery cuts a torn tail and, unless configured otherwise, refuses to open on other damage | |
| Atomic commits: a write is planned, validated (reasoning, consistency, SHACL) and committed as one revision; a request that times out commits nothing ([data flow](docs/ARCHITECTURE.md#22-data-flow)) | One writer at a time |
| Client transactions, over the RDF4J protocol or the engine API: reads see the transaction's own changes | Each read takes the latest commit; `SNAPSHOT` and `SERIALIZABLE` isolation are refused |
| Backups: N-Quads exports with a checksum, image backups, point-in-time restore from the archived log ([server setup](docs/ops/server-setup.md), [drills](docs/ops/backup-restore-drills.md)) | |
| Read replicas that follow a primary's log ([replication](docs/ops/replication.md)) | No automatic failover; no partitioning across machines |
| Several repositories per server, each with its own reasoning and rules | |

#### What a commit does

A write (a SPARQL update, a Graph Store write, the commit of a client transaction) is
one commit, and its operations apply in order, each seeing the ones before it. Before
anything is visible:

1. the reasoner updates the inferred statements from the change, in the same
   transaction, and checks consistency (under `owl2-dl` also with the DL engines; a check
   they don't decide lets the commit through, and the status says `unknown`);
2. with the SHACL gate on, the shapes validate the focus nodes the change can affect,
   inferred statements included. Only what the commit introduces counts: data that was
   invalid before doesn't block it.

If a check fails, or the request times out, nothing of the request is committed: no
asserted statement, no inference, and the response says why. Otherwise the asserted and
the inferred changes are written to the log (synced, on disk) and published together as
one revision, so a query sees all of the commit or none of it. A deleted statement's
inferences go in the same commit, unless they still follow from what remains. The
gate's result isn't stored (`report` logs it); `GET /dataset/shacl` validates the current
data on request. Bulk loads (`nrese-server load`) don't go through these gates. With
reasoning on, a load recomputes the inferences afterwards and fails when the rules'
consistency checks find a violation (under `owl2-dl`, those of OWL 2 RL: the DL check
doesn't run); the data stays, and the store is quarantined. With reasoning off, nothing
is checked
([storage contract](docs/spec/02-storage-and-transactions.md#writes),
[consistency](docs/spec/reasoning-semantics.md#consistency),
[SHACL gate](docs/ops/config-reference.md)).

### Access control and operations

| Capability | Limits and notes |
|---|---|
| Authentication: static bearer tokens, JWT, OIDC token introspection, client certificates from trusted proxies, and local users with Argon2id passwords ([configuration](docs/ops/config-reference.md#auth-modes)) | Off by default (`auth.mode = none`) |
| Graph-level access: roles, workspaces and personal spaces; each user's queries see only the graphs it may read, and the policy decides which inferences it sees ([access control](docs/ops/http-api.md#graph-level-access-control), [ADR-0008](docs/adr/0008-users-workspaces-policies.md)) | Endpoints over the whole dataset (classification, SHACL, autocomplete) are for readers of every graph |
| Deployment postures, from an open workbench to read-only and authenticated servers; timeouts, memory budgets per request, rate limits | [Configuration](docs/ops/config-reference.md) |
| Health and readiness, Prometheus metrics, running queries listed and cancelled ([operations](docs/ops/http-api.md#service-and-operations)) | |

### Interfaces

| Interface | What |
|---|---|
| `/dataset/…` | SPARQL Protocol and Graph Store Protocol for the default repository |
| `/repositories/…` | The [RDF4J REST protocol](https://rdf4j.org/documentation/reference/rest-api/), version 12 (`GET /protocol`), for RDF4J's `HTTPRepository` and the tools built on it ([HTTP interface](docs/ops/http-api.md#rdf4j-protocol)) |
| `/api/v1/…` | One API for every capability, described as OpenAPI 3.1 at `/api/v1/openapi.json` ([HTTP interface](docs/ops/http-api.md#engine-api-apiv1)) |
| `/console`, `/ops` | A browser console for queries, updates and graphs, and one for operators ([apps/nrese-console](apps/nrese-console)) |
| `nrese-server` | The server, and the commands `load`, `query`, `convert`, `backup`, `restore`, `prune-archive`, `check-config`, `config-schema` and `print-query` ([server setup](docs/ops/server-setup.md)) |
| Console CLI | A small TypeScript command-line client of the same API ([getting started](docs/dev/getting-started.md#frontend-cli)) |
| Errors | Every HTTP error is a problem document (RFC 9457) with the request id the server's log also carries ([status codes](docs/ops/http-api.md#status-codes), [a worked example](docs/ops/http-api.md#example-a-sparql-syntax-error)); a server that doesn't start says why on standard error ([startup errors](docs/ops/server-setup.md#startup-errors)) |

Configuration comes from a TOML file, environment variables or `--set`, with a JSON
Schema (`nrese-server config-schema`). The [Dockerfile](Dockerfile) builds images for
`linux/amd64` and `linux/arm64`.

## How it is built

The workspace is split into crates by what they own. Dependencies point down only: a
crate may use a crate in a lower layer, or one placed below it in its own layer, never
one above. Each crate's `lib.rs` opens with what
it owns. [ARCHITECTURE.md](docs/ARCHITECTURE.md) follows a write and a query from
request to answer, names for each concept its page and its code, and says at which layer
each kind of problem is fixed; the decisions behind it, with their reasons, are in
[adr/](docs/adr/README.md).

```mermaid
flowchart TB
  subgraph L5[apps]
    console[nrese-console]
  end
  subgraph L4[server]
    server[nrese-server]
  end
  subgraph L3[store]
    store[nrese-store]
  end
  subgraph L2[evaluation]
    sparql[nrese-sparql]
    reasoner[nrese-reasoner]
    shacl[nrese-shacl]
  end
  subgraph L1[engines]
    engine[nrese-engine]
    dl[nrese-dl]
    owl[nrese-owl]
    vector[nrese-vector]
    exec[nrese-exec]
  end
  subgraph L0[crates/rdf]
    rdfio[nrese-rdf-io]
    syntax[nrese-sparql-syntax]
    results[nrese-sparql-results]
    rdf[nrese-rdf]
    json[nrese-json]
    xsd[nrese-xsd]
  end
  console -. HTTP .-> server
  server --> store
  store --> shacl & reasoner & dl
  shacl --> sparql
  sparql --> engine & owl & syntax & results
  reasoner --> engine & owl & rdfio
  dl --> owl & exec & xsd
  engine --> vector & rdf
  vector --> exec
  rdfio --> rdf & json
  syntax --> rdf
  results --> rdf & json
  rdf -.-> xsd
```

The picture leaves out edges that a longer path already implies; the table lists every
direct dependency (test-only ones left out). None of the crates is on crates.io
(`publish = false`), and none promises a stable API yet. "On its own" says whether a
crate's job stands without the rest of the stack; to use one, take it as a git dependency
pinned to a commit. Their API reference isn't published: `cargo doc -p CRATE --open` builds
it from a checkout, and each crate's `lib.rs` opens with what the crate owns.

| Crate | Owns | Uses | On its own |
|---|---|---|---|
| [`nrese-xsd`](crates/rdf/nrese-xsd) | XML Schema datatypes: numbers, dates, times, durations with SPARQL's operations; OWL 2's datatype map | – | Yes |
| [`nrese-json`](crates/rdf/nrese-json) | A strict JSON pull parser, DOM and writer; JSON canonicalisation | – | Yes |
| [`nrese-rdf`](crates/rdf/nrese-rdf) | The RDF model: IRIs, terms, triples, quads, datasets, vocabularies; RDFC-1.0 | `nrese-xsd` (optional) | Yes |
| [`nrese-rdf-io`](crates/rdf/nrese-rdf-io) | Readers and writers for every RDF syntax above | `nrese-rdf`, `nrese-json` | Yes |
| [`nrese-sparql-syntax`](crates/rdf/nrese-sparql-syntax) | SPARQL 1.1 and 1.2 parser, algebra and writer | `nrese-rdf` | Yes |
| [`nrese-sparql-results`](crates/rdf/nrese-sparql-results) | SPARQL results in JSON, XML, CSV, TSV, read and written | `nrese-rdf`, `nrese-json` | Yes |
| [`nrese-exec`](crates/nrese-exec) | Id-level tables, joins, sorting, grouping, graph closures, memory budgets | – | Yes: knows no terms |
| [`nrese-vector`](crates/nrese-vector) | Vector distances, exact scans, HNSW with filters | `nrese-exec` | Yes |
| [`nrese-owl`](crates/nrese-owl) | The OWL 2 structural model read from RDF or functional syntax, profile checks, normalisation into DL clauses, proofs and justifications | – | Yes: works over the caller's term ids |
| [`nrese-dl`](crates/nrese-dl) | The OWL 2 DL engines: consequence-based core, hypertableau, bounds, classification, realisation, model certificates | `nrese-owl`, `nrese-exec`, `nrese-xsd` | Yes, with example command-line tools |
| [`nrese-engine`](crates/nrese-engine) | Term dictionary, quad indexes, snapshots, transactions, write-ahead log, checkpoints, bulk load, statistics, full-text and vector indexes | `nrese-rdf`, `nrese-exec`, `nrese-vector` | Yes: a quad store without SPARQL |
| [`nrese-sparql`](crates/nrese-sparql) | SPARQL evaluation: planner, executor, `EXPLAIN`, result cache, updates, QL rewriting, text, vector and GeoSPARQL search | `nrese-engine`, `nrese-exec`, `nrese-owl`, `nrese-rdf`, `nrese-xsd`, `nrese-sparql-syntax`, `nrese-sparql-results` | With `nrese-engine` |
| [`nrese-reasoner`](crates/nrese-reasoner) | Rule IR, built-in rulesets, user rules, materialisation, incremental maintenance, consistency, explanations | `nrese-engine`, `nrese-exec`, `nrese-owl`, `nrese-rdf`, `nrese-rdf-io`, `nrese-xsd` | With `nrese-engine` |
| [`nrese-shacl`](crates/nrese-shacl) | SHACL shapes compiled and validated, whole or per change | `nrese-engine`, `nrese-exec`, `nrese-sparql`, `nrese-sparql-syntax`, `nrese-rdf`, `nrese-xsd` | With `nrese-engine` and `nrese-sparql` |
| [`nrese-store`](crates/nrese-store) | The database as a library: operations, the commit pipeline and its gates, repositories, access state, bulk loads and quarantine, OWL 2 DL answers and their status, backups | every crate above | Yes: everything but HTTP |
| [`nrese-server`](crates/nrese-server) | HTTP, authentication, limits, configuration, the console's hosting, the `nrese-server` binary | `nrese-store`, `nrese-sparql`, `nrese-reasoner`, `nrese-exec`, `nrese-rdf`, `nrese-rdf-io`, `nrese-xsd`, `nrese-sparql-results` | The binary |
| [`apps/nrese-console`](apps/nrese-console) | The browser console and its CLI (TypeScript, React) | the HTTP API only | With a server |

Two more crates serve the tests: [`nrese-sparql-reference`](crates/nrese-sparql-reference), a
SPARQL evaluator written straight from the specification's algebra, which the executor is
compared against; and [`nrese-fuzz`](crates/nrese-fuzz), fuzz targets for every parser of
untrusted input ([fuzz/](fuzz/README.md)). Benchmarks live in [benches/](benches/README.md); the
runs recorded there are as of their dates.

## Documentation

| To | Read |
|---|---|
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
- Upgrading a store written by an earlier version: what changes on purpose, and rolling back
  ([upgrade](docs/ops/server-setup.md#131-upgrading-a-store-from-main-to-v2)).
- Reasoning semantics versions 4 (7 November 2026) and 3 (28 October 2026): which stored
  verdicts and closures are rebuilt once ([reasoning semantics](docs/spec/reasoning-semantics.md)).
- Crash recovery of the write-ahead log (2 November 2026) and durable renames on Windows
  (7 November 2026): what each changed ([ADR-0002](docs/adr/0002-engine-storage-lsm-permutations.md)).
- Routes of the earlier release, deprecated since 3 October 2026 and answering until the next
  release ([HTTP interface](docs/ops/http-api.md#service-and-operations)).

What is done and what is open, item by item: [docs/STATUS.md](docs/STATUS.md) (its counts
and measurements are as of when each was recorded); the order
of work: [docs/ROADMAP.md](docs/ROADMAP.md); the main limits and deviations, and when to choose
another system: [docs/limits.md](docs/limits.md).

Platforms: the image for `linux/amd64` and `linux/arm64`; the merge checks also build and
test on Windows ([scripts/check.sh](scripts/check.sh)). A stop signal (SIGTERM, ctrl-c)
lets the requests in flight finish and closes the store; a stop not done 8 s after the
signal ends the process as a kill would. On Windows only ctrl-c in its console stops it
this way, not a service stop ([stopping](docs/ops/server-setup.md#64-stopping)).
After a crash or a kill, an on-disk store
recovers every acknowledged commit; on damage other than a torn last write it refuses to
open, unless configured otherwise ([durability](docs/spec/02-storage-and-transactions.md#durability-on-disk-mode)); backups
and restores: [server setup](docs/ops/server-setup.md#10-backup-and-recovery-setup),
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
