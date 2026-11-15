# Limits and deviations

Where NRESE stops, and where it departs from a standard on purpose, in the commit these pages
come with. Where there is something to do instead, the item says so. What is in is on
[features and standards](what-it-covers.md).

## Choose another system when

- You need several writers at once. One commit runs at a time, and reads never wait for
  it ([storage](spec/02-storage-and-transactions.md#writes)).
- You need automatic failover, or data spread over several machines. Read replicas
  follow a primary's log, but nothing promotes a replica or partitions the data
  ([replication](ops/replication.md)).
- You need named graphs as separate reasoning contexts. Rules read every graph, and
  inferences go to the default graph ([graph scope](spec/reasoning-semantics.md#graph-scope)).
- You need every OWL 2 DL answer proven complete. Some answers are `sound-only`, which
  means rows may be missing, and if you ask for exact answers, a request whose answer can't be
  proven complete fails with an error ([owl2-dl](spec/reasoning-semantics.md#owl2-dl-owl-2-dl-answers-with-their-completeness)).
- You need SHACL rules (SHACL Advanced Features) or `sh:entailment`. Neither is
  supported ([SHACL limits](ops/http-api.md#shacl-validation)).
- You need property-graph query languages (openCypher, Gremlin, GQL). Only SPARQL is
  offered.
- You need the store to compute embeddings. Vector search works on vectors you bring
  ([vector search](ops/http-api.md#sparql)).
- You want a released Rust library. The crates aren't published on crates.io.

## SPARQL

- Update's `LOAD` is refused, and `LOAD SILENT` does nothing. Load data over the Graph Store
  Protocol or with `nrese-server load` ([loading data](ops/server-setup.md#loading-data)).
- `SERVICE` calls only the endpoints `federation.allow` names
  ([configuration](ops/config-reference.md)).
- `HAVING` may name a `SELECT` alias, an extension ([why](ops/http-api.md#sparql)). For
  a query that runs the same on other stores, repeat the aggregate:
  `HAVING (COUNT(?x) > 1)`.
- A default graph merged from several graphs keeps a blank node they share as one node,
  where SPARQL's RDF merge keeps them apart ([why](ops/http-api.md#sparql)). No setting
  gives the standard merge. Where it matters, read with `GRAPH ?g { … }` and keep `?g` in
  the answer.
- GeoSPARQL measures in metres only in CRS84 and a listed set of projected systems, and reads
  no GML arcs or 3D solids: such a measure or literal gives no value. Convert coordinates and
  approximate arcs before loading ([GeoSPARQL](ops/http-api.md#sparql)).
- Full-text query strings drop Lucene's `OR`, `+`, field names and boosts
  ([full-text search](ops/http-api.md#sparql)). Words without `AND` match as any of them, as
  `OR` would. To require words, join them with `AND` (every word is then needed), and to search
  one property, name it in `text:query`'s list.
- A named graph without statements doesn't exist. `CREATE GRAPH` stores nothing, and an
  emptied graph answers 404 ([Graph Store Protocol](ops/http-api.md#graph-store-protocol)).
  Read that 404 as an empty graph.
- A reader of only some graphs gets no plan steps in `EXPLAIN`.
- Directional strings in the data alone don't make a result announce RDF 1.2, but a query that
  makes or matches them, or triple terms, does ([announcing](ops/http-api.md#requests-in-detail)).
  A client that may meet them should read results as RDF 1.2.
- The first query that plans with a pattern counts the pattern's distinct values, and waits for the count. Counts
  are reused until the pattern's matches change by a quarter, and a pattern with more than about 4 million matches
  isn't counted afresh.
- After a load, the planner's statistics of which predicates occur together (characteristic sets) are built once,
  about 1.5 s at one thread for 10 M statements, kept beside the checkpoint, and rebuilt in the background once
  the data has changed by a quarter. Data with more than 131,072 distinct predicate sets (DBpedia among them) gets
  none. A build at first use runs inside the planning query and can't be cancelled.

## RDF syntaxes

- JSON-LD remote contexts aren't fetched. Over HTTP, send the context inline, and in the library
  supply a document loader.
- Notation3 is read in the library only (not over HTTP), and only what RDF can hold. N3
  rules go to the reasoner through `reasoner.rules`
  ([configuration](ops/config-reference.md#reasoner)).
- Blank nodes are scoped to one write, so every request gets fresh ones
  ([storage](spec/02-storage-and-transactions.md#data-model)). Give a node an IRI where a
  later write must name it.

## Reasoning

What each mode computes and what its answers promise is in
[the reasoning contract](spec/reasoning-semantics.md#modes). These are the parts of it that
leave something out.

- Inferences are materialised, in the default graph. A statement both asserted and derivable counts as
  asserted ([storage](spec/02-storage-and-transactions.md#data-model)).
- `rdfs` has no axiomatic triples, and `rdfs-full` leaves out `rdfD1` and the axioms about
  `rdf:_1`, `rdf:_2`, … ([rdfs](spec/reasoning-semantics.md#rdfs-a-6-rule-rdfs-subset),
  [modes](spec/reasoning-semantics.md#modes)).
- `owl2-rl` doesn't infer `x owl:sameAs x` for every term, so `?x owl:sameAs ?x` doesn't
  return one for each, though an asserted one is returned. It checks the datatype rules in part,
  and of the axiomatic triples it holds only `dt-type1`'s 32 datatype statements, in every store
  ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).
  Lists past their limits (a property chain or key of more than 250 properties, more than
  64 member sequences) aren't used at all, never truncated, and each is reported in the log
  and the reasoning diagnostics
  ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)), so keep lists within
  those limits. Literals aren't equated by value (`"1"^^xsd:integer` and
  `"01"^^xsd:integer` stay two terms), so compare values in a `FILTER` (`?v = 1`), which SPARQL
  does by value.
- Without the OWL 2 QL rewriting, every answer over a ruleset's closure is `sound-only`
  ([completeness](spec/reasoning-semantics.md#completeness)). Under `owl2-rl`, turn the
  rewriting on with `reasoner.ql_rewriting = "on"`. Under `owl2-dl`, raise the `dl.*` budgets
  where they leave answers `sound-only`, or ask `dl-answers=exact` to get an error instead.
- Over a ruleset's closure, an answer under `OPTIONAL`, `MINUS`, `EXISTS`, `NOT EXISTS`, an
  aggregate, or `LIMIT` or `OFFSET` after `ORDER BY` also says `sound-only`, though a missing
  inference can make one of its rows wrong there, and the status doesn't say so yet. Read such
  answers as `unsound`.
- User rules (Notation3 or `.pie`) have no built-ins but `log:equalTo` and
  `log:notEqualTo` and make no new blank nodes, and with user rules no answer is claimed
  complete ([user rules](spec/reasoning-semantics.md#user-rules-notation3)).
- Under `owl2-dl`, a commit whose consistency check isn't decided
  (a budget or the timeout) goes through, and the status says `unknown`. No answer is then
  complete ([consistency](spec/reasoning-semantics.md#consistency)). Raise `dl.timeout`
  and the `dl.*` budgets ([configuration](ops/config-reference.md#reasoner)).
- An explanation of what the rules derived (`/explain`) examines at most 4,096 statements.
  Outside `owl2-dl`, a derivation it doesn't find within them is answered 404, the same as for a
  statement that doesn't hold, so you can't tell the two apart. Justifications
  (`justifications=…`) say `complete: false` when a budget cut them short, and under `owl2-dl`
  a set of axioms past `dl.explanation_tests` or `dl.timeout` comes back unminimised, with
  `minimal: false`. Where OWL 2 DL doesn't decide it, the answer is a 422 that says why, not a
  404 ([HTTP interface](ops/http-api.md#engine-api-apiv1)).
- Bulk loads and the ontology file loaded at start-up skip the commit checks
  ([what a commit does](what-it-covers.md#what-a-commit-does),
  [storage](spec/02-storage-and-transactions.md#bulk-loads)), so neither SHACL nor the OWL 2 DL
  consistency check runs. To have the checks, load over HTTP (Graph Store), or validate after
  the load with `GET /dataset/shacl`.
- Under `owl2-dl`, a large `owl:sameAs` class is proven member by member, which can be slow. At a budget the answer
  is `sound-only` and partial (about 18 s for a class of 20,000 members, with 1,002 of 20,002 rows certain). Proofs
  per class are planned for v1.6. Raise `dl.timeout` and the `dl.*` budgets.

## SHACL

- SHACL Core and SHACL-SPARQL. Not supported: SHACL Advanced Features, and `sh:entailment`,
  which is reported as a failure ([HTTP interface](ops/http-api.md#shacl-validation)).
- The commit gate is off by default, and when it is on, it checks only what a commit introduces
  ([configuration](ops/config-reference.md)). To check data already stored, validate it
  with `GET /dataset/shacl`.

## Memory and stopping

- A memory limit bounds what requests reserve, not what the process holds, and the process can hold more than the
  limits add up to. No fixed factor bounds the difference.
  - The settings: one query, [`budgets.query_memory`](ops/config-reference.md#budgets.query_memory) (4 GiB by
    default); every running request together,
    [`budgets.total_query_memory`](ops/config-reference.md#budgets.total_query_memory) (50 % of the memory);
    both inside the process's, [`budgets.process_memory`](ops/config-reference.md#budgets.process_memory) (75 %).
    An embedded store has no separate query pool, and its queries charge the process's budget.
  - None of these is set aside for queries. The data, the text index, reasoning, the allocator's retained memory
    and other processes draw on the same memory, so leave room beside them.
  - Outside the limits: the full-text index, built by the first text search and kept; operators' working state
    (the keys of `OPTIONAL`, `ORDER BY`, `GROUP BY`, `MINUS` and `NOT EXISTS`, and the like) and a join's output
    while it is built; the working state of a cyclic count (triangles, four-cycles); and what reasoning allocates,
    which is watched rather than charged, so an overshoot is seen within 10 ms. The full list is on
    [contracts](architecture/contracts.md#memory).
  - A join may take half the room left when it starts. That is a reading, not a reservation: it is checked every
    1,024 rows, and joins running at once can read the same room.
  - A design that bounds the rest follows this release.
- A query can be refused while part of what it reserved is still unused, since reservations are taken ahead. That
  part has no stated bound in this release.
- Near the memory limit, a cached decoding can cause a refusal while memory the cache could give back is still
  held. The output is never wrong.
- A cancel, a deadline or a refusal takes effect at the work's next poll, which bounds work, not time. Under
  nested reads in reasoning, more work runs between two polls ([the bound](architecture/contracts.md#cancellation)).
  A stop never gives a partial or wrong answer.
- Some phases run to their end after a stop: the executor's selection and sorting (seconds at 10 M rows on a term
  key), one call of a regular expression (seconds for adversarial megabyte literals), and the graph kernel's
  sorts and set-up arrays.

## Storage and transactions

- One writer at a time, and a commit holds at most 4 GiB of log payload, about 130 M quads.
  Larger loads go through the bulk loader
  ([storage](spec/02-storage-and-transactions.md#limits)).
- Client transactions read the latest commit, and `SNAPSHOT` and `SERIALIZABLE` isolation are
  refused ([RDF4J protocol](ops/http-api.md#rdf4j-protocol)). A read, then a write
  that depends on it, fits in one SPARQL update (`DELETE … INSERT … WHERE`), which is one
  commit.
- A stop signal during start-up acts as a kill, and on Windows a service stop or
  `taskkill /F` ends the server without letting its requests finish. Acknowledged commits
  are kept either way ([stopping](ops/server-setup.md#stopping)). On Windows, stop it
  with ctrl-c in its console.
- A write that fails or is refused can leave its new terms in the dictionary, and the next commit publishes them,
  at most what its own request added ([dictionary continuity](adr/0002-engine-storage-lsm-permutations.md#implementation-notes-2026-09-25-e2e4)). Under
  the global search scope they show in text and IRI search, and under the reader's scope they don't
  ([`store.search_scope`](ops/config-reference.md#store.search_scope)). v1.6 removes them.
- A commit keeps every term it added, even one no statement uses (an `INSERT` and a `DELETE` of it in one
  transaction), and such terms behave like those of deleted statements. Reclaiming them is
  planned for v1.6.

## Access control

- There is no authentication by default (`auth.mode = none`), so set one before the server is
  reachable from other machines ([configuration](ops/config-reference.md#auth-modes)).
- Endpoints over the whole dataset (classification, SHACL validation, autocomplete) are
  only for readers of every graph
  ([access control](ops/http-api.md#graph-level-access-control)).
- A refusal for memory or work can hint at how much data a reader of only some graphs may not see. v2.0 admits
  requests by what the reader sees.
