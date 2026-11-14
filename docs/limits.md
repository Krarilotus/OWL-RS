# Limits and deviations

What NRESE doesn't do, and where it departs from a standard on purpose, for the code of
the commit this page is in. Each item is stated in the document it links to; this page only gathers them, each
with what to do instead, or "none known" where there is nothing. It is the detail of the
"left out" list of the [scope](what-it-covers.md#scope), which also says what is in and what is
planned.

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
- `HAVING` may name a `SELECT` alias, an extension ([why](ops/http-api.md#sparql)). For
  a query that runs the same on other stores, repeat the aggregate:
  `HAVING (COUNT(?x) > 1)`.
- A default graph merged from several graphs keeps a blank node they share as one node,
  where SPARQL's RDF merge keeps them apart ([why](ops/http-api.md#sparql)). No setting
  gives the standard merge; where it matters, read with `GRAPH ?g { … }` and keep `?g` in
  the answer.
- GeoSPARQL measures in metres only in CRS84 and a listed set of projected systems, and reads
  no GML arcs or 3D solids: such a measure or literal gives no value. Convert coordinates and
  approximate arcs before loading; for solids, none known ([GeoSPARQL](ops/http-api.md#sparql)).
- Full-text query strings drop Lucene's `OR`, `+`, field names and boosts
  ([full-text search](ops/http-api.md#sparql)). Words without `AND` match as any of them, as
  `OR` would; to require words, join them with `AND` (every word is then needed); to search one
  property, name it in `text:query`'s list; for boosts, none known.
- A named graph without statements doesn't exist: `CREATE GRAPH` stores nothing, and an
  emptied graph answers 404 ([Graph Store Protocol](ops/http-api.md#graph-store-protocol)).
  Read that 404 as an empty graph; to keep an empty graph listed, none known.
- A reader of only some graphs gets no plan steps in `EXPLAIN`. None known.
- Directional strings in the data alone don't make a result announce RDF 1.2; a query that
  makes or matches them, or triple terms, does ([announcing](ops/http-api.md#requests-in-detail)).
  None known: a client that may meet them should read results as RDF 1.2.

## RDF syntaxes

- JSON-LD remote contexts aren't fetched. Over HTTP, send the context inline; a library
  caller supplies a document loader.
- Notation3 is read in the library only (not over HTTP), and only what RDF can hold. N3
  rules go to the reasoner through `reasoner.rules`
  ([configuration](ops/config-reference.md#reasoner)).
- Blank nodes are scoped to one write: every request gets fresh ones
  ([storage](spec/02-storage-and-transactions.md#data-model)). Give a node an IRI where a
  later write must name it.

## Reasoning

- **Materialised, in the default graph.** A statement both asserted and derivable counts as
  asserted ([storage](spec/02-storage-and-transactions.md#data-model)).
- **`rdfs`** is a six-rule subset without axiomatic triples; `rdfs-full` has every RDFS rule
  ([rdfs](spec/reasoning-semantics.md#rdfs-a-6-rule-rdfs-subset)).
- **`owl2-rl`** doesn't infer `x owl:sameAs x` for every term (`?x owl:sameAs ?x` doesn't
  return one for each; an asserted one is returned), checks the datatype rules in part, and of the axiomatic triples holds only `dt-type1`'s 32 datatype statements, in every store ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).
  Lists past their limits (a property chain or key of more than 250 properties, more than
  64 member sequences) aren't used at all, never truncated, and each is reported in the log
  and the reasoning diagnostics
  ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)); none known beyond
  keeping lists within those limits. Literals aren't
  equated by value (`"1"^^xsd:integer` and `"01"^^xsd:integer` stay two terms): compare
  values in a `FILTER` (`?v = 1`), which SPARQL does by value. Otherwise none known.
- **What an answer promises.** Without the OWL 2 QL rewriting, every answer over a
  ruleset's closure is `sound-only`. The rewriting runs by default under `owl2-ql` only;
  under `owl2-rl`, `reasoner.ql_rewriting = "on"` checks each query as under `owl2-ql`.
  Under `owl2-dl`, budgets and undecided candidates give `sound-only` (raise the `dl.*`
  budgets, or ask `dl-answers=exact` to get an error instead), and an operator that isn't
  monotone over answers that may be missing gives `unsound`
  ([what an answer promises](what-it-covers.md#what-an-answer-promises)).
- **User rules** (Notation3 or `.pie`) have no built-ins but `log:equalTo` and
  `log:notEqualTo` and make no new blank nodes; with user rules no answer is claimed
  complete ([user rules](spec/reasoning-semantics.md#user-rules-notation3)). None known.
- **Undecided consistency.** Under `owl2-dl`, a commit whose consistency check isn't decided
  (a budget or the timeout) goes through, and the status says `unknown`; no answer is then
  complete ([consistency](spec/reasoning-semantics.md#consistency)). Raise `dl.timeout`
  and the `dl.*` budgets ([configuration](ops/config-reference.md#reasoner)).
- **Bulk loads skip the commit gates** ([storage](spec/02-storage-and-transactions.md#bulk-loads)):
  neither SHACL nor the OWL 2 DL consistency check runs. With reasoning on,
  `nrese-server load` recomputes the inferences and fails when the rules' consistency
  checks find a violation (under `owl2-dl`, those of OWL 2 RL); the data stays loaded, and
  the store is quarantined until it is repaired. With reasoning off, nothing is checked
  ([consistency](spec/reasoning-semantics.md#consistency)). For the gates, load over HTTP
  (Graph Store), or validate after the load with `GET /dataset/shacl`.

## SHACL

- SHACL Core and SHACL-SPARQL. Not supported: SHACL Advanced Features, and `sh:entailment`,
  which is reported as a failure ([HTTP interface](ops/http-api.md#shacl-validation)).
  None known.
- The commit gate is off by default; when on, it checks only what a commit introduces
  ([configuration](ops/config-reference.md)). To check data already stored, validate it
  with `GET /dataset/shacl`.

## Storage and transactions

- One writer at a time; a commit holds at most 4 GiB of log payload, about 130 M quads;
  larger loads go through the bulk loader
  ([storage](spec/02-storage-and-transactions.md#limits)).
- Client transactions read the latest commit; `SNAPSHOT` and `SERIALIZABLE` isolation are
  refused ([RDF4J protocol](ops/http-api.md#rdf4j-protocol)). A read, then a write
  that depends on it, fits in one SPARQL update (`DELETE … INSERT … WHERE`), which is one
  commit.
- A stop signal during start-up acts as a kill, and on Windows a service stop or
  `taskkill /F` ends the server without letting its requests finish; acknowledged commits
  are kept either way ([stopping](ops/server-setup.md#64-stopping)). On Windows, stop it
  with ctrl-c in its console.

## Access control

- No authentication by default (`auth.mode = none`): set one before the server is reachable
  from other machines ([configuration](ops/config-reference.md#auth-modes)).
- Endpoints over the whole dataset (classification, SHACL validation, autocomplete) are
  only for readers of every graph
  ([access control](ops/http-api.md#graph-level-access-control)). None known.
