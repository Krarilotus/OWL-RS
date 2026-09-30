# NRESE as the export store of the Datamodel Workflow

How the Datamodel Workflow (DMW) uses NRESE through the standard protocols its exporter already speaks (its plan: WP15 export, D18 validation in the store). Nothing here is specific to DMW on the server side: any exporter that uses SPARQL Update, the Graph Store Protocol and SHACL can follow it.

Status on 30 September 2026: the protocol below is replayed end to end by `scripts/smoke-dmw-export.sh`, which passes with reasoning off and with `owl2-rl`. DMW's own exporter doesn't exist yet, so nothing has been run from DMW itself.

## Server settings

| Setting | Value | Why |
|---|---|---|
| `store.default_graph` | `union` | Exports go into one named graph per module and version. Competency questions and provenance queries don't name graphs, so they must read all of them |
| `store.mode` | `on-disk` | The default is in memory |
| `reasoner.mode` | `disabled`, `rdfs` or `owl2-rl` | With `owl2-rl`, inferred statements are queryable and an export that makes the data inconsistent is rejected with an explanation ([reasoning-semantics.md](../spec/reasoning-semantics.md)) |
| `auth.mode` | `bearer-static`, `bearer-jwt` or `oidc-introspection` | Writing needs the admin token or role; the read token or role can only query |
| `shacl.shapes_graph` | the default | The graph that holds the ABox contract |

As environment variables: `NRESE_DEFAULT_GRAPH=union`, `NRESE_STORE_MODE=on-disk`, `NRESE_REASONING_MODE=…`, `NRESE_AUTH_MODE=…` ([config-reference.md](../ops/config-reference.md)).

## What the exporter does

All URLs are relative to the server; [http-api.md](../ops/http-api.md) has the details.

| Step | Request | Answer |
|---|---|---|
| Export a module version or a batch of accepted data | `PUT /dataset/data?graph=<graph IRI>` with the graph as Turtle, N-Triples, RDF/XML, JSON-LD, N-Quads or TriG | 201 if the graph is new, 200 if it replaced one |
| Export the same version again | the same `PUT` | 200, and the graph is identical: a `PUT` replaces, and blank nodes are scoped to the payload |
| Read a graph back | `GET /dataset/data?graph=<graph IRI>` | 200; 404 if it doesn't exist |
| Remove a version | `DELETE /dataset/data?graph=<graph IRI>` | 204; 404 if it doesn't exist |
| Change data in place | `POST /dataset/sparql` with a SPARQL Update (`update=…` form field, or `application/sparql-update` body) | 204 |
| Ask a competency question | `POST /dataset/sparql` with `query=…` | the results, in the format `Accept` asks for |
| Store the ABox contract | `PUT /dataset/data?graph=http://rdf4j.org/schema/rdf4j#SHACLShapeGraph` with the shapes | 201 or 200 |
| Validate everything against the stored shapes | `GET /dataset/shacl` with `Accept: application/json` | 200 with `conforms`, `shapes` and `results` |
| Validate one export | `GET /dataset/shacl?graph=<graph IRI>` | the same, for that graph |
| Validate a draft against shapes that aren't stored | `POST /dataset/shacl?graph=<graph IRI>` with the shapes as the body | the report; nothing is stored |

A suggested graph naming, which the smoke test uses: one graph per module version (`…/model/<module>/<version>`) and one per export (`…/export/<module>/<date or batch>`). Provenance (PROV-O) travels in the same graph as the data it describes.

## What to know

- **Validation runs on request, not on commit.** A `PUT` of data that breaks the shapes succeeds; the exporter asks `/dataset/shacl` afterwards (or before, with the posted-shapes form on a draft graph). Validation on commit is a later step ([plan](../plan/2026-09-30-graphdb-parity-plan.md), U7).
- **SHACL Core only.** `sh:sparql` constraints aren't evaluated yet.
- **Updates without `GRAPH`** write to, and delete from, the default graph only, also with `store.default_graph = "union"`. The exporter should name the graph in updates.
- **The shapes graph is an ordinary graph:** a query over all graphs sees the shapes too. Validation leaves it out of the data by itself.
- **Inferred statements** live in the default graph. `GET /dataset/data?graph=…` returns what was exported, not what was inferred from it; queries see both unless they say `infer=false`.
- **A consistency rejection** (with `owl2-rl`) is a 400 with an explanation: the violated rule, the statements that clash, and the likely trigger in the export.

## Trying it

```bash
NRESE_DEFAULT_GRAPH=union NRESE_REASONING_MODE=owl2-rl cargo run -p nrese-server
scripts/smoke-dmw-export.sh http://127.0.0.1:8080
```

The script exports a small module with data and provenance (`fixtures/integration/dmw/`), exports it again and compares, asks a competency and a provenance question, changes data with SPARQL Update, validates conforming and non-conforming data, and removes what it created. It stops at the first difference and says what differed.
