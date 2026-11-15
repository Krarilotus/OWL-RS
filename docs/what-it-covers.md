# What it covers

This page says what NRESE does in the version it comes with, not in a plan or another version. It
starts with the scope, what
is in, what is left out and what is planned, one line each with a link to the page that has the
detail.

## Scope

The sections below say what is in. Each item there links the specification, document or test
that backs it and gives its limits beside it, and none of it is a plan.

- [SPARQL](#sparql): SPARQL 1.1 query, update and federation, the SPARQL 1.2 drafts, and
  full-text, vector and, in part, GeoSPARQL search
- [RDF syntaxes](#rdf-syntaxes): the W3C RDF syntaxes, JSON-LD 1.1, Notation3 data in the
  library, canonicalisation and parallel bulk loads
- [Reasoning](#reasoning): from RDFS to OWL 2 DL, materialised and kept up to date by every
  commit, and the status every answer carries
- [SHACL](#shacl): SHACL Core and SHACL-SPARQL, on request and as a commit gate
- [Storage](#storage-transactions-and-durability): its own storage engine, with atomic commits,
  durability, client transactions, backups, read replicas and several repositories
- [Access control and operations](#access-control-and-operations): authentication, graph-level
  access control, deployment postures, budgets and metrics
- [Interfaces](#interfaces): the SPARQL, Graph Store, RDF4J and engine HTTP interfaces, the
  browser console, the server's commands and the console CLI

What is left out today is below, and [limits](limits.md) says what to do instead:

- several writers at once, automatic failover and data split across machines
  ([choose another system](limits.md#choose-another-system-when))
- named graphs as separate reasoning contexts (planned for v2.0), and every OWL 2 DL answer
  proven complete ([choose another system](limits.md#choose-another-system-when))
- SHACL Advanced Features (rules) and `sh:entailment` ([SHACL](limits.md#shacl))
- embeddings computed by the store, a released Rust library, and property-graph query languages
  (planned, below) ([choose another system](limits.md#choose-another-system-when))
- the deviations from the standards, such as SPARQL Update's `LOAD`
  ([SPARQL](limits.md#sparql), [RDF syntaxes](limits.md#rdf-syntaxes),
  [reasoning](limits.md#reasoning), [storage](limits.md#storage-and-transactions))

What is planned is in the [roadmap](ROADMAP.md), in order, and only the stages it marks done are
in the code:

- the open items before v2 goes to `main`, then benchmarks, the merge into `main`, concise docs
  and a refactoring, then v3 ([the order](ROADMAP.md#the-order-of-work))
- property-graph queries (ISO GQL, openCypher) over the same store, failover and a write
  cluster, mappings from relational, CSV and JSON sources, questions in natural language turned
  into SPARQL, versioned graphs and an ontology editor with Linked Data publishing
  ([the order](ROADMAP.md#the-order-of-work), item 6)
- for OWL 2 DL, classification speed and tighter answer bounds ([DL](ROADMAP.md#dl))
- with a version named: one snapshot for a whole RDF4J transaction in v1.6
  ([RDF4J protocol](ops/http-api.md#rdf4j-protocol)), named graphs as reasoning boundaries in
  v2.0 ([engine gaps](STATUS.md#engine-gaps)), and a CRS database with units and conversions for
  every system in v2.0 ([requests in detail](ops/http-api.md#requests-in-detail))
- not yet ordered: an audit log, SBOM and signed releases, a threat model, the frontend on the
  engine API, and scaling across machines ([not yet ordered](ROADMAP.md#not-yet-ordered))

## SPARQL

| Capability | Limits and notes |
|---|---|
| [SPARQL 1.1](https://www.w3.org/TR/sparql11-overview/) Query, Update and federated `SERVICE` | The W3C suites run in the checks and in CI, with no known failure listed (505 of 505 on 2 October 2026: [capability matrix](spec/06-target-capability-matrix.md); [evaluation](../crates/nrese-sparql/tests/w3c_sparql11), [syntax](../crates/rdf/nrese-sparql-syntax/tests/w3c_syntax)). Deviations (`LOAD`, `SERVICE`, `HAVING` aliases, merged blank nodes), each with what to do: [limits](limits.md#sparql) |
| SPARQL 1.2 and RDF 1.2 drafts: triple terms, directional strings | Results announce `version 1.2` when they may use them ([HTTP interface](ops/http-api.md#sparql)) |
| [SPARQL 1.1 Protocol](https://www.w3.org/TR/sparql11-protocol/), [Graph Store Protocol](https://www.w3.org/TR/sparql11-http-rdf-update/), service description | [HTTP interface](ops/http-api.md) |
| Results in JSON, XML, CSV and TSV; graphs in seven RDF syntaxes ([which](ops/http-api.md#requests-in-detail)) | Chosen by `Accept` ([formats](ops/http-api.md#sparql)) |
| `EXPLAIN`: the plan with estimated rows, or the run with actual rows and times | A reader of only some graphs gets no plan steps (they would reveal statistics of the others) |
| Full-text search (BM25) through the `bds:` and Jena `text:query` vocabularies, with phrases, prefixes, fuzzy words and stemming in 18 languages | One index over every string literal, built at the first search; an on-disk store keeps it across restarts ([full-text](ops/http-api.md#sparql)) |
| Vector search: vectors as literals (`"[…]"^^nrv:vector`), `SERVICE nrv:search` with cosine, dot product or Euclidean distance, exact or through HNSW, filtered by the rest of the query | You bring the vectors; the store computes no embeddings. Approximate search trades recall for speed through `nrv:searchBudget` ([vector search](ops/http-api.md#sparql)) |
| [GeoSPARQL 1.1](https://docs.ogc.org/is/22-047r1/22-047r1.html), in part: functions and relations over WKT, GeoJSON and simple GML, with an R-tree | Measures in metres only in CRS84 and a listed set of projected systems; no GML arcs or 3D solids ([GeoSPARQL](ops/http-api.md#sparql), [compliance runner](../crates/nrese-sparql/tests/geosparql_compliance)) |

## RDF syntaxes

| Capability | Limits and notes |
|---|---|
| Read and write [N-Triples](https://www.w3.org/TR/n-triples/), [N-Quads](https://www.w3.org/TR/n-quads/), [Turtle](https://www.w3.org/TR/turtle/), [TriG](https://www.w3.org/TR/trig/), [RDF/XML](https://www.w3.org/TR/rdf-syntax-grammar/) | W3C RDF 1.1 and 1.2 suites, each document also read one byte at a time and written back ([runner](../crates/rdf/nrese-rdf-io/tests/w3c_rdf)) |
| Read and write [JSON-LD 1.1](https://www.w3.org/TR/json-ld11/) | Remote contexts aren't fetched: a library caller supplies a document loader. W3C `toRdf`, `expand`, `fromRdf` suites; tests for JSON-LD 1.0 processors only and for generalised RDF are not run ([runner](../crates/rdf/nrese-rdf-io/tests/w3c_jsonld)) |
| Read [Notation3](https://w3c.github.io/N3/spec/) data, in the library | Only what RDF can hold; rules are read by the reasoner. Not offered over HTTP. W3C N3 group's parser tests, one known failure listed ([runner](../crates/rdf/nrese-rdf-io/tests/w3c_n3)) |
| RDF4J Binary RDF, versions 1 and 2 | Read in both versions, written as version 2; as a payload and a result format |
| [RDF Dataset Canonicalization](https://www.w3.org/TR/rdf-canon/) (RDFC-1.0) | W3C suite ([runner](../crates/rdf/nrese-rdf-io/tests/w3c_rdfc)) |
| Bulk loading, N-Triples and N-Quads on all cores ([server setup](ops/server-setup.md)); statements that don't parse are quarantined, not fatal (malformed XML or JSON and read errors still stop the load: [bulk loading](ops/server-setup.md#54-bulk-loading)) | Strict mode stops at the first error instead ([quarantine](design/quarantine.md)) |

## Reasoning

Inferences are materialised. They are computed when data is loaded, kept up to date by every
commit in the same revision, and stored apart from the asserted statements. A query reads both
by default, or only the asserted ones with `infer=false`.

The modes, from none to OWL 2 DL, are below in a line each. What each one computes, what it
leaves out and what its answers promise is in [the reasoning contract](spec/reasoning-semantics.md#modes).

- `disabled`, the default, computes nothing.
- `rdfs` has six RDFS rules, for domain, range, sub-class and sub-property, and `rdfs-full`
  has the RDFS rules and axioms but some that the contract lists.
- `rdfs-plus` and `owl-horst` add equality and inverse, symmetric, transitive and functional
  properties to RDFS, and `owl-horst` adds `hasValue`, `someValuesFrom` and `allValuesFrom`.
- `owl2-ql` has the OWL 2 QL rules, and gives answers through existential axioms by query
  rewriting ([design](design/ql-rewriting.md)). A query with a dataset of its own doesn't get
  the rewriting, so its answers say `sound-only`.
- `owl2-rl` has the W3C OWL 2 RL/RDF rules.
- `owl2-dl` takes `owl2-rl` as a lower bound and adds OWL 2 DL engines of its own, a
  hypertableau and a consequence-based core, for consistency, classification, realisation and
  the certain answers they can prove ([design](design/owl2-dl.md)).
- `custom` runs your rules alone, in Notation3 or the `.pie` rule format, and you can add such
  rules to any mode above as well.

A commit that, with its inferences, breaks a consistency rule of the mode is refused, and the
response names the rule and its premises ([consistency](spec/reasoning-semantics.md#consistency)).

Beside the modes:

- With reasoning on, you can ask why a statement holds and get its derivation, or the minimal
  sets of asserted statements it follows from, and under `owl2-dl` the minimal sets of axioms (`/explain`,
  [HTTP interface](ops/http-api.md#engine-api-apiv1)).
- The DL engines classify and realise the ontology in any mode
  ([HTTP interface](ops/http-api.md#classification)).
- The inferences are maintained incrementally, and this is tested against full recomputation on
  random changes ([maintenance](spec/reasoning-semantics.md#maintenance)).
- The W3C OWL 2 test cases check conformance for RL ([runner](../crates/nrese-store/tests/w3c_owl2_rl)),
  for QL ([runner](../crates/nrese-store/tests/w3c_owl2_ql), with 9 failures listed with their
  reasons) and for DL ([runner](../crates/nrese-dl/tests/tableau_w3c), with the undecided and
  disputed cases listed).
- Stored inferences carry a fingerprint of the rules that made them, so after an upgrade a build
  that changes what a ruleset derives recomputes them once at start
  ([freshness](spec/reasoning-semantics.md#freshness)).
- Rules read every graph, and inferences go to the default graph, so a named graph isn't a
  reasoning boundary yet ([graph scope](spec/reasoning-semantics.md#graph-scope)).

### What an answer promises

Every SPARQL query answer carries its status in an `NRESE-Completeness` header, whatever the
query form and the protocol. `EXPLAIN` shows the same status, and classification and realisation
carry it too ([details](ops/http-api.md#sparql)). Graph and statement downloads (Graph Store,
RDF4J `/statements`) carry none.

The status is one of the four the README names: `complete`, `sound-only`, `unsound` and
`not-certified`. What each promises under each mode is in
[the reasoning contract](spec/reasoning-semantics.md#what-complete-refers-to), and the header's
form, with its regime and reasons, in the [HTTP interface](ops/http-api.md#sparql). Inside a
transaction whose pending changes delete statements, answers are `unsound`, because those
statements' inferences stay until the commit.

Under `owl2-dl` you can ask for exact answers only (`dl-answers=exact`), and an answer that
can't be proven complete is then a 422 error naming the undecided candidates.

## SHACL

| Capability | Limits and notes |
|---|---|
| [SHACL](https://www.w3.org/TR/shacl/) (W3C Recommendation, 2017): Core and SHACL-SPARQL | W3C suite for both, with no known failure listed ([runner](../crates/nrese-shacl/tests/w3c_shacl)). Data is validated with its inferred statements unless `infer=false`. Not supported: SHACL Advanced Features (rules), and `sh:entailment`, which is reported as a failure |
| Validation on request (`/dataset/shacl`), against stored shapes or shapes sent with the request | Sent shapes live in a transaction that is never committed, and count against the request's memory ([HTTP interface](ops/http-api.md#shacl-validation)) |
| Validation on commit (`shacl.gate`: `report` or `enforce`), only for the focus nodes the commit can affect | Off by default ([configuration](ops/config-reference.md)) |

## Storage, transactions and durability

| Capability | Limits and notes |
|---|---|
| Its own storage engine: dictionary-encoded terms, sorted index orders (seven; four while all data is in the default graph), log-structured runs, snapshots that readers never wait for ([architecture](ARCHITECTURE.md#4-engine-design-l1)) | |
| In memory or on disk: a checksummed write-ahead log and checkpoints; recovery cuts a torn tail and, unless configured otherwise, refuses to open on other damage | |
| Atomic commits: a write is planned, validated (reasoning, consistency, SHACL) and committed as one revision; a request that times out commits nothing ([data flow](ARCHITECTURE.md#22-data-flow)) | One writer at a time |
| Client transactions, over the RDF4J protocol or the engine API: reads see the transaction's own changes | Each read takes the latest commit; `SNAPSHOT` and `SERIALIZABLE` isolation are refused |
| Backups: N-Quads exports with a checksum, image backups, point-in-time restore from the archived log ([server setup](ops/server-setup.md), [drills](ops/backup-restore-drills.md)) | |
| Read replicas that follow a primary's log ([replication](ops/replication.md)) | No automatic failover; no partitioning across machines |
| Several repositories per server, each with its own reasoning and rules | |

### What a commit does

A write (a SPARQL update, a Graph Store write or the commit of a client transaction) is one
commit, and its operations apply in order, each one seeing the ones before it. Before anything
becomes visible:

1. With reasoning on, the reasoner updates the inferred statements from the change, in the same
   transaction, and checks consistency. Under `owl2-dl` the DL engines check it too, and a check
   they don't decide lets the commit through, with the status `unknown`.
2. With the SHACL gate on, the shapes validate the focus nodes the change can affect, inferred
   statements included. Only what the commit introduces counts, so data that was invalid before
   doesn't block it.

If a check refuses the write, or the request times out, nothing of the request is committed,
neither an asserted statement nor an inference, and the response says why. Otherwise the
asserted and the inferred changes are written to the log (synced, on disk) and published
together as one revision, so a query sees all of the commit or none of it. When you delete a
statement, its inferences go in the same commit, unless they still follow from what remains.
The gate's result isn't stored (`report` logs it), and `GET /dataset/shacl` validates the
current data on request.

Bulk loads (`nrese-server load` and the import endpoint) and the ontology file loaded at
start-up don't go through these checks. With reasoning on, `nrese-server load` recomputes the
inferences afterwards and fails when the rules' consistency checks find a violation. Under
`owl2-dl` those are the OWL 2 RL rules' checks, because the DL check doesn't run. The data
stays, and the store is quarantined. With reasoning off, nothing is checked
([storage contract](spec/02-storage-and-transactions.md#writes),
[consistency](spec/reasoning-semantics.md#consistency),
[SHACL gate](ops/config-reference.md)).

## Access control and operations

| Capability | Limits and notes |
|---|---|
| Authentication: static bearer tokens, JWT, OIDC token introspection, client certificates from trusted proxies, and local users with Argon2id passwords ([configuration](ops/config-reference.md#auth-modes)) | Off by default (`auth.mode = none`) |
| Graph-level access: roles, workspaces and personal spaces; each user's queries see only the graphs it may read, and the policy decides which inferences it sees ([access control](ops/http-api.md#graph-level-access-control), [ADR-0008](adr/0008-users-workspaces-policies.md)) | Endpoints over the whole dataset (classification, SHACL, autocomplete) are for readers of every graph |
| Deployment postures, from an open workbench to read-only and authenticated servers; timeouts, memory budgets per request, rate limits | [Configuration](ops/config-reference.md) |
| Health and readiness, Prometheus metrics, running queries listed and cancelled ([operations](ops/http-api.md#service-and-operations)) | |

## Interfaces

| Interface | What |
|---|---|
| `/dataset/…` | SPARQL Protocol and Graph Store Protocol for the default repository |
| `/repositories/…` | The [RDF4J REST protocol](https://rdf4j.org/documentation/reference/rest-api/), version 12 (`GET /protocol`), for RDF4J's `HTTPRepository` and the tools built on it ([HTTP interface](ops/http-api.md#rdf4j-protocol)) |
| `/api/v1/…` | One API for what the server does (the library-only parts, such as Notation3 data and resolving quarantined statements, have none yet), described as OpenAPI 3.1 at `/api/v1/openapi.json` ([HTTP interface](ops/http-api.md#engine-api-apiv1)) |
| `/console`, `/ops` | A browser console for queries, updates and graphs, and one for operators ([apps/nrese-console](../apps/nrese-console)) |
| `nrese-server` | The server, and the commands `load`, `query`, `convert`, `backup`, `restore`, `prune-archive`, `check-config`, `config-schema` and `print-query` ([server setup](ops/server-setup.md)) |
| Console CLI | A small TypeScript command-line client of the same API ([getting started](dev/getting-started.md#frontend-cli)) |
| Errors | Every HTTP error is a problem document (RFC 9457) with the request id the server's log also carries, with three exceptions ([status codes](ops/http-api.md#status-codes), [a worked example](ops/http-api.md#example-a-sparql-syntax-error)); a server that doesn't start says why on standard error ([startup errors](ops/server-setup.md#startup-errors)) |

Configuration comes from a TOML file, environment variables or `--set`, with a JSON
Schema (`nrese-server config-schema`). The [Dockerfile](../Dockerfile) builds images for
`linux/amd64` and `linux/arm64`.
