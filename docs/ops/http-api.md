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
| `explain=true` | Run the query and return how it ran, as JSON |

Other parameters are ignored, so clients that send their own (`queryLn`, `timeout`) work.

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

The matched literal (the subject) is a simple or language-tagged string that is the object of a statement in the graph the pattern reads. Words are runs of letters and digits. The index is built in memory at the first search after startup and extended with new literals at later searches; searches start the pattern's joins.

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

Terms in `subj`, `pred`, `obj` and `context` are written as in N-Triples (`<iri>`, `_:b`, `"text"@en`, `"1"^^<…#int>`), and `context=null` is the default graph. Writes go through the same pipeline as SPARQL updates (validation, reasoning, the SHACL gate) and need the update permission. A transaction's operations wait on the server and are applied at its commit; its reads (`QUERY`, `GET`, `SIZE`) see its changes (they are applied to an engine transaction that is never committed, which holds the writer slot while they read). Transactions untouched for ten minutes are dropped. Namespaces start as `rdf`, `rdfs`, `owl` and `xsd`; an on-disk store keeps them in `rdf4j-namespaces.json` in its data directory.

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

## Engine API (`/api/v1`)

One repository-scoped API for every capability ([ADR-0007](../adr/0007-one-engine-api.md)), the same for every repository; the `/dataset/…` routes are the default repository's (`nrese`) and stay.

| URL | What |
|---|---|
| `GET /api/v1/repositories` | The repositories (JSON: `id`, `title`, `path`, `default`) |
| `GET`/`PUT`/`DELETE /api/v1/repositories/{id}` | One repository (`title`, `reasoning` in effect, `settings`); `PUT` creates it with JSON settings (`title`, `reasoning` by mode name, `rules`), 201, 409 if it exists; `DELETE` removes it and its data (administrators) |
| `…/repositories/{id}/query`, `/update`, `/sparql` | SPARQL Protocol, as `/dataset/query`, `/dataset/update`, `/dataset` |
| `…/repositories/{id}/data` | Graph Store Protocol (`?graph=` or `?default`), as `/dataset/data` |
| `…/repositories/{id}/tell`, `/shacl`, `/autocomplete`, `/classification` | as their `/dataset/…` counterparts |
| `…/repositories/{id}/info`, `/summary`, `/reasoning`, `/service-description` | readiness and statistics, reasoning diagnostics, the service description |
| `…/repositories/{id}/backup`, `/restore` | N-Quads backup and restore |
| `GET …/repositories/{id}/namespaces`, `PUT`/`DELETE …/namespaces/{prefix}` | The repository's prefixes (JSON object; the IRI as the `PUT` body); RDF4J's `/namespaces` reads the same |
| `POST …/repositories/{id}/sessions` | Opens a client transaction (JSON: `id`, `path`, `idle_seconds`); then `POST {path}/update` (SPARQL update), `POST`/`DELETE {path}/data` (RDF to add or remove, `?graph=` for its graph), `GET`/`POST {path}/query` (reads the data as the session would leave it), `POST {path}/commit`, `DELETE {path}` (rollback). RDF4J transactions use the same sessions |
| `GET …/repositories/{id}/explain?subj=&pred=&obj=` | Why a statement holds (terms in N-Triples syntax): `{"steps": [...]}`, the statement first, each step with `subject`, `predicate`, `object`, `origin` (`asserted`, `inferred`), `rule` and `premises` (indexes of steps); the shallowest, smallest derivation from asserted statements, the same on every call. 404 if it doesn't hold or reasoning is off |
| `POST …/repositories/{id}/import` | Bulk-loads the RDF document in the body (`?graph=`, `?replace=true`, `?skip_errors=true`), then recomputes the inferences (administrators) |
| `POST …/repositories/{id}/reasoning/rematerialise` | Recomputes the inferences (administrators) |

### Users, workspaces and policies (`/api/v1/access`)

