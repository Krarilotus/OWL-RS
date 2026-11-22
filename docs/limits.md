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
  inferences go to the default graph ([graph scope](spec/reasoning-semantics.md#graph-scope)); a
  query can take its inferences from its own graphs alone
  ([`inference-scope=dataset`](ops/http-api.md#inference-scope)), at a cost (below).
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
- A full-text search in the same group as a spatial relation with a fixed side
  (`{ ?g geo:sfWithin ex:box . ?g text:query "needle" }`) can miss matches: the search is read
  there as a stored statement. Planned for v1.5.1.
- A full-text search joined with a vector search (`SERVICE nrv:search`) can miss its matches
  when the vector search runs first over many candidates: the search is then read as a stored
  statement. Planned for v1.5.1.
- Two groups written side by side, `{ { … } { … } }`, are read as one group, so a full-text
  search and the other group's triples are taken together. For one,
  `SELECT ?o ?r { { ?o bds:search "alpha" } { ?o bds:relevance ?r } }` applies the second
  group's option to the search and gives each match with its relevance, where the groups as
  written give no row (with no `bds:relevance` statement stored). Planned for v1.5.1.
- With computed spatial relations, `SELECT DISTINCT` over a spatial relation joined with other
  triples (`SELECT DISTINCT ?g { ?g geo:sfWithin ?box . ?g a ex:Selected }`) can return only the
  relations stated in the data and miss the computed ones. Planned for v1.5.1.
- Under `SELECT DISTINCT`, a vector search (`SERVICE nrv:search`) whose query vector comes from
  outside the `SERVICE` (a `VALUES`, say) can fail with an argument error: the vector isn't
  passed in. Planned for v1.5.1.
- A named graph without statements doesn't exist. `CREATE GRAPH` stores nothing, and an
  emptied graph answers 404 ([Graph Store Protocol](ops/http-api.md#graph-store-protocol)).
  Read that 404 as an empty graph.
- A reader of only some graphs gets no plan steps in `EXPLAIN`.
- A part of a query that draws a value at each evaluation (`RAND()`, `UUID()`, `STRUUID()`,
  `BNODE()`) is evaluated as follows, and gives up some faster plans for it, so it costs more
  than one that draws nothing:
  - an `EXISTS` that draws is evaluated once per tested row, and in an `OPTIONAL`'s condition
    once per compatible pair: its cost grows with the rows, not with their distinct values;
  - a `LATERAL` right side that draws is evaluated once per left row;
  - a `SERVICE` pattern that draws is sent once per endpoint it resolves to, per evaluation of
    the `SERVICE`, without the joined rows' values, and joined here;
  - a group that draws, beside a vector search, is evaluated first: the search doesn't go
    first. Where every row of the group binds the matched vector, the search ranks only those
    vectors; otherwise it ranks every vector of the active graph;
  - a `GROUP BY` with an `AVG` over a drawn value is never computed in parts: where its rows
    don't fit the query's memory, the query is refused.
- In a plan that passes rows sideways into a group, a group that draws is evaluated from the
  distinct values of the rows joined to it, not from the rows themselves, which costs a little
  more than a group that draws nothing. Where those rows share no variable the group binds in
  every solution, or leave such a variable unbound in a row, the group is evaluated alone and
  then joined: a `FILTER(RAND() < …)` over a `UNION` in which only some arms bind the joined
  variable reads all of those arms' data, not only the rows' values.
  The answer is right, but slower, or refused under a tight memory budget.
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
  and seeds the closure with `dt-type1`'s 32 datatype declarations (`rdf:PlainLiteral rdf:type rdfs:Datatype` and the like),
  the annotation property declarations from `prp-ap`, and the class declarations from `cls-thing` and `cls-nothing1`
  (see [when these seeds are published](spec/reasoning-semantics.md#omitted-and-what-that-means)).
  It doesn't type a literal by its own datatype (`dt-type2`), nor do `owl-horst` and
  `rdfs-plus`: without a range, `∃:p.xsd:integer ⊑ :C` and `:a :p 5` give no `:a a :C`, even in
  one commit, where `owl2-dl` gives it
  ([omissions](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).
  The reader follows every accepting walk to `rdf:nil`, including through cycles. A head
  with no such walk gives no rules. The diagnostic can make reads over the inferences
  incomplete. A write that changes whether a head has an accepting walk is rebuilt after
  incremental maintenance ([list limits](spec/reasoning-semantics.md#list-limits)).
  Lists whose walks exceed the encoder's bounds use rules over individual cells.
  The walk search has a deterministic work bound and uses the cell form when it
  reaches that bound. Operations with an existing reasoner stop also use it while
  compiling and evaluating lists.
  An adversarially branched head can reach this bound and use the cell form while giving
  the same answers.
  With QL rewriting on, a list outside OWL 2 DL's RDF syntax can still make a read
  `sound-only` through the structural reader's `BrokenList` hazard.
  Literals aren't
  equated by value (`"1"^^xsd:integer` and `"01"^^xsd:integer` stay two terms): compare
  values in a `FILTER` (`?v = 1`), which SPARQL does by value. Otherwise none known.
- Under `owl2-rl` with the QL rewriting on, a committed read over a left-out list axiom may be
  `unsound` under `NOT EXISTS`, `MINUS`, `OPTIONAL` or another operator
  [the rule](spec/reasoning-semantics.md#what-may-be-complete) names. The query result, plan
  and EXPLAIN report that status. Lists with accepting walks use their rule conclusions
  at any length.
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
- Under `owl2-ql`, and under `owl2-rl` with the QL rewriting on, a read that asks only about a
  predicate or class that appears nowhere in the store can be told `false` and `complete` on
  data that are inconsistent under OWL 2 DL in a way the rules don't read (a cardinality
  written in another spelling than the mapping's `"0"` or `"1"^^xsd:nonNegativeInteger`,
  say). On consistent data such a read is right: nothing entails a statement of a name that
  appears nowhere.
- Under `owl2-ql`, a store that asserts an individual into a class expression
  (`:x a [ owl:intersectionOf ( :A :B ) ]`, or a cardinality restriction) gets no `complete`
  answer, since what follows from that assertion may be missing (`:x a :A` answers `false`):
  OWL 2 QL allows only a named class there. Under `owl2-rl` with the QL rewriting on, the same
  holds for every expression but those OWL 2 RL allows on the right of a subclass axiom (an
  intersection of such, `allValuesFrom`, `hasValue`, a complement, an at-most of one), which
  the rules apply. An inclusion into an intersection
  (`:C rdfs:subClassOf [ owl:intersectionOf ( :A :B ) ]`), which OWL 2 QL allows, is answered
  and `complete`.
- Under `owl2-ql`, an ontology with an axiom the OWL reader leaves out or alters (a reserved
  name used as a class, property or individual, a structure it can't read, a property declared
  as two kinds, SWRL rules) gets no `complete` answer from the rewriting: the reads it answers
  are told `sound-only` at best, with the reader's reason, since conclusions of that axiom may
  be missing. For one: with `:A rdfs:subClassOf [ owl:intersectionOf ( rdf:List :C ) ] .
  :a a :A`, `SELECT ?x { ?x a :C }` returns no row, told `sound-only`, the reason naming
  `rdf:List` as reserved vocabulary used as an entity; under the Direct Semantics `:a` is a
  `:C`. With `:D` in place of `rdf:List`, the same read returns `:a`, `complete`.
- Under `owl2-rl` with `reasoner.ql_rewriting = "on"`, a universal restriction on
  `owl:topObjectProperty` puts every individual into its filler, which the rules don't derive:
  the read answers `false` and is told `complete`. For one:
  `:x a [ owl:onProperty owl:topObjectProperty ; owl:allValuesFrom :D ] . :y a :Y`, asked
  `:y a :D`. At the defaults it isn't told `complete`.
- Under `owl2-ql`, a read that lists every partner of a top property, or of a property
  equivalent to, above, or inverse to one, isn't told `complete`: every individual is a
  partner, and the rewriting doesn't list them. For one: with
  `:q owl:equivalentProperty owl:topObjectProperty`, `SELECT ?y { :a :q ?y }`. The same holds
  for listing the members of a top property's domain or range, `SELECT ?x { ?x a :B }`.
  Asking whether such a pair exists is answered for a property equivalent to, above, or
  inverse to a top property, and whether an individual is such a member is answered; asked
  of the top property itself, it isn't told `complete`.
- Under `owl2-ql`, and under `owl2-rl` with the rewriting on, a store inconsistent only through
  an existential the rewriting gives isn't found inconsistent: a read every inconsistent store
  entails answers `false` and is told `complete`. For one: `:A ⊑ ∃:p.:B`, `:N ⊑ ¬∃:p.:B`,
  `:x a :A, :N`, asked `:x a :D`.
- Under `owl2-ql`, a contradiction over data values by axioms outside OWL 2 QL is accepted, and
  the store reports itself consistent, with reads of unrelated terms told `complete`:
  `:C rdfs:subClassOf [ owl:onProperty :p ; owl:allValuesFrom xsd:integer ]` and `:a :p "x"`,
  then `:a a :C`; or a `hasValue` of a literal outside the property's datatype range. `owl2-rl`
  and `owl2-dl` refuse both. A plain datatype range with a literal outside it is refused in
  every mode.
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
- Under `reasoner.equality = "compact"`, four cases are checked less than under the default
  (`representatives`) and `replicate`. Where they matter, use the default
  ([`reasoner.equality`](ops/config-reference.md#reasoner.equality)).
  - A commit that makes two different data values the same can be accepted where one value
    reached its `owl:sameAs` class through an earlier commit that merged two individuals (one of
    them declared `owl:sameAs` a literal). The other settings refuse that commit, and a full
    rebuild of the inferences finds the clash in every setting.
  - A full rebuild doesn't check a literal against a datatype it reaches only through an
    `owl:sameAs` alias of the datatype's IRI, so it finds such data consistent. Commits refuse
    that data in every setting, and the other settings' rebuilds find it.
  - After a full rebuild, a key whose list is reached through `owl:sameAs rdf:nil`
    (`:C owl:hasKey :l . :l owl:sameAs rdf:nil`) doesn't make two instances of `:C` the same,
    where a commit, and `owl:hasKey rdf:nil` written directly, do in every setting.
  - A write of an `owl:sameAs` statement, between two properties or two individuals, can pass a
    SHACL gate set to `enforce` though it brings a result: the equality is expanded when the data
    are read, so the gate starts from that statement alone. An insert of the statement that
    breaks the shape is refused in every setting. After such writes, validate in full
    (`GET /dataset/shacl`).
- A store whose ontology uses an inverse property expression (`ObjectInverseOf`, written
  `_:x owl:inverseOf :p`) rebuilds its inferences at every commit that changes its data, so a
  commit's time grows with the store: at 100,000 individuals, a one-fact commit takes about 40
  to 55 ms instead of 0.3 to 0.4 ms. Stores with named inverses only (`:q owl:inverseOf :p`)
  are unchanged.
- Such a store decides a commit's consistency without the facts the inverse gave in earlier
  commits. A clash only they reach is not refused, and a clash only they held is refused, until
  the next commit. After an accepted clash of that kind the store still reports itself
  consistent; a rebuild of the inferences finds it.
- A blank node used as a property otherwise than as an inverse expression
  (`_:b a owl:ReflexiveProperty`, `:p rdfs:subPropertyOf _:b`, an equality that copies a
  predicate onto a blank node) gives facts no store holds; a read after a later commit may miss
  them. Such input is outside OWL 2 DL.
- A literal value under an inverse, symmetric or functional object property gives a fact no
  store holds, with the literal as its subject. A read after a later commit may miss what
  follows from it, and `owl2-ql` and `owl2-dl` still tell it `complete`; `owl2-rl` doesn't. For
  one: `:p owl:inverseOf :q . :a :p "v"`, then `:q rdfs:range :D`, asked `:a a :D`. That input
  is outside OWL 2 DL. A literal typed by a range is such a fact too, in valid OWL 2 DL: read
  back by `someValuesFrom` that datatype in a later commit (`:p rdfs:range xsd:integer .
  :a :p 5`, then `∃:p.xsd:integer ⊑ :C`, asked `:a a :C`), `owl2-rl` and `owl-horst` miss it
  until a rebuild, and don't tell it `complete`; `owl2-dl` answers it. Under `owl2-rl`, a later
  commit's consistency check misses a clash through such a fact too: `:p owl:inverseOf :q .
  :x :p "bad"`, then `:q rdfs:domain xsd:integer`, is accepted and the store says it is
  consistent, where the same statements in one commit are refused; a rebuild of the inferences
  finds the violation. The earlier `main` does the same.
- Where a functional data property holds one value written in two lexical forms on one subject
  (`:p a owl:FunctionalProperty . :a :p "1"^^xsd:integer , "1.0"^^xsd:decimal`), a first load
  and a rebuild of the inferences give a later statement that uses one form the other form too,
  and an incremental write doesn't: a store's rows can differ from its own rebuild. Under compact
  equality, for a later statement in the integer form, a first load and a rebuild leave out the
  other form too. The earlier `main` does the same ([why](spec/reasoning-semantics.md#generalised-triples)).
- <a id="two-spellings-across-commits"></a>Under `owl2-dl`, a functional data property (or an
  at-most-one restriction) that holds one value written two ways (`"1"^^xsd:int`,
  `"01"^^xsd:int`) equates the two literals within a commit, but not across commits: after a
  later commit, a read of another property's values may return one spelling where the same data
  in one commit return both, and it is still told `complete`. The data are valid, consistent
  OWL 2 DL, so here a `complete` answer can be wrong. The missing row is the same value in its
  other spelling: a count is off by such duplicates, and a `FILTER` on the spelling (`STR`) can
  differ, while a `FILTER` on the value (`?v = 1`) finds it either way. A rebuild of the
  inferences (`POST …/reasoning/rematerialise`) restores it.
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
  declaration), a punned name (one IRI as a class or
  property and as an individual), or an empty intersection or union (`owl:intersectionOf
  rdf:nil`, used or not) leaves the rules-only route: reads that were `complete` by the rules
  alone lose that or take a slower way. The SHACL shapes graph is a
  graph of the store and is read too: a shape's `sh:targetClass ex:Animal` makes `ex:Animal`
  an individual as well as a class, a punned name. With annotation-property
  axioms every answer is `unsound`; with punning, every answer is `unsound` where an
  `owl:sameAs` reaches a punned name. Each is the whole ontology's, not the query's
  ([`owl2-dl`](spec/reasoning-semantics.md#owl2-dl-owl-2-dl-answers-with-their-completeness)).
  Remove such axioms, give the individual its own IRI, or use `owl2-rl`, whose promise
  is the rules' reading.
- Under `owl2-dl`, a name used as two kinds OWL 2 DL keeps apart (an object and a data
  property, either and an annotation property, a class and a datatype) leaves the rules-only
  route where an axiom other than an assertion names it: every answer is `unsound` and the
  consistency `unknown`. So does a structure no axiom uses that the rules may read more of
  (a blank node with two forms, a negative assertion's statements without its type, or one
  that is also an individual): every answer is `unsound`, the consistency decided. A name
  only assertions use as two kinds (`dc:creator "N"` beside `dc:creator :n`) keeps the route.
  Declare each name as one kind, or remove the unused structure.
- Under `owl2-dl`, where the OWL 2 RL rules find a clash in such an ontology, this release
  doesn't decide whether the data is consistent under OWL 2 DL: the commit is refused,
  saying so, and data loaded with such a clash is `undecided` and quarantined, with the
  reason in `/dataset/info` (`consistency_reason`). The check is the whole ontology's, so a
  real clash beside an unrelated punned name or empty list reads `undecided` too, and a DL engine's later
  verdict at a query doesn't change readiness. In quarantine, each commit rebuilds the
  inferences in full, and under `owl2-dl` reads the asserted ontology once more to tell
  `undecided` from `inconsistent`. Remove the statements behind the clash, remove the
  annotation property's domain, range, super-property or sub-property or the empty list, give
  the individual its own IRI, remove the unused structure, or declare each name as one kind.
  Under `owl2-rl`, which takes the rules' reading, the clash is an inconsistency.
- On a store in quarantine, a commit can be refused as introducing a violation that was
  already there, though a full rebuild finds the same violation after it (under `owl2-rl`, for
  one): a commit that deletes a statement the remaining statements still entail, that deletes a
  schema statement they derive again, or that edits the list of an axiom together with other
  statements. The commit changes nothing, and the store stays as it was. Repair the existing
  violation first; the commit is then accepted.
- A write maintained by a rebuild of the inferences (a split of an `owl:sameAs` class under
  compact equality, a delete the maintenance rebuilds, a reasoning change on a store with an
  inverse property expression, a store's first insert) is judged by the rebuild's own violations
  while the store is marked consistent. The rebuild reads the whole store, so a clash that was
  there before, and that an earlier check missed, refuses such a write too. On a store already
  marked inconsistent such a write is judged by the incremental check, as other writes are.
- A store holding `owl:sameAs` on OWL's own vocabulary isn't OWL 2 DL. A rebuild of it can miss
  a clash the equality hides, and a write that removes the equality is then refused with the
  clash it reveals.
- In the cases run under `owl2-rl`, data that makes `rdf:rest` or `rdf:nil` `owl:sameAs` a term
  of its own misses a property chain's inference; the read is told `sound-only`, as every read
  over the `owl2-rl` closure is. `:out owl:propertyChainAxiom ( :p :q )` with
  `:a :p :b . :b :q :z` isn't applied when its list goes on through a name made equal to
  `rdf:rest` (the list written with
  `:r`, then `rdf:rest owl:sameAs :r`; under the default equality setting) or ends in a name made
  equal to `rdf:nil` (under compact equality): `ASK { :a :out :z }` stays `false`. The earlier
  `main` infers both, with no status. In another case run, a key written with a property made
  equal to `owl:hasKey` (under compact equality), it is the earlier `main` that misses the
  equality this release infers. Don't make the standard vocabulary equal to terms of your own.
- A write refused by a list rule (`eq-diff2` of an `owl:AllDifferent`, `cax-adc` of an
  `owl:AllDisjointClasses`) can be named by the rule and an individual, or by the rule alone with
  no member, without the statements behind it.
- A store opened as a library, without the server, keeps the consistency verdict an earlier
  version recorded for its reads until its first reasoned write; the server checks it again at
  its start.
- A disjointness that names an operand twice (`ex:C owl:disjointUnionOf ( ex:A ex:A )`, or a
  class twice in `owl:AllDisjointClasses`) is read by position, as OWL 2's RDF-Based Semantics
  reads it, and as the OWL 2 RL rules read the other disjointness axioms: the repeated operand is
  empty. Under the Direct Semantics the
  operands are a set, so an ontology consistent under that reading can be refused here; the
  reader's report names the repeat. Under `owl2-dl` such a disjoint union takes the ontology off
  the rules' route, and the DL check decides it.
- A negative property assertion through an inverse property, committed after the statement it
  contradicts, isn't caught by the OWL 2 RL rules' incremental check.
- Under `owl2-dl`, an OWL 2 RL clash in an ontology with an empty intersection or union is
  stated as undecided, not as an inconsistency.
- Under `owl2-dl`, the status an answer is told (`complete`, `sound-only`) is a promise for data
  that is an OWL 2 DL ontology, and this release doesn't yet check all of OWL 2 DL's conditions
  on the data. Data that breaks one it doesn't check isn't refused for it, and the OWL 2 RL
  rules still read it, so an answer can be told `complete` or `sound-only` where OWL 2 DL
  doesn't entail it. Remove structures the OWL reader leaves unused and use each name as one
  kind only, declared as that kind, or use `owl2-rl`, whose promise is the rules' reading. Two
  kinds are known, and other structures and names of these kinds can do the same:
  - A structure the OWL reader can't read as OWL 2 DL and leaves unused, such as a blank node
    that no axiom uses but that carries two class structures at once. With its classes and its
    property declared, and with
    `_:x owl:intersectionOf ( :A :B ) ; owl:onProperty :p ; owl:allValuesFrom :D` and
    `:x a :A , :B . :x :p :y`, `ASK { :y a :D }` is `true`, told `complete`; the earlier `main`
    gives the same answer, with no status. An `owl:hasValue` beside an `owl:allValuesFrom` does
    the same.
  - A name used as two kinds of entity, such as a property never declared and used both with a
    literal and with a resource. With `:C a owl:Class . :p rdfs:domain :C . :a :p "x" .
    :b :p :c`, `ASK { :a a :C }` is `true`, told `complete`; the earlier `main` gives the same
    answer, with no status. An annotation property also declared functional can make two
    names equal, told `sound-only`.
- Labels and notes on a store with `owl:sameAs` read as `unsound`. Under `owl2-dl`, and under `owl2-ql`
  and `owl2-rl` where the QL rewriting answers, a query that reads an annotation property
  (`rdfs:label`, `rdfs:comment`, or one the data declares) or any predicate (a variable
  predicate, `DESCRIBE`) is `unsound` where the store holds `owl:sameAs` between two different
  terms, or an inferred annotation statement: the rules copy annotations across equal names, and
  OWL 2 DL doesn't. The same holds for statements of a SHACL property (`sh:path`, say) where the
  store holds such an equality: SHACL content has no OWL meaning either. The copies are still
  among the rows. Read the annotations as stated with `infer=false`; under a ruleset's own
  closure without the rewriting the same reads stay `sound-only`, since there the copies are right
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
  where its answer can still be written in time (below), the read returns the rows it has,
  `sound-only` with the bounds' counts, its untested
  candidates unresolved, and the reason names `dl.timeout` though it was the query's timeout
  that passed. Under `dl-answers=exact` it is refused, as at any limit. Raise the query timeout
  for such a read, or ask `dl-answers=sound`, whose answers skip those checks and are
  `sound-only`. The store stops 300 ms before the query timeout, to write the answer in time: a
  read with many rows in hand, or one whose lower or upper evaluation or shared view is still
  being prepared then, can still end in `408`, and a query timeout of 300 ms or less leaves the checks no time: a read that needs
  them is told incomplete from its start.
- With [`dl.model_labels`](ops/config-reference.md#dl.model_labels) on, a read whose request has
  no room for the labels' work gets no refutations from them and is decided by its own tests.
  Candidates past the first rows of the read without the labels go to their tests in batches of
  no more decoded bytes than that read's costliest. Where the request has no room to admit such
  a batch, or one row
  alone costs more, the tests end there: the rest are left unresolved, and the read is told
  incomplete, with the request's memory as the reason, or for such a row "the next candidate's
  row costs more than … bytes". No later, cheaper row is tried.
- Under `owl2-dl`, the reasoning a read's answer needs first (the bounds over the data, which
  every `owl2-dl` read waits for) runs on past the query timeout, until it ends or until
  [`dl.timeout`](ops/config-reference.md#dl.timeout) (30 min), on the server's shared workers:
  the work can outlive the read that began it, and the store keeps the bounds for later reads.
  Meanwhile reads on other repositories of the same server can slow down or time out, and
  further `owl2-dl` reads on that repository wait for the same work until their own deadline,
  then answer over the OWL 2 RL closure, saying why, or under `dl-answers=exact` are refused
  (such an answer can still end in `408` where it waits for the ontology to be read). It is a
  wait, not a lock: when the work ends, the other reads are at their
  usual speed at once. Measured on one machine (10 workers, bounds that take 59 s to build,
  a query timeout of 5 s): for about 30 s after the `408` a read on another repository that
  takes 8 ms idle timed out or took up to 4.3 s; with `dl.timeout` at 10 s the other reads
  were normal again within 5 to 10 s. Set `dl.timeout` a little above the query timeout: a
  store whose reasoning needs longer then never gets it, so its `owl2-dl` answers stay
  `sound-only`, and each such read spends up to `dl.timeout` again.
  [`dl.threads`](ops/config-reference.md#dl.threads) doesn't confine this work. A store with
  heavy `owl2-dl` data on a server of its own avoids it.
- <a id="inference-scoped-to-a-dataset"></a>A read with `inference-scope=dataset` is never certified
  (`not-certified`). The first such read at a revision costs like computing the support graph
  sets for the whole store, not for its graphs. Measured once on one machine, in an optimised
  build at `918b1bd75`, on a generated store of 64 graphs with 1,000,000 asserted and 2,000,029
  inferred statements under OWL 2 RL, held in memory, with the result cache off and 9 GiB of
  query memory: the first scoped read of a revision took 23.4 s and 822 MB of query memory; the
  same read again 204 ms and 10.5 MB; a read of another set of graphs 802 ms and 111 MB; the
  read after a commit of one statement 215 ms, after one of 1,000 statements 415 ms. Their memory comes from the shared query budget, and
  they aren't kept across a restart: with `inferred = "supported"` in the access policy they
  are computed in the background at start, else the first scoped read after one computes them.
  A commit updates them for what it changes; a bulk load, a rebuild of the inferences or a
  schema change computes them again in full.
- With `inference-scope=dataset`, text search, vector search, reader-ranked search and computed
  spatial relations are refused (`501`), wherever they stand in the query, also in a part that
  has no rows to run on: the refusal is decided before anything is evaluated, and for a pattern
  an `EXISTS` or a `LATERAL` builds from an outer binding, when it is built. A `SERVICE` body is
  answered by its endpoint.
- An OWL list (of an intersection, a union, a property chain or a key, say) is read over the
  whole store. A read that sees inferred statements by support still needs access to the
  statements of at least one derivation, including its list premises.
- A query whose dataset is named (`FROM`, or `default-graph-uri`) without
  `inference-scope=dataset` reads the asserted statements of those graphs and none of the
  inferred ones, which are in the default graph. Under `owl2-rl` its status is `sound-only`
  with the reason of a read over the closure, which doesn't say that the inferred statements
  are outside its dataset. For what follows from those graphs, add `inference-scope=dataset`.
- <a id="a-list-past-the-cap"></a>The 64-walk bound selects a rule form.
  Cyclic lists with exits and lists past the encoder bounds contribute
  the conclusions of their accepting walks during ordinary incremental maintenance
  ([list limits](spec/reasoning-semantics.md#list-limits)).

## SHACL

- The commit gate is off by default, and when it is on, it checks only what a commit introduces
  ([configuration](ops/config-reference.md)). To check data already stored, validate it
  with `GET /dataset/shacl`.
- The gate counts the affected nodes it holds across nested shapes against the write's memory
  budget. A commit that exceeds the budget is refused without applying the write, even in
  `report` mode. This can refuse commits that previously succeeded while holding more memory
  than the budget allowed. To give the commit more room, raise
  [`budgets.query_memory`](ops/config-reference.md#budgets.query_memory)
  (`NRESE_MAX_QUERY_MEMORY_BYTES`).
- A rebuild of the inferences passes no gate: a change of the rules, the rebuild at a start, the
  one after a load or an import, and one asked for (`POST …/reasoning/rematerialise`) store
  their inferences without checking them against the shapes, and
  later commits are checked only for what they introduce. Validate with `GET /dataset/shacl`
  after one.
- With no shapes stored, a gate set to `enforce` accepts every write. An upload of shapes that
  fails (one that runs past its timeout, say) stores none. The repository's info says which
  shapes are in force (`GET /dataset/info`, `shacl`: their number and `vacuous`), and the log
  warns at the start while a gate that is on checks nothing.
- The info reports the count for its dataset revision and the revision that last changed
  the asserted shapes graph. A bulk load or replica batch can publish new shapes before
  their count is ready. The info then gives `null` for `shapes` and `vacuous`, with the
  reason in `not_counted`. Cancelling the count leaves the published data in place.
  A later load, replica batch or restart can retry a count deferred by a stop or the
  process memory limit. Malformed shapes have a bounded diagnostic in the info.
- A commit that changes the shapes is checked against the shapes as they are after it: one
  update that removes the shapes and adds data is checked against none. It needs the right to
  write the shapes graph.
- When a target reaches a recursive shape (one that refers to itself through `sh:node` or
  `sh:property`, inside `sh:or` too), every data commit under the gate validates the whole
  store, after the commit and, if that finds results, before it too. The cost grows with the
  store. With LUBM 10's asserted data (1.27 million statements) and one recursive shape, a commit
  of one new node took 87 ms and peaked at 12 MB on 4 cores. Without the recursive shape, it
  took 5 ms and 1.7 MB.
- Under `reasoner.equality = "compact"`, a write of `owl:sameAs` can pass the gate though it
  brings a result ([reasoning](#reasoning)).
- With the gate set to `report` or `enforce`, a write that leaves an uncompilable shapes graph
  unchanged is refused (a bulk load can store such shapes; the info's `shacl` then says why
  they aren't counted). Repair or replace the shapes graph to restore writes. A validation
  stopped by its budget fails the write in either mode.

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
- A published snapshot's `owl:sameAs` classes are built by the first query that needs them, charged to its memory.
  Where they don't fit, that query runs without them and nothing is kept, until a query with room builds them: the
  answer is the same, only slower. Earlier versions built them in the first query whatever its budget.
- A cancel, a deadline or a refusal takes effect at the work's next poll, which bounds work, not time. Under
  nested reads in reasoning, more work runs between two polls ([the bound](architecture/contracts.md#cancellation)).
  A stop never gives a wrong answer. A query whose time limit runs out before its output begins gets `408`; one
  whose limit runs out after the server began sending its output keeps the `200` and the `NRESE-Completeness`
  header already sent, and its body is cut: on HTTP/1.1 the chunked body never gets its last chunk. Treat a body without its end as no answer, whatever its status and header say.
- Some phases run to their end after a stop: the executor's selection and sorting (seconds at 10 M rows on a term
  key), one call of a regular expression (seconds for adversarial megabyte literals), and the graph kernel's
  sorts and set-up arrays.

## Storage and transactions

- If the background maintenance worker stops, the `degraded` list in detailed health and info
  reports where it stopped, and new commits grow the write-ahead log and uncompacted run stacks
  until restart, slowing reads and increasing index memory.
- A commit holds at most 4 GiB of log payload, about 130 M quads.
  Larger loads go through the bulk loader
  ([storage](spec/02-storage-and-transactions.md#limits)).
- A client transaction keeps its operations, not their effects, and runs them again at its
  commit ([RDF4J protocol](ops/http-api.md#rdf4j-protocol)). An update with a `WHERE`, a
  `CLEAR` or `DROP`, or a removal by pattern matches what another client committed meanwhile; a
  value an update generates (`UUID()`, `RAND()`, `NOW()`, `BNODE()`, blank nodes) and read inside
  the transaction isn't the one committed; a malformed update fails only at the next read or at
  the commit; and a failed commit ends the transaction. Reads take the latest commit, and
  `SNAPSHOT` and `SERIALIZABLE` isolation are refused. Two transactions that each read and
  then write are not isolated from one another: neither commit is refused because of the
  other, so a check made inside one of them (is this item still unclaimed?) can pass in both. A read, then a write
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
  asserted in a graph it can't read. The status tells that reader that such an assertion exists.
  Its rows are right, but under the QL rewriting (`owl2-ql`, or `owl2-rl` with
  `reasoner.ql_rewriting = "on"`) the rewriting doesn't run for it at such a revision, so a row
  only the rewriting gives (one an existential leads to) can be missing there. Where the
  rewriting runs for it, its limits under `owl2-rl` ([reasoning](#reasoning): a universal
  restriction on `owl:topObjectProperty`, a store inconsistent only through an existential) hold
  for this reader alike, `complete` included. Under `reasoner.equality = "compact"`, and
  wherever the support computation leaves a list axiom out in any graph, it is told
  `not-certified` as well. So whether it is told `not-certified` or `complete` can depend on what
  a graph it can't read holds; withholding the status changes none of its rows. Before a read,
  the library's status of a query (`status_without_dl`) tells it `not-certified` on a reasoned
  store, also with `infer=false`, where the read itself carries no status (over HTTP, `complete`
  with no regime).
- An unrelated broken list in a graph the reader cannot see can change a read's status
  from `complete` to `not-certified` while its rows stay the same.
- A refusal for memory or work can hint at how much data a reader of only some graphs may not see. v2.0 admits
  requests by what the reader sees.
