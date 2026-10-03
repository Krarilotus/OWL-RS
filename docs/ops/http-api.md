# HTTP interface

What the server answers on which URL, as implemented on 30 September 2026. Paths are relative to the server's address (default `http://127.0.0.1:8080`). Which surfaces are switched on depends on the deployment posture ([config-reference.md](config-reference.md)); a disabled surface answers 404.

## SPARQL

| URL | Methods | What |
|---|---|---|
| `/dataset/sparql`, `/dataset` | GET, POST | Queries and updates on one URL |
| `/dataset/query` | GET, POST | Queries only |
| `/dataset/update` | POST | Updates only |

Use the combined URL for clients that take one endpoint address (RDF4J's SPARQL repository, Fuseki- and Blazegraph-style configurations).

**Queries** (SPARQL 1.1 Protocol):
- `GET ?query=…`
- `POST` with `Content-Type: application/x-www-form-urlencoded` and a `query` field
- `POST` with `Content-Type: application/sparql-query` and the query as the body

| Parameter | Meaning |
|---|---|
| `default-graph-uri`, `named-graph-uri` (repeatable) | The query's dataset; replaces its `FROM` clauses |
| `infer=false` | Asserted statements only (the default reads asserted and inferred) |
| `explain=true` | Run the query and return how it ran, as JSON: `executor`, `rewrites` (the rewrites that changed the query before it ran, in order: `triple-terms`, `join-groups`, `filter-pushdown`, `ask-limit`), `rows`, `micros`, and `steps` (one per operator: `depth`, `operator`, `detail`, `estimated_rows`, `rows`, `micros`) |
| `explain=plan` | Return the plan the query would run as, without running it, as JSON: `executor`, `rewrites`, and `steps` (one per plan node from the top down: `depth`, `operator`, `detail`, `estimated_rows` from the store's statistics, `null` where unknown, as below a `SERVICE`) |

Other parameters are ignored, so clients that send their own (`queryLn`, `timeout`) work.

Results that may use RDF 1.2 announce it (RDF 1.2 Concepts §2.1, SPARQL 1.2 Query Results JSON §3.1.3): when the query makes or matches triple terms or directional strings, or the store holds triple terms, JSON results carry `"version": "1.2"` in their head and the media type `; version=1.2`, and CONSTRUCT and DESCRIBE results in Turtle, TriG, N-Triples and N-Quads start with `VERSION "1.2"` and announce it in the media type. Other results announce nothing. Directional strings in the data alone don't count yet (that needs a dictionary statistic).

One extension to the query language, as Jena and QLever have it: `HAVING` may name a `SELECT` alias (`SELECT (COUNT(?x) AS ?n) … HAVING (?n > 1)`), which then means the alias's expression. Read by the standard, the alias is unbound in `HAVING` and no group passes.

Without dataset parameters and `FROM` clauses, a query reads the default graph, or the merge of all graphs if the server is configured with `store.default_graph = "union"` ([config-reference.md](config-reference.md)). `/version` says which.

**Full-text search,** with Blazegraph's vocabulary (what ResearchSpace sends), in any basic graph pattern:

```sparql
PREFIX bds: <http://www.bigdata.com/rdf/search#>
SELECT ?s ?o ?score WHERE {
  ?o bds:search "tower bridge" ; bds:relevance ?score ; bds:matchAllTerms "true" .
  ?s rdfs:label ?o .
}
```

| Predicate | Object |
|---|---|
| `bds:search` | the words (a literal); a word ending in `*` matches as a prefix; words in double quotes are a phrase, matching the literals that hold them in sequence (`"\"quick brown\" fox"`); `-word` excludes the literals that have it; `word~` (two edits) or `word~1` matches words within that many insertions, deletions, substitutions or transpositions; case doesn't matter |
| `bds:relevance` | a variable: an `xsd:double` in (0, 1], the best match 1 (BM25, normalised) |
| `bds:rank` | a variable: the rank by relevance, from 1 |
| `bds:matchAllTerms "true"` | every word must occur (else any) |
| `bds:prefixMatch "true"` | every word matches as a prefix |
| `bds:minRelevance`, `bds:minRank`, `bds:maxRank` | limits |
| `bds:stem "en"` | words match by their Snowball stem in that language, phrases too: `"connect"` finds *connected*, *connection*, *connecting* (an NRESE extension; Blazegraph ignores it). Languages: ar, da, de, el, en, es, fi, fr, hu, it, nl, no, pt, ro, ru, sv, ta, tr |

The matched literal (the subject) is a simple or language-tagged string that is the object of a statement in the graph the pattern reads. Words are runs of letters and digits. The index is built in memory at the first search and extended with new literals at later searches; a durable store writes it to `derived/` of its data directory after each checkpoint and reads it back at the first search after a restart, so it isn't built again. Searches start the pattern's joins.

The same index answers **Jena's `text:query`** (what Fuseki clients send) and **GraphDB's legacy `luc:` predicates**:

```sparql
PREFIX text: <http://jena.apache.org/text#>
SELECT ?s ?score ?label WHERE {
  (?s ?score ?label) text:query (rdfs:label "tower AND bridge" 10 "lang:en") .
}

PREFIX luc: <http://www.ontotext.com/owlim/lucene#>
SELECT ?x ?score WHERE { ?x luc:myIndex "tower bridge" ; luc:score ?score }
```

`text:query`'s subject is `?s` or a list of `?s`, the score, the matched literal (then its graph and property, unbound); its object is the query string or a list of an optional property, the query string, an optional limit on the matched literals and `"lang:xx"`. Without a property, a literal matches as the object of any property. `luc:` takes any index name (there is one index, over every string literal) and finds the resources with a matching literal. Query strings are read as Lucene's syntax as far as this index goes: words, phrases, `word*`, fuzzy `word~` and `word~1`, `AND` (every word needed, else any), `NOT` and `-` (the word is excluded; a phrase after them is dropped); `OR`, `+`, field names and boosts (`^`) are dropped. Scores are the relevance above, not Lucene's.

**Vector similarity search** through a virtual `SERVICE`: the vectors are literals of the datatype `nrv:vector` (`"[0.12, -0.5, 0.33]"^^nrv:vector`, numbers as in a JSON array; what embedding services return), and the search finds the nearest ones:

```sparql
PREFIX nrv: <urn:nrese:vector:>
SELECT ?doc ?score WHERE {
  ?doc ex:embedding ?v .
  SERVICE nrv:search {
    ?v nrv:near "[0.12, -0.5, 0.33]"^^nrv:vector ; nrv:k 10 ; nrv:score ?score .
  }
}
```

| Predicate | Object |
|---|---|
| `nrv:near` | the query vector: a vector literal, or a variable the rest of the query binds to one (`?other ex:embedding ?q`: a search per value) |
| `nrv:k` | how many nearest vectors per query vector (default 10, at most 10,000) |
| `nrv:score` | a variable: the cosine similarity or dot product (`xsd:double`), or the Euclidean distance with `nrv:metric "l2"` |
| `nrv:rank` | a variable: the rank, from 1 |
| `nrv:metric` | `"cosine"` (the default), `"dot"` or `"l2"` |
| `nrv:exact true` | compare every vector |
| `nrv:searchBudget` | the graph search's beam (default 64): wider finds more of the true nearest, slower |

The subject is the matched vector literal; only literals the query's dataset uses as objects are found, so a user finds no vector of a graph it may not read. The search returns the `k` nearest vectors that satisfy the patterns joined to it (`?doc ex:embedding ?v` with other conditions on `?doc`). Where those patterns are estimated to bind few rows, they go first and the search is among their values: an exact scan when they are few (under 2% of the vectors of that dimension), the graph with that filter otherwise. Where they bind many, the search goes first and each hit, nearest first, is checked against them through the indexes until `k` pass (100,000 vectors of 384 dimensions: 0.24 ms a query through the graph, recall@10 0.94; 1.3 ms exact). Vectors of each dimension are indexed in memory at the first search and extended with new literals at later ones; up to 20,000 of a dimension are compared exactly, beyond that through an HNSW graph per metric. The graph is built at the first search that needs it, at once up to 50,000 vectors and on a thread of its own beyond (searches scan exactly until it is there); vectors added later are scanned exactly until they pass a tenth of the graph, which is then extended on a thread. A durable store keeps the vectors and graphs in `derived/` (written after each checkpoint), so a restart doesn't rebuild them. Wrong options are client errors that name the option.

**GeoSPARQL functions** over geometry literals (`geo:wktLiteral`, `geo:geoJSONLiteral`, and `geo:gmlLiteral` for the simple features: points, lines, polygons, envelopes and their multi-forms), in any expression: the Simple Features, Egenhofer and RCC8 relations (`geof:sfWithin`, `geof:ehCovers`, `geof:rcc8ntpp`, …), `geof:relate` (DE-9IM), `geof:distance`, `geof:area`, `geof:length` (with an OGC unit such as `uom:metre`), `geof:buffer`, `convexHull`, `envelope`, `centroid`, `intersection`, `union`, `difference`, `symDifference`, `getSRID`, `isEmpty`, `dimension`, `boundary`, `asWKT`, `asGeoJSON`. Constructions answer in their first argument's serialisation (GeoJSON only in CRS84), coordinates rounded to 9 decimal places; `buffer` in metres works on CRS84 too (in a plane about the geometry); an empty literal is the empty geometry. Coordinates are CRS84 (longitude, latitude) unless the literal names another system (GeoJSON is always CRS84; GML names it in `srsName`); EPSG:4326 is read latitude first. In CRS84, metres are geodesic (WGS84), degrees planar. The relations also work as triple patterns between features and geometries (`?building geo:sfWithin ex:Berlin`, GeoSPARQL's query-rewrite extension): a feature relates through its `geo:hasDefaultGeometry` (else its `geo:hasGeometry`), a geometry through its `geo:asWKT`, `geo:asGeoJSON` or `geo:asGML`; relation statements in the data count as well; an R-tree over the shapes' bounding boxes finds the candidates (built at the first such pattern, kept for the snapshot). A filter `geof:sfWithin(?wkt, constant)` (any relation but the disjointness ones) over a `geo:asWKT`-style pattern starts the joins from the literals the R-tree finds near the constant. Not read: GML curves with arcs and 3D solids. The GeoSPARQL compliance benchmark runs in CI: 175 of its 212 queries pass, the others are reference answers that contradict the standard (`crates/nrese-sparql/tests/geosparql_compliance/expected-failures.txt`).

**Updates:**
- `POST` with a form and an `update` field
- `POST` with `Content-Type: application/sparql-update` and the update as the body

| Parameter | Meaning |
|---|---|
| `using-graph-uri`, `using-named-graph-uri` (repeatable) | The dataset of every `WHERE` clause; replaces `USING` |

`CLEAR GRAPH` and `DROP GRAPH` of a graph that holds nothing succeed: the store doesn't record empty graphs, so there is nothing to miss.

On the combined URL a form is a query or an update by its field, and a body by its media type. A `GET` without `query` returns the service description.

**Result formats,** chosen from the `Accept` header by weight (`q`), with wildcards; the first of each list is the default:

| Query form | Media types |
|---|---|
| `SELECT` | `application/sparql-results+json`, `application/sparql-results+xml`, `text/csv`, `text/tab-separated-values` |
| `ASK` | `application/sparql-results+json`, `application/sparql-results+xml` |
| `CONSTRUCT`, `DESCRIBE` | `application/n-triples`, `text/turtle`, `application/rdf+xml`, `application/ld+json`, `application/n-quads`, `application/trig`, `application/x-binary-rdf` (RDF4J's Binary RDF, also accepted as a payload) |

`application/json` and `application/xml` are taken as the SPARQL results formats, `application/x-turtle` as Turtle, and `text/plain` as N-Triples.

## Classification

`GET /dataset/classification`: the OWL 2 EL class hierarchy of the asserted statements (all graphs), computed on request with the completion rules of CEL and ELK (conjunctions, existential restrictions, property hierarchies, chains and transitivity, domains, ranges, disjointness). On random EL ontologies it equals ELK's hierarchy.

- `application/json` (the default): `subsumptions` (pairs `[sub, super]` of class IRIs, every one, equivalent classes both ways, without `owl:Thing`), `unsatisfiable` (classes that can have no instance), `skipped` (axioms outside EL by kind: unions, universal restrictions, cardinalities, inverses, nominals; the hierarchy is complete for the EL part), `micros`
- `application/n-triples`: the same as `rdfs:subClassOf` statements, `owl:Nothing` as the superclass of unsatisfiable classes

The hierarchy isn't stored; the reasoning modes materialise instance-level inferences (see [config-reference.md](config-reference.md)).

## Graph Store Protocol

`/dataset/data?default` or `/dataset/data?graph=<IRI>`

| Method | Does | Answers |
|---|---|---|
| GET, HEAD | Reads the graph, in the graph formats above | 200; 404 if the named graph doesn't exist |
| PUT | Replaces the graph with the payload | 201 if it created the named graph, 200 if it replaced it, 204 for the default graph |
| POST | Adds the payload to the graph | as PUT |
| DELETE | Removes the graph | 204; 404 if the named graph doesn't exist |

- A named graph exists while it holds statements. The default graph always exists.
- Payloads: any of the six graph formats, named by `Content-Type`. A payload that names graphs of its own (possible in N-Quads, TriG and JSON-LD) is rejected.
- `Content-Location` gives the base IRI for relative IRIs in the payload.
- Writing the same content twice leaves the same graph: blank nodes are scoped to the payload, and a `PUT` replaces.

## RDF4J protocol

The RDF4J REST protocol, as RDF4J's `HTTPRepository`, GraphDB's clients and the tools built on them speak it. The configured store is repository `nrese` (what `/dataset/…` serves); `PUT /repositories/{id}` creates another (admins; on disk under `repositories/<id>/` of the data directory, opened again at start, with the default's store settings). The repository configuration in the body (RDF4J's, Turtle unless `Content-Type` says otherwise) sets its title (`rdfs:label`) and reasoning: GraphDB's `ruleset` (`empty`, `rdfs`, `rdfsplus`, `owl-horst`, `owl2-ql`, `owl2-rl`, also `-optimized`, or an NRESE reasoning mode), else an RDFS inferencer sail (`…RDFSInferencer`) for RDFS and a sail stack without one for none; a configuration naming neither keeps the server's reasoning, and one naming another repository id is refused (400). User rules: a `ruleset` that is the path of a GraphDB ruleset file (`….pie`, read on the server; the whole program), or rules in the configuration (`nrc:rules`, Notation3 unless `nrc:rulesFormat "pie"`, `nrc:` = `https://nrese.dev/ns/config#`; added to the ruleset named beside them, else the whole program). Rules are checked when the repository is created (400 with the faulty rule) and kept with its settings and `DELETE /repositories/{id}` removes one with its data. A repository besides the default has no `SERVICE` client and no preloaded ontology.

| Path | What |
|---|---|
| `GET /protocol` | the protocol version, `12` |
| `GET /repositories` | the repository list (SPARQL results) |
| `PUT`, `DELETE /repositories/{id}` | a repository created or removed (admins) |
| `GET`, `POST /repositories/{id}` | a SPARQL query (`query`, `infer`, the dataset parameters); a form with `update` is an update |
| `GET /repositories/{id}/statements` | the statements matching `subj`, `pred`, `obj`, `context` (repeatable), `infer`, in the negotiated RDF format (N-Quads and TriG with their graphs) |
| `POST /repositories/{id}/statements` | an RDF payload added (into each `context` if given, else into the graphs it names), or a SPARQL update (`update=` form, `application/sparql-update`) |
| `PUT /repositories/{id}/statements` | the statements of the `context`s (or all) replaced by the payload |
| `DELETE /repositories/{id}/statements` | the statements matching `subj`, `pred`, `obj`, `context` removed |
| `GET /repositories/{id}/size` | the number of statements (in the `context`s) |
| `GET /repositories/{id}/contexts` | the named graphs (`contextID`) |
| `GET`, `DELETE /repositories/{id}/namespaces`; `GET`, `PUT`, `DELETE /repositories/{id}/namespaces/{prefix}` | namespace prefixes |
| `/repositories/{id}/rdf-graphs/service` | the Graph Store protocol, as `/dataset/data` |
| `POST /repositories/{id}/transactions` | begins a transaction: `201` with its URL in `Location` |
| `PUT /…/transactions/{txid}?action=ADD`, `DELETE`, `UPDATE`, `COMMIT`, `PING`, `QUERY`, `GET`, `SIZE`; `DELETE /…/transactions/{txid}` | a transaction's operations; `COMMIT` applies them in one commit, `DELETE` rolls back |

Terms in `subj`, `pred`, `obj` and `context` are written as in N-Triples (`<iri>`, `_:b`, `"text"@en`, `"1"^^<…#int>`), and `context=null` is the default graph. Writes go through the same pipeline as SPARQL updates (validation, reasoning, the SHACL gate) and need the update permission. A transaction's operations wait on the server and are applied at its commit; its reads (`QUERY`, `GET`, `SIZE`) see its changes (they are applied to a speculative engine transaction that is never committed and takes no writer slot, so other clients commit meanwhile; the result is kept for the transaction's next read until it or the store changes). A transaction belongs to whoever began it. Transactions untouched for ten minutes are dropped. Namespaces start as `rdf`, `rdfs`, `owl` and `xsd`; an on-disk store keeps them in `namespaces.json` in its data directory. A query or update may use them without declaring them, as in GraphDB and RDF4J repositories: a prefix it uses but doesn't declare means what the repository binds it to (its own `PREFIX` declarations win; the result cache keeps such queries apart per set of namespaces).

## SHACL validation

`/dataset/shacl`

| Method | Validates against | Body |
|---|---|---|
| GET | The repository's shapes graph (`shacl.shapes_graph`, by default `http://rdf4j.org/schema/rdf4j#SHACLShapeGraph`) | none |
| POST | The shapes in the request body; they aren't stored | The shapes, in any graph format |

Shapes are stored like any graph: `PUT /dataset/data?graph=<the shapes graph>`.

| Parameter | Meaning |
|---|---|
| (none) | Validate every graph, except graphs that hold shapes |
| `graph=<IRI>` or `default` | Validate one graph |
| `shapes-graph=<IRI>` | `GET` only: shapes stored in another graph |
| `infer=false` | Validate asserted statements only (the default includes inferred ones) |

The answer is the validation report, with status 200 whether the data conforms or not:
- `text/turtle` by default, or any other graph format: the `sh:ValidationReport` graph;
- `application/json`: `conforms`, `revision`, `shapes` (how many were used; 0 means no shapes were found) and `results`, each with `focusNode`, `resultPath`, `value`, `sourceShape`, `sourceConstraintComponent`, `resultSeverity` and `resultMessage`.

Ill-formed shapes are 400, naming the shape and the parameter.

Limits today:
- SHACL Core only; validation runs on request, not yet on commit.
- A `POST` holds the writer while it validates (its shapes live in a transaction that is never committed), so other writes wait.
- The shapes graph is an ordinary graph: queries see it, and reasoning reads it like any other.

## Draft checks (`/api/v1/draft-check`)

Isolated checks of pinned inputs, for the Datamodel Workflow and any other client of its store-check protocol (`dmw-store-check/1`, see [the DMW guide](../integration/datamodel-workflow.md#draft-checks)). Each check runs in a throwaway in-memory store: no repository is read or written. The permission asked is the one to query.

| Method and URL | What |
|---|---|
| `GET /api/v1/draft-check/capabilities` | The protocol, operations (`shacl`, `query`, `reasoning`), profiles, semantics, this build's id, and the longest time limit a request may ask for |
| `POST /api/v1/draft-check` | One check: a JSON request with the check's identity, limits and inputs (N-Triples; the query as SPARQL text); the answer is JSON |

Every answer is 200 with a terminal status (`completed`, `failed`, `timeout`, `unsupported`) and echoes the request's identity. A request that isn't a draft-check request is 400; a larger one than the upload limit, 413.

Limits today:
- One profile, `owl2-rl`: consistency by the OWL 2 RL rules, unsatisfiable classes by the OWL 2 EL classifier. Axioms either skipped are reported as unsupported, and the answer is then not complete.
- SHACL Core only; SHACL-SPARQL, SHACL-AF and `sh:entailment` are reported as unsupported.
- A time limit above the server's query timeout is refused, not shortened.

## Engine API (`/api/v1`)

One repository-scoped API for every capability ([ADR-0007](../adr/0007-one-engine-api.md)), the same for every repository; the `/dataset/…` routes are the default repository's (`nrese`) and stay. `GET /api/v1/openapi.json` describes it (OpenAPI 3.1, generated from the server's handlers and types; a test keeps it complete), for generating clients.

| URL | What |
|---|---|
| `GET /api/v1/repositories` | The repositories (JSON: `id`, `title`, `path`, `default`) |
| `GET`/`PUT`/`PATCH`/`DELETE /api/v1/repositories/{id}` | One repository (`title`, `reasoning` in effect, `settings`); `PUT` creates it with JSON settings (`title`, `reasoning` by mode name, `rules`), 201, 409 if it exists; `PATCH` changes the settings given (`null` removes one), a new `reasoning` or `rules` at once: the inferences are recomputed, and writes waiting meanwhile are answered 503 (send them again); `DELETE` removes it and its data (administrators). The default repository's changed settings are kept in `repository.json` of the data directory and override the configuration's reasoning at the next start |
| `…/repositories/{id}/query`, `/update`, `/sparql` | SPARQL Protocol, as `/dataset/query`, `/dataset/update`, `/dataset` |
| `…/repositories/{id}/data` | Graph Store Protocol (`?graph=` or `?default`), as `/dataset/data` |
| `…/repositories/{id}/tell`, `/shacl`, `/autocomplete`, `/classification` | as their `/dataset/…` counterparts |
| `…/repositories/{id}/info`, `/summary`, `/reasoning`, `/service-description` | readiness and statistics, reasoning diagnostics, the service description |
| `…/repositories/{id}/backup`, `/restore` | N-Quads backup and restore |
| `POST …/repositories/{id}/image` | An image backup into `backups/` of the data directory (administrators) |
| `GET …/repositories/{id}/namespaces`, `PUT`/`DELETE …/namespaces/{prefix}` | The repository's prefixes (JSON object; the IRI as the `PUT` body); RDF4J's `/namespaces` reads the same |
| `POST …/repositories/{id}/sessions` | Opens a client transaction (JSON: `id`, `path`, `idle_seconds`); then `POST {path}/update` (SPARQL update), `POST`/`DELETE {path}/data` (RDF to add or remove, `?graph=` for its graph), `GET`/`POST {path}/query` (reads the data as the session would leave it), `POST {path}/commit`, `DELETE {path}` (rollback). A session exists for the user who opened it only (its id is random; for anyone else it is 404), and reads in it keep their view of the data until the session or the store changes. RDF4J transactions use the same sessions |
| `GET …/repositories/{id}/explain?subj=&pred=&obj=` | Why a statement holds (terms in N-Triples syntax): `{"steps": [...]}`, the statement first, each step with `subject`, `predicate`, `object`, `origin` (`asserted`, `inferred`), `rule` and `premises` (indexes of steps); the shallowest, smallest derivation from asserted statements, the same on every call. Within the requester's graph access: an asserted premise in no graph it may read is a step with `origin` `hidden` and no terms, and a statement it doesn't see (an inferred one where inferences are hidden from it or its graphs don't support it, an asserted one in no readable graph) is a 404 as if it didn't hold. 404 if it doesn't hold or reasoning is off |
| `POST …/repositories/{id}/import` | Bulk-loads the RDF document in the body (`?graph=`, `?replace=true`, `?skip_errors=true`), then recomputes the inferences (administrators); with `?async=true` as a job: 202 with its `path` |
| `GET`/`POST …/repositories/{id}/import/files` | The files of the server's import directory (`store.import_directory`; 404 without one), and importing some by name (`{"files": [...], "graph", "replace", "skip_errors"}`) as a job, without upload or size limit; paths can't leave the directory (administrators) |
| `GET /api/v1/jobs`, `GET`/`DELETE /api/v1/jobs/{job}` | Imports running and the latest 100 finished: `state` (`running`, `done`, `failed`, `cancelled`), `phase` (`loading`, `reasoning`), files done, statements parsed, and the report or error (operators); `DELETE` cancels a running import before its next file, committing nothing (administrators) |
| `POST …/repositories/{id}/reasoning/rematerialise` | Recomputes the inferences (administrators) |
| `GET`, `PUT`, `DELETE …/repositories/{id}/rules` | The repository's user rules: `GET` as stored (JSON: `name`, `format`, `text`); `PUT` the file as it is (`?format=n3` or `pie`, else by `?name=`'s extension), administrators only, compiled before it is stored (a mistake is `400` with its line and column) and in effect at once; `DELETE` removes them. A repository without reasoning of its own gets `custom` for a GraphDB ruleset (the whole program), else its current reasoning plus the rules; without rules, a `custom` repository returns to the server's reasoning |
| `GET`, `PUT`, `DELETE …/repositories/{id}/shapes` | The repository's SHACL shapes (its shapes graph, `shacl.shapes_graph`), read and written as the Graph Store Protocol does. Shapes are checked before they are stored, by every write to the shapes graph through any protocol and whether the SHACL gate is on or not: shapes that don't compile are refused (`400`, one message per problem) and the graph stays as it was |
| `GET …/repositories/{id}/graphs` | The graphs with statements the requester may read, each with its number of asserted statements (`graph` absent for the default graph) |
| `GET …/repositories/{id}/queries`, `DELETE …/queries/{query}` | The queries running now (`id`, `query`, `origin`, `started`, `elapsed_ms`; operators), and cancelling one (administrators): it stops at its next check and its client gets a 408 |

### Users, workspaces and policies (`/api/v1/access`)

[ADR-0008](../adr/0008-users-workspaces-policies.md). Server-wide; every change takes a `reason` (in the JSON body, or `?reason=` for `DELETE` and the import), answers with its history record (`number`, `author`, `time`, `reason`, `summary`, `added`, `removed`), and applies to the next request.

| URL | What | Who |
|---|---|---|
| `GET /api/v1/access/me` | The requester: `user`, `roles`, `admin`, `enforced`, `reads_everything`, `personal_space`, `workspaces` (each with `prefix`, `level`, `members`) | anyone who may read |
| `GET /api/v1/access` | The whole state: `settings`, `roles`, `users` (`local_login`, never a hash), `workspaces` | administrators |
| `PUT /api/v1/access/settings` | `enforced`, `fallback` (`deny`, `allow`), `inferred` (`hidden`, `visible`, `supported`), `users_create_workspaces`, `min_password_length` (10), `session_hours` (12) | administrators |
| `PUT`/`DELETE /api/v1/access/roles/{name}` | A role's rule: `read`, `write`, `deny` (IRIs or prefixes ending in `*`), `default_graph` (`none`, `read`, `write`, `deny`), `service` (whether its users may call other endpoints with `SERVICE`) | administrators |
| `PUT`/`DELETE /api/v1/access/users/{name}` | A user record: `admin`, `roles` (added to its credentials'), `password` for a local login (empty removes it; at least `min_password_length` characters); removal takes its memberships along | administrators; a user its own `password` |
| `POST /api/v1/access/login` | `{"user", "password"}` of a local login: `{"user", "token", "expires_in_seconds"}`; send `Authorization: Bearer {token}` afterwards. 401 for wrong credentials, 429 after too many failures from the client's address (`auth.trusted_proxies`) | anyone |
| `POST /api/v1/access/logout` | Ends the session of the bearer token sent | the session |
| `GET /api/v1/access/workspaces` | The requester's workspaces, its personal space first (administrators: all) | anyone who may read |
| `GET`/`PUT`/`DELETE /api/v1/access/workspaces/{name}` | A workspace: `title`, `repository` (empty: every one), `graphs` taken in besides its prefix (administrators only). Whoever creates one owns it; `~user` is that user's personal space | owners, administrators |
| `PUT`/`DELETE /api/v1/access/workspaces/{name}/members/{user}` | A member's `level`: `owner`, `editor`, `viewer`; a personal space has viewers only; a workspace keeps an owner (409) | owners, administrators |
| `GET /api/v1/access/history?limit=` | The changes, the latest first (100 by default) | administrators |
| `POST /api/v1/access/import?reason=`, `GET /api/v1/access/export` | The role rules and fallbacks as a policy file (TOML); importing turns enforcement on | administrators |
| `GET /api/v1/saved-queries`; `GET`, `PUT`, `DELETE /api/v1/saved-queries/{space}/{name}` | Saved queries (and updates) in a space: a personal space (`~alice`) or a workspace. `PUT` takes `query`, optional `title`, `description` and `repository`; the text must parse (with its repository's namespaces), and the name is letters, digits, `.`, `_`, `-`. Each keeps who changed it last and when | read: whoever reads the space; write: its editors and owners; administrators |

Graph prefixes: a personal space is `{base}space/{user}/`, a workspace `{base}workspace/{name}/`, with `base` from `auth.workspace_base` (`NRESE_WORKSPACE_BASE`, default `urn:nrese:`). User names come from a token's `sub` (or an introspection's `username`) or a client certificate's subject, where they are letters, digits and `. _ @ + | : -`, or from a local login: `Authorization: Basic` with a user's password (Argon2id; a credential verified once is remembered while the password stays), or a session token from `/login`. Local logins work besides any authentication mode (`auth.local_logins`, on by default); a user's sessions end when its password changes or it is removed.

## Graph-level access control

With access enforced (the access state's `enforced` setting, on once a policy file is imported or an administrator turns it on) each user has its own dataset: the graphs its roles, its workspaces and its personal space let it read. The policy file (`auth.access_policy`, [configuration](config-reference.md#graph-level-access-control)) is imported at the first start; afterwards the state changes through `/api/v1/access` (a changed file is reported at start and applies only when imported).

- **Queries** evaluate over that dataset, restricted before anything is evaluated: the other graphs are absent, not forbidden. `GRAPH ?g` never binds them, `FROM` and `GRAPH` with their names match nothing, counts don't see them, and a union default graph merges the readable graphs only. The same holds for the `WHERE` clauses of updates, RDF4J's `/statements`, `/size` and `/contexts`, and reads inside RDF4J transactions.
- **Inferred statements** come from statements in any graph; users who may not read every graph see the asserted statements only, unless the policy sets `inferred = "visible"` (all inferred statements, to users who may read the default graph) or `inferred = "supported"` (those one of whose derivations uses graphs the user may read alone; [configuration](config-reference.md)). Queries, counts, statement reads and explanations agree on which.
- **Graph Store reads** of an unreadable graph answer 404, as for a graph that doesn't exist.
- **Writes** that would insert or delete a statement in a graph the user may not write are refused as a whole with 403, whether the statement is there or not, so the answer says nothing about graphs the user can't read. `CLEAR`/`DROP ALL` and `NAMED`, and RDF4J deletions without a context, act on the readable graphs only; clearing an unreadable graph does nothing.
- **Endpoints over the whole dataset** (autocomplete, classification, SHACL validation, query suggestions) answer 403 to users who may not read every graph.

## Status codes

| Code | When |
|---|---|
| 400 | The request is wrong: syntax error, invalid IRI, malformed RDF, a commit rejected by a consistency check (with an explanation) |
| 401, 403 | Authentication or authorisation failed; 403 also for a write to a graph the access policy doesn't let the user write |
| 409 | The resource exists already (a repository), or the change would leave a workspace without an owner |
| 404 | The surface is disabled, or the named graph doesn't exist |
| 406 | The client accepts no format the result has |
| 408 | The policy timeout passed; a timed-out write was not committed |
| 413 | The request or the query's memory need exceeds the policy limit |
| 415 | The body's media type isn't one the endpoint reads |
| 429 | Rate limit |
| 500 | The server's fault; logged with the request id |
| 503 | Starting, or (for `/readyz`) in quarantine |

Errors are `application/problem+json` documents. Every response carries `x-request-id`.

## Service and operations

| URL | What |
|---|---|
| `/healthz`, `/readyz` | Liveness; readiness with revision, reasoning mode and consistency |
| `/version` | Build, enabled surfaces, reasoning semantics |
| `/dataset/service-description` | SPARQL service description (Turtle); it names every endpoint above |
| `/dataset/info` | Statement and graph counts |
| `/dataset/autocomplete?q=…` | Resources whose labels' words or local names' words begin with the words of `q`, best first: `{"suggestions": [{"iri", "label", "score"}]}`. Labels are the values of `rdfs:label`, `skos:prefLabel`, `skos:altLabel`, `foaf:name`, `schema:name`, `dcterms:title` and `dc:title`; local names are split at camel case and punctuation. `limit` (default 10, at most 1000), `infer=false` for asserted statements only. The indexes are built at the first use |
| `/dataset/tell` | Adds an RDF payload to a graph (`POST`) |
| `/metrics` | Prometheus metrics (below) |
| `/console`, `/ops` | User console, operator UI |
| `GET /api/v1/capabilities` | What the server offers: surfaces, endpoints, reasoning modes |
| `GET /api/v1/health` | Readiness and the state of every part (store, reasoning, access, jobs) |
| `GET /api/v1/diagnostics` | How the server runs: memory, caches, the query budget, requests |
| `GET /api/v1/ai/status`, `POST /api/v1/ai/query-suggestions` | AI query suggestions, where configured |

**Routes of the earlier release** answer as before until the next release, with `Deprecation: @1790985600` (RFC 9745: since 3 October 2026) and `Link: <successor>; rel="successor-version"`:

| Earlier | Now |
|---|---|
| `/ops/api/capabilities` | `/api/v1/capabilities` |
| `/ops/api/health/extended` | `/api/v1/health` |
| `/ops/api/diagnostics/runtime` | `/api/v1/diagnostics` |
| `/ops/api/diagnostics/reasoning`, `/ops/api/dataset/summary` | `/api/v1/repositories/{id}/reasoning`, `…/summary` |
| `/ops/api/admin/dataset/backup`, `/restore`, `/image` | `/api/v1/repositories/{id}/backup`, `/restore`, `/image` |
| `/api/ai/status`, `/api/ai/query-suggestions` | `/api/v1/ai/status`, `/api/v1/ai/query-suggestions` |
| `/api/v1/queries…` (saved queries) | `/api/v1/saved-queries…` (the running queries are `/api/v1/repositories/{id}/queries`) |

`/metrics`, in Prometheus' text format:

| Metric | What |
|---|---|
| `nrese_ready`, `nrese_dataset_revision`, `nrese_store_quads`, `nrese_store_inferred`, `nrese_store_named_graphs` | Readiness and the dataset |
| `nrese_reasoner_mode_info`, `nrese_store_mode_info` | Modes, as labels |
| `nrese_query_cache_hits_total`, `nrese_query_cache_misses_total`, `nrese_query_cache_bytes` | The result cache |
| `nrese_query_memory_bytes`, `nrese_query_memory_peak_bytes`, `nrese_query_memory_limit_bytes` | Intermediate results of running queries, against `budgets.total_query_memory` |
| `nrese_http_responses_total{kind, status}` | Responses by kind of request (`query`, `update`, `sparql` for the single endpoint, `graph_store`, `shacl`, `other`) and status class (`2xx` … `5xx`) |
| `nrese_http_request_duration_seconds{kind}` | Histogram of the time to the response's start, by kind |
| `nrese_http_requests_in_flight{kind}` | Requests begun and not yet answered (active and queued work), by kind |
| `nrese_index_runs`, `nrese_index_bytes{place}` | Index runs; index data on the heap and mapped from the checkpoint |
| `nrese_dictionary_terms`, `nrese_dictionary_bytes{part}` | Dictionary terms; their text, the heap's index, and what is mapped |
| `nrese_wal_bytes_since_checkpoint`, `nrese_compactions_total`, `nrese_checkpoints_total` | What a restart would replay; run merges and checkpoints since start |
| `nrese_vector_index_bytes` | The vector index (vectors and HNSW graphs) |
| `nrese_derived_indexes_loaded_total` | Derived indexes (text, IRI text, vectors) read from `derived/` since start instead of built |
| `nrese_support_sets_total{how}`, `nrese_support_views_total{how}`, `nrese_support_seconds_total{phase}` | Support graph sets for `inferred = "supported"`: computed afresh or updated for a commit, readers' views built or patched, and the time each took |
| `nrese_process_resident_bytes` | Resident memory (Linux; mapped file pages included, which the OS can drop) |
| `nrese_backups_total{kind, outcome}` | Backups and restores (`dump`: N-Quads export, `image`, `restore`) that succeeded (`ok`) or `failed` |
| `nrese_backup_last_success_timestamp_seconds{kind}`, `nrese_backup_last_duration_seconds{kind}`, `nrese_backup_last_bytes{kind}` | The last success of each kind: when it ended (0: none since start), how long it took, its size (for alerting on stale backups) |