[ADR-0008](../adr/0008-users-workspaces-policies.md). Server-wide; every change takes a `reason` (in the JSON body, or `?reason=` for `DELETE` and the import), answers with its history record (`number`, `author`, `time`, `reason`, `summary`, `added`, `removed`), and applies to the next request.

| URL | What | Who |
|---|---|---|
| `GET /api/v1/access/me` | The requester: `user`, `roles`, `admin`, `enforced`, `reads_everything`, `personal_space`, `workspaces` (each with `prefix`, `level`, `members`) | anyone who may read |
| `GET /api/v1/access` | The whole state: `settings`, `roles`, `users` (`local_login`, never a hash), `workspaces` | administrators |
| `PUT /api/v1/access/settings` | `enforced`, `fallback` (`deny`, `allow`), `inferred` (`hidden`, `visible`), `users_create_workspaces` | administrators |
| `PUT`/`DELETE /api/v1/access/roles/{name}` | A role's rule: `read`, `write`, `deny` (IRIs or prefixes ending in `*`), `default_graph` (`none`, `read`, `write`, `deny`) | administrators |
| `PUT`/`DELETE /api/v1/access/users/{name}` | A user record: `admin`, `roles` (added to its credentials'); removal takes its memberships along | administrators |
| `GET /api/v1/access/workspaces` | The requester's workspaces, its personal space first (administrators: all) | anyone who may read |
| `GET`/`PUT`/`DELETE /api/v1/access/workspaces/{name}` | A workspace: `title`, `repository` (empty: every one), `graphs` taken in besides its prefix (administrators only). Whoever creates one owns it; `~user` is that user's personal space | owners, administrators |
| `PUT`/`DELETE /api/v1/access/workspaces/{name}/members/{user}` | A member's `level`: `owner`, `editor`, `viewer`; a personal space has viewers only; a workspace keeps an owner (409) | owners, administrators |
| `GET /api/v1/access/history?limit=` | The changes, the latest first (100 by default) | administrators |
| `POST /api/v1/access/import?reason=`, `GET /api/v1/access/export` | The role rules and fallbacks as a policy file (TOML); importing turns enforcement on | administrators |

Graph prefixes: a personal space is `{base}space/{user}/`, a workspace `{base}workspace/{name}/`, with `base` from `auth.workspace_base` (`NRESE_WORKSPACE_BASE`, default `urn:nrese:`). User names come from a token's `sub` (or an introspection's `username`) or a client certificate's subject, where they are letters, digits and `. _ @ + | : -`.

## Graph-level access control

With access enforced (the access state's `enforced` setting, on once a policy file is imported or an administrator turns it on) each user has its own dataset: the graphs its roles, its workspaces and its personal space let it read. The policy file (`auth.access_policy`, [configuration](config-reference.md#graph-level-access-control)) is imported at the first start; afterwards the state changes through `/api/v1/access` (a changed file is reported at start and applies only when imported).

- **Queries** evaluate over that dataset, restricted before anything is evaluated: the other graphs are absent, not forbidden. `GRAPH ?g` never binds them, `FROM` and `GRAPH` with their names match nothing, counts don't see them, and a union default graph merges the readable graphs only. The same holds for the `WHERE` clauses of updates, RDF4J's `/statements`, `/size` and `/contexts`, and reads inside RDF4J transactions.
- **Inferred statements** come from statements in any graph; users who may not read every graph see the asserted statements only, unless the policy sets `inferred = "visible"`.
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
| `/ops/api/…` | Operator API: capabilities, diagnostics, backup and restore |

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
| `nrese_process_resident_bytes` | Resident memory (Linux; mapped file pages included, which the OS can drop) |
| `nrese_backups_total{kind, outcome}` | Backups and restores (`dump`: N-Quads export, `image`, `restore`) that succeeded (`ok`) or `failed` |
| `nrese_backup_last_success_timestamp_seconds{kind}`, `nrese_backup_last_duration_seconds{kind}`, `nrese_backup_last_bytes{kind}` | The last success of each kind: when it ended (0: none since start), how long it took, its size (for alerting on stale backups) |
