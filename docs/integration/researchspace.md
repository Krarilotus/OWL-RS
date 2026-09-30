# ResearchSpace on NRESE

How to run the ResearchSpace platform with NRESE as its triple store, what was tested, and what doesn't work yet.

Status on 30 September 2026: tested with the image `researchspace/platform-ci:latest` on Linux, through ResearchSpace's HTTP interfaces (`scripts/smoke-researchspace.sh`). Its pages and forms were not tested in a browser.

## Running both

```bash
docker compose -f ops/researchspace/docker-compose.yml up --build
scripts/smoke-researchspace.sh          # once ResearchSpace has started (about 20 s)
```

| | Address | |
|---|---|---|
| ResearchSpace | `http://localhost:10214` | default login `admin` / `admin` |
| NRESE | `http://localhost:10215` | console at `/console`, SPARQL at `/dataset/sparql` |

Both are published on localhost only. ResearchSpace starts with a default password and NRESE without authentication, so change the password and configure `NRESE_AUTH_MODE` before publishing them elsewhere.

## How ResearchSpace is connected

ResearchSpace talks to its store through RDF4J's SPARQL repository, which takes one URL for queries and updates. The compose file sets it to NRESE's combined endpoint:

```
-Dconfig.environment.sparqlEndpoint=http://nrese:8080/dataset/sparql
```

NRESE needs one setting for ResearchSpace: `NRESE_DEFAULT_GRAPH=union` (`store.default_graph = "union"`). ResearchSpace keeps every resource in a named graph of its own and queries without naming graphs, as Blazegraph lets it.

An existing ResearchSpace installation is moved the same way: point `sparqlEndpoint` at NRESE and load the data (export from the old store as N-Quads, then `nrese-server load`, or `PUT` each graph to `/dataset/data`).

## What was tested and works

| What | How it was checked |
|---|---|
| Startup: ResearchSpace connects to its repositories and loads its 645 system resources | 73,751 statements in 1,435 named graphs, written in about 1,400 updates; no warning or error in ResearchSpace's log |
| Queries through ResearchSpace's SPARQL endpoint | `SELECT` over all graphs, `GRAPH ?g`, `ASK`, `CONSTRUCT` as Turtle, property paths |
| Updates through ResearchSpace's SPARQL endpoint | `INSERT DATA` into a named graph, `DROP GRAPH` |
| Resources through ResearchSpace's LDP container API | create (201), found in its container, read as Turtle with ResearchSpace's provenance, delete |
| Pages | the start page is served after login |
| NRESE's answers | every request ResearchSpace sent during startup and the checks (about 4,400) was answered 200 or 204 |

## What doesn't work yet

| What | Why | Until then |
|---|---|---|
| Keyword search in the stock templates | They use Blazegraph's `bds:search`, which NRESE doesn't implement yet. The query runs without an error and finds nothing | Templates can search with `FILTER(CONTAINS(LCASE(?label), "…"))` or `REGEX`, which is slow on large data. Full-text search with a `bds:search` shim is the next step ([plan](../plan/2026-09-30-graphdb-parity-plan.md), U5) |
| The RDF4J repository type (`openrdf:HTTPRepository`) | NRESE has no RDF4J protocol yet | Use the SPARQL repository, as above (plan: U6) |
| Blazegraph query hints (`hint:`) and other `bd:` extensions | Not implemented | They are triple patterns NRESE doesn't know: a query that uses one finds nothing. Remove them from adapted templates |
| Federation (`SERVICE`) to outside endpoints | Not enabled | ResearchSpace's own Ephedra federation runs inside ResearchSpace and is unaffected |

## What changed in NRESE for ResearchSpace

Found by running it, and now covered by tests:

- One endpoint for queries and updates, and content negotiation for RDF4J's long weighted `Accept` lists.
- The default graph as the merge of all graphs.
- `CLEAR GRAPH` and `DROP GRAPH` of a graph that holds nothing succeed: RDF4J clears a graph before it writes it.
- Request size limits follow the policy (updates up to 16 MiB by default): ResearchSpace writes its larger system graphs in single updates.
