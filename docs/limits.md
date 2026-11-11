# Limits and deviations

What NRESE doesn't do, and where it departs from a standard on purpose, as of 11 November
2026. Each item is stated in the document it links to; this page only gathers them. What
is planned is in [ROADMAP.md](ROADMAP.md), and nothing here is a promise about it.

## Choose another system when

- **You need several writers at once.** One commit runs at a time; reads never wait for
  it ([storage](spec/02-storage-and-transactions.md#writes)).
- **You need automatic failover, or data spread over several machines.** Read replicas
  follow a primary's log; nothing promotes a replica or partitions the data
  ([replication](ops/replication.md)).
- **You need named graphs as separate reasoning contexts.** Rules read every graph, and
  inferences go to the default graph ([graph scope](spec/reasoning-semantics.md#graph-scope)).
- **You need every OWL 2 DL answer proven complete.** Some answers are `sound-only` and say
  so; asking for exact answers turns those into an error rather than a guess
  ([owl2-dl](spec/reasoning-semantics.md#owl2-dl-owl-2-dl-answers-with-their-completeness)).
- **You need SHACL rules (SHACL Advanced Features) or `sh:entailment`.** Neither is
  supported ([SHACL limits](ops/http-api.md#shacl-validation)).
- **You need property-graph query languages** (openCypher, Gremlin, GQL). Only SPARQL is
  offered.
- **You need the store to compute embeddings.** Vector search works on vectors you bring
  ([vector search](ops/http-api.md#sparql)).
- **You want a released Rust library.** The crates aren't published on crates.io.

## SPARQL

- Update's `LOAD` is refused, and `LOAD SILENT` does nothing. Load data over the Graph Store
  Protocol or with `nrese-server load` ([server setup](ops/server-setup.md)).
- `SERVICE` calls only the endpoints `federation.allow` names
  ([configuration](ops/config-reference.md)).
- Two deliberate deviations: `HAVING` may name a `SELECT` alias, and a default graph merged
  from several graphs keeps a blank node they share as one ([why](ops/http-api.md#sparql)).
- A reader of only some graphs gets no plan steps in `EXPLAIN`.

## RDF syntaxes

- JSON-LD remote contexts aren't fetched; a library caller supplies a document loader.
- Notation3 is read in the library only (not over HTTP), and only what RDF can hold.
- Blank nodes are scoped to one write: every request gets fresh ones
  ([storage](spec/02-storage-and-transactions.md#data-model)).

## Reasoning

- **Materialised, in the default graph.** A statement both asserted and derivable counts as
  asserted ([storage](spec/02-storage-and-transactions.md#data-model)).
- **`rdfs`** is a six-rule subset without axiomatic triples; `rdfs-full` has every RDFS rule
  ([rdfs](spec/reasoning-semantics.md#rdfs-a-6-rule-rdfs-subset)).
- **`owl2-rl`** doesn't store reflexive `owl:sameAs` (`?x owl:sameAs ?x` returns no
  reflexive pairs), checks the datatype rules in part, and doesn't store axiomatic triples.
  Lists past their limits are reported, not truncated
  ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).
- **What an answer promises.** Without the OWL 2 QL rewriting, every answer over a
  ruleset's closure is `sound-only`. The rewriting runs by default under `owl2-ql` only.
  Under `owl2-dl`, budgets and undecided candidates give `sound-only`, and an operator
  that isn't monotone over answers that may be missing gives `unsound`
  ([what an answer promises](../README.md#what-an-answer-promises)).
- **User rules** (Notation3 or `.pie`) have no built-ins but `log:equalTo` and
  `log:notEqualTo` and make no new blank nodes; with user rules no answer is claimed
  complete ([user rules](spec/reasoning-semantics.md#user-rules-notation3)).
- **Bulk loads skip the commit gates** ([storage](spec/02-storage-and-transactions.md#bulk-loads)):
  `nrese-server load` fails when the loaded data is inconsistent, and the data stays
  loaded; SHACL isn't checked. A store whose data is found inconsistent is quarantined
  until it is repaired ([consistency](spec/reasoning-semantics.md#consistency)).

## SHACL

- SHACL Core and SHACL-SPARQL. Not supported: SHACL Advanced Features, and `sh:entailment`,
  which is reported as a failure ([HTTP interface](ops/http-api.md#shacl-validation)).
- The commit gate is off by default; when on, it checks only what a commit introduces
  ([configuration](ops/config-reference.md)).
- A validation with shapes sent in the request holds the writer while it runs.

## Storage and transactions

- One writer at a time; a commit holds at most 4 GiB of log payload, about 130 M quads;
  larger loads go through the bulk loader
  ([storage](spec/02-storage-and-transactions.md#limits)).
- Client transactions read the latest commit; `SNAPSHOT` and `SERIALIZABLE` isolation are
  refused ([RDF4J protocol](ops/http-api.md#rdf4j-protocol)).

## Access control

- No authentication by default (`auth.mode = none`)
  ([configuration](ops/config-reference.md#auth-modes)).
- Endpoints over the whole dataset (classification, SHACL validation, autocomplete) are
  only for readers of every graph
  ([access control](ops/http-api.md#graph-level-access-control)).
