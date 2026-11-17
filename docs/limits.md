# Limits and deviations

Where NRESE stops, and where it departs from a standard on purpose, in the commit these pages
come with. What is in is on
[features and standards](what-it-covers.md).

## Choose another system when

- You need several writers at once. One commit runs at a time, and reads never wait for
  it ([storage](spec/02-storage-and-transactions.md#writes)).
- You need automatic failover, or data spread over several machines. Read replicas
  follow a primary's log, but nothing promotes a replica or partitions the data
  ([replication](ops/replication.md)).
- You need reasoned answers on a replica that say whether rows may be missing. A replica's
  reasoned reads say `not-certified`, and a primary under `reasoner.equality = "compact"`
  serves no replica ([replication](ops/replication.md#limits-of-this-first-step)).
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

- A statement both asserted and derivable counts as asserted
  ([storage](spec/02-storage-and-transactions.md#data-model)).
- `rdfs` has no axiomatic triples, and `rdfs-full` leaves out `rdfD1` and the axioms about
  `rdf:_1`, `rdf:_2`, … ([rdfs](spec/reasoning-semantics.md#rdfs-a-6-rule-rdfs-subset),
  [modes](spec/reasoning-semantics.md#modes)).
- `owl2-rl` doesn't infer `x owl:sameAs x` for every term, so `?x owl:sameAs ?x` doesn't
  return one for each, though an asserted one is returned. It checks the datatype rules in part,
  and of the axiomatic triples it holds only `dt-type1`'s 32 datatype statements, in every store
  ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).
  Lists past their limits (a property chain of more than 250 properties, a key of more than
  127, an intersection of more than 256 classes, more than 64 member sequences) aren't used
  at all, never truncated, and each is reported in the log and the reasoning diagnostics;
  while one is left out, no answer that reads the inferences is `complete`
  ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)); none known beyond
  keeping lists within those limits. Literals aren't
  equated by value (`"1"^^xsd:integer` and `"01"^^xsd:integer` stay two terms): compare
  values in a `FILTER` (`?v = 1`), which SPARQL does by value. Otherwise none known.
- Without the OWL 2 QL rewriting, every answer over a
  ruleset's closure is `sound-only`. The rewriting runs by default under `owl2-ql` only;
  under `owl2-rl`, `reasoner.ql_rewriting = "on"` checks each query as under `owl2-ql`.
  Under `owl2-dl`, budgets and undecided candidates give `sound-only` (raise the `dl.*`
  budgets, or ask `dl-answers=exact` to get an error instead); candidates are decided only
  in a query that is one basic graph pattern with filters, so under a `UNION`, for one, they
  stay undecided. Where the class or property a read asks about may hold of every element
  (`owl:Thing` a subclass of it, a reflexive property), every name of the ontology may be an
  answer, a class's or property's name too, and the store returns only its individuals: such
  a read is `sound-only`, its reason naming the class or property. A query with two answer
  variables or more, or a path, is `sound-only` there without the check that could keep it
  `complete`. An operator that isn't
  monotone over answers that may be missing gives `unsound`
  ([what an answer promises](what-it-covers.md#what-an-answer-promises)). Under `owl2-dl`
  and `owl2-ql`, and under `owl2-rl` with the rewriting on, a read of the RDF, RDFS or OWL
  vocabulary (`?s rdfs:label ?l`), a variable predicate or class (`?x a ?class`) and
  `DESCRIBE` give `sound-only` at best
  ([what may be complete](spec/reasoning-semantics.md#what-may-be-complete)). In those
  modes, a query that matches a number
  or a date it names or computes against the data gives `sound-only` at best: the store
  matches it as written, so `"01"^^xsd:integer` doesn't find a stored `"1"`. Compare values
  in a `FILTER` instead
  ([what may be complete](spec/reasoning-semantics.md#what-may-be-complete)). Any value
  the query computes counts, so a computed string or IRI matched against the data
  (`BIND(CONCAT(…) AS ?n)`, `BIND(IRI(…) AS ?s)`) gives `sound-only` too, though nothing
  is missed. The same holds where a query joins, deduplicates, groups or counts the values of
  a property that holds numbers or dates (`unsound` under an aggregate): `"7"` and `"07"`
  are two terms to the store. A reader of only some graphs gets it wherever the query
  compares values, so their status says nothing about graphs they can't read. Compare values
  in a `FILTER` instead. A query with a triple term or a base-direction literal, or whose
  reader's graphs hold a triple term, is never `complete`: OWL 2 doesn't define them. For
  triple terms, none known
  ([what may be complete](spec/reasoning-semantics.md#what-may-be-complete)).
- A rebuild of the inferences (`POST …/reasoning/rematerialise`) that fails while it replaces
  them leaves them uncertified: answers are `not-certified` until a rebuild succeeds. A change of
  a repository's QL rewriting made in between takes effect with that rebuild, not at once. Run
  the rebuild again ([HTTP interface](ops/http-api.md#engine-api-apiv1)).
- User rules (Notation3 or `.pie`) have no built-ins but `log:equalTo` and
  `log:notEqualTo`, make no new blank nodes, and hold at most 256 atoms in a rule's
  premises or conclusion (split a longer rule); with user rules no answer is claimed
  complete ([user rules](spec/reasoning-semantics.md#user-rules-notation3)).
- Under `owl2-dl`, a commit whose consistency check isn't decided
  (a budget or the timeout) goes through, and the status says `unknown`. No answer is then
  complete ([consistency](spec/reasoning-semantics.md#consistency)). Raise `dl.timeout`
  and the `dl.*` budgets ([configuration](ops/config-reference.md#reasoner)).
- Under `owl2-dl`, the datatypes of the standard vocabularies outside the OWL 2 datatype map
  (`xsd:date`, `xsd:gYear`, `xsd:duration` and the like) have no values the checks read.
  Where such a value can decide a clash (under a range, a functional property or a
  cardinality, or compared), the consistency is `unknown` and no read is `complete`; a date
  nothing reads changes nothing. Use `xsd:dateTime`, which is in the map, where you can
  ([`owl2-dl`](spec/reasoning-semantics.md#owl2-dl-owl-2-dl-answers-with-their-completeness)).
- A value the store can't represent (an `xsd:dateTime` with a year beyond 10¹¹, say) is checked
  neither for a clash nor for being in its datatype, so a store that holds one is told consistent.
- Under `owl2-dl`, a key or a negative property assertion over a value the store's quick
  proof of consistency can't read (an `xsd:date` written two ways; also, stricter than needed,
  an integer beyond 128 bits or a string with a base direction) leaves the consistency
  `unknown`. Keys on strings and language-tagged strings are decided. Once the store met
  such a value, later commits are decided without that proof, by the slower checks, until
  the store is restarted or its inferences are rebuilt (`POST …/reasoning/rematerialise`).
- Under `owl2-dl`, a literal OWL 2 DL gives no meaning is told as the rules tell it where
  they can. An ill-typed literal of the map (`"abc"^^xsd:integer`) alone leaves the store
  consistent and its reads `complete`; under a range its commit is refused; where the full
  check must read its value (beside a `hasValue`), the consistency is `unknown`. A literal of
  a datatype the ontology names itself (`"x"^^:T`) is kept as written and never compared
  by value; where the full check must read it, the consistency is `unknown`. A literal typed
  `rdfs:Literal` is ill-typed: under any range, `rdfs:Literal` included, its commit is
  refused.
- Under `owl2-dl`, an ontology with annotation-property axioms (a domain, range or
  super-property of an annotation property, or another kind of property made its
  sub-property; `rdfs:label`, `rdfs:comment` and the other built-in ones count without a
  declaration) or a punned name (one IRI as a class or
  property and as an individual) leaves the rules-only route. The SHACL shapes graph is a
  graph of the store and is read too: a shape's `sh:targetClass ex:Animal` makes `ex:Animal`
  an individual as well as a class, a punned name. With annotation-property
  axioms every answer is `unsound`; with punning, every answer is `unsound` where an
  `owl:sameAs` reaches a punned name. Both are the whole ontology's, not the query's
  ([`owl2-dl`](spec/reasoning-semantics.md#owl2-dl-owl-2-dl-answers-with-their-completeness)).
  Remove such axioms, give the individual its own IRI, or use `owl2-rl`, whose promise
  is the rules' reading.
- Under `owl2-dl`, where the OWL 2 RL rules find a clash in such an ontology, this release
  doesn't decide whether the data is consistent under OWL 2 DL: the commit is refused,
  saying so, and data loaded with such a clash is `undecided` and quarantined, with the
  reason in `/dataset/info` (`consistency_reason`). The check is the whole ontology's, so a
  real clash beside an unrelated punned name reads `undecided` too, and a DL engine's later
  verdict at a query doesn't change readiness. In quarantine, each commit rebuilds the
  inferences in full, and under `owl2-dl` reads the asserted ontology once more to tell
  `undecided` from `inconsistent`. Remove the statements behind the clash, remove the
  annotation property's domain, range, super-property or sub-property, or give the individual its own
  IRI. Under `owl2-rl`, which takes the rules' reading, the clash is an inconsistency.
- Labels and notes on a store with `owl:sameAs` read as `unsound`. Under `owl2-dl`, and under `owl2-ql`
  and `owl2-rl` where the QL rewriting answers, a query that reads an annotation property
  (`rdfs:label`, `rdfs:comment`, or one the data declares) or any predicate (a variable
  predicate, `DESCRIBE`) is `unsound` where the store holds `owl:sameAs` between two different
  terms, or an inferred annotation statement: the rules copy annotations across equal names, and
  OWL 2 DL doesn't. The copies are still among the rows. Read the annotations as stated with
  `infer=false`; under a ruleset's own closure without the rewriting the same reads stay
  `sound-only`, since there the copies are right
  ([the rule](spec/reasoning-semantics.md#annotation-copies)).
- Under `owl2-dl`, an ontology with a part the reader leaves out is never told consistent:
  every verdict but an inconsistency is `unknown`. Where that part is one the OWL 2 RL rules
  may still read (a reserved name used as a class or individual, OWL's vocabulary outside the
  mapping's forms, a malformed structure or list such as `owl:hasKey ()`, a key without a
  property, syntax not read yet), every answer is
  `unsound` too. The reason gives how many parts were left out and names the first in words,
  with its terms as the data writes them (a blank node by a statement next to it, a triple
  term as `_`; a read that finds the part itself names no term). Only the first is named, and
  no log or endpoint lists the others. A read inside a transaction is told what
  was left out of the revision it began on, so an addition still pending that would be left
  out may be told `sound-only` until it is committed
  ([`owl2-dl`](spec/reasoning-semantics.md#owl2-dl-owl-2-dl-answers-with-their-completeness)).
  Remove such statements, or use `owl2-rl`, whose promise is the rules' reading.
- Under `owl2-dl`, `owl:Thing`, `owl:Nothing` and the four top and bottom properties where an
  individual goes (`:a :p owl:Thing`) aren't noticed. OWL 2 DL allows no reserved name as an
  individual, and any other one stated so is a part the reader leaves out, as above; these six
  are not, so a read that returns one (with `:p rdfs:range :C`, `SELECT ?x { ?x a :C }`
  returns `owl:Thing`) can be told `complete`. Don't state them as individuals.
- Under `owl2-dl`, a variable the query doesn't return is read existentially: it may stand
  for an element that has no name. The SPARQL entailment regime binds every variable of the
  pattern to a name of the ontology, so the store may return a row the regime doesn't
  ([`owl2-dl`](spec/reasoning-semantics.md#owl2-dl-owl-2-dl-answers-with-their-completeness)).
- An undeclared property used with literal values is read as a data property. Declare
  annotation properties other than the built-in ones, such as `skos:prefLabel`
  (`skos:prefLabel a owl:AnnotationProperty`);
  otherwise a labelled class reads as punned, and under `owl2-dl` it leaves the rules-only
  route. Nothing in this release shows which properties were typed by their use; this
  query lists the ones used with literals and given no declaration:

  ```sparql
  PREFIX owl: <http://www.w3.org/2002/07/owl#>
  SELECT DISTINCT ?property WHERE {
    ?subject ?property ?value .
    FILTER(isLiteral(?value))
    FILTER NOT EXISTS {
      VALUES ?kind { owl:DatatypeProperty owl:ObjectProperty owl:AnnotationProperty }
      ?property a ?kind .
    }
    FILTER(!STRSTARTS(STR(?property), "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        && !STRSTARTS(STR(?property), "http://www.w3.org/2000/01/rdf-schema#")
        && !STRSTARTS(STR(?property), STR(owl:)))
  }
  ```
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
  is `sound-only` and partial. Proofs per class are planned for v1.6. Raise `dl.timeout` and the `dl.*` budgets.
- Under `owl2-dl`, a read whose candidates the exact services must each refute (all members of
  `:H` where `owl:Thing` is a subclass of `:H` or `:K`, say) grows steeply with the store, as
  each candidate's test reads every individual: measured on one machine, about 18 ms at 100
  members, 0.3 s at 300 and 10 s at 1,000. [`dl.max_candidates`](ops/config-reference.md#dl.max_candidates)
  (1,000) bounds how many are tested, not what each test costs; the rest are left out of the
  answer, which is `sound-only` and names the setting, as it does at
  [`dl.timeout`](ops/config-reference.md#dl.timeout) (30 min). On a server the query's timeout
  ([`budgets.query_timeout`](ops/config-reference.md#budgets.query_timeout), 30 s) comes first:
  the answer is a `408` with no rows and no status, and nothing in it says that the OWL 2 DL
  checks took the time or which setting to raise. Raise the query timeout for such a read, or
  ask `dl-answers=sound`, whose answers skip those checks and are `sound-only`.

## SHACL

- The commit gate is off by default, and when it is on, it checks only what a commit introduces
  ([configuration](ops/config-reference.md)). To check data already stored, validate it
  with `GET /dataset/shacl`.
- A rebuild of the inferences passes no gate: a change of the rules, the rebuild at a start, the
  one after a load or an import, and one asked for (`POST …/reasoning/rematerialise`) store
  their inferences without checking them against the shapes, and
  later commits are checked only for what they introduce. Validate with `GET /dataset/shacl`
  after one.
- With no shapes stored, a gate set to `enforce` accepts every write. An upload of shapes that
  fails (one that runs past its timeout, say) stores none, so check its status and read the
  shapes graph back.
- A commit that changes the shapes is checked against the shapes as they are after it: one
  update that removes the shapes and adds data is checked against none. It needs the right to
  write the shapes graph.
- Shapes that don't compile refuse every write, and a validation stopped by its budget fails
  the write, with the gate set to `report` as to `enforce`.

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
  - A checkpoint's working memory, a background one's and the one a bulk load commits by, counts
    against no limit, not even `budgets.process_memory`. It grows with the whole dictionary, about
    17 to 23 bytes per term: about 720 MiB for DBpedia core's 40.5 million terms. Leave room for it.
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

- A commit holds at most 4 GiB of log payload, about 130 M quads.
  Larger loads go through the bulk loader
  ([storage](spec/02-storage-and-transactions.md#limits)).
- A client transaction keeps its operations, not their effects, and runs them again at its
  commit ([RDF4J protocol](ops/http-api.md#rdf4j-protocol)). An update with a `WHERE`, a
  `CLEAR` or `DROP`, or a removal by pattern matches what another client committed meanwhile; a
  value an update generates (`UUID()`, `RAND()`, `NOW()`, `BNODE()`, blank nodes) and read inside
  the transaction isn't the one committed; a malformed update fails only at the next read or at
  the commit; and a failed commit ends the transaction. Reads take the latest commit, and
  `SNAPSHOT` and `SERIALIZABLE` isolation are refused. A read, then a write
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
- A point-in-time restore doesn't cross a bulk load or a rematerialisation: they write a
  checkpoint, not a log record, so it needs an image taken after them. A restore by time can't
  tell a complete log from one missing its end, and replays log records without a commit time
  whatever the time; compare the revision it reports with the one you expect
  ([point-in-time restore](ops/backups.md#point-in-time-restore)).

## Access control

- There is no authentication by default (`auth.mode = none`), so set one before the server is
  reachable from other machines ([configuration](ops/config-reference.md#auth-modes)).
- Endpoints over the whole dataset (classification, SHACL validation, autocomplete) are
  only for readers of every graph
  ([access control](ops/http-api.md#graph-level-access-control)).
- A reader of some graphs who sees inferred statements by support (`inferred = "supported"`) is
  told `not-certified` for every read at a revision where a statement it can derive is also
  asserted in a graph it can't read. Its answers are right, and so is the status, but the status
  tells that reader that such an assertion exists.
- A refusal for memory or work can hint at how much data a reader of only some graphs may not see. v2.0 admits
  requests by what the reader sees.
