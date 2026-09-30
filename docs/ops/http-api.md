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

Without dataset parameters and `FROM` clauses, a query reads the default graph, or the merge of all graphs if the server is configured with `store.default_graph = "union"` ([config-reference.md](config-reference.md)). `/version` says which.

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
| `CONSTRUCT`, `DESCRIBE` | `application/n-triples`, `text/turtle`, `application/rdf+xml`, `application/ld+json`, `application/n-quads`, `application/trig` |

`application/json` and `application/xml` are taken as the SPARQL results formats, `application/x-turtle` as Turtle, and `text/plain` as N-Triples.

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

## Status codes

| Code | When |
|---|---|
| 400 | The request is wrong: syntax error, invalid IRI, malformed RDF, a commit rejected by a consistency check (with an explanation) |
| 401, 403 | Authentication or authorisation failed |
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
| `/dataset/tell` | Adds an RDF payload to a graph (`POST`) |
| `/metrics` | Prometheus metrics |
| `/console`, `/ops` | User console, operator UI |
| `/ops/api/…` | Operator API: capabilities, diagnostics, backup and restore |
