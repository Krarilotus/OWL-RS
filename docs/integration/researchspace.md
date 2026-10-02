# ResearchSpace on NRESE

How to run the ResearchSpace platform with NRESE as its triple store, what was tested, and what doesn't work yet.

Status on 2 October 2026: tested with the image `researchspace/platform-ci:latest` on Linux, through ResearchSpace's HTTP interfaces (`scripts/smoke-researchspace.sh`, 8 steps, all pass: startup data, queries, updates, the LDP API, keyword search with `bds:search`, Blazegraph's query hints). Every SPARQL query in ResearchSpace's shipped templates (291) was also sent to NRESE: all that ResearchSpace sends as written parse and run; the others use ResearchSpace's own placeholders (`??`, its namespace prefixes), which it resolves before sending, or its own `SERVICE`s, which run inside it. Its pages and forms were not tested in a browser.

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
| Startup: ResearchSpace connects to its repositories and loads its system resources | 54,745 statements in 647 named graphs with the image of 2 October (73,751 in 1,435 with that of 30 September); no warning or error in ResearchSpace's log |
| Its search templates' Blazegraph check (an `ASK` with a query hint) and the knowledge map's hint prelude | true, and the hinted queries answer as without the hint (before 2 October both matched nothing, so the search fell back and the knowledge map's filters found nothing) |
| Queries through ResearchSpace's SPARQL endpoint | `SELECT` over all graphs, `GRAPH ?g`, `ASK`, `CONSTRUCT` as Turtle, property paths |
| Updates through ResearchSpace's SPARQL endpoint | `INSERT DATA` into a named graph, `DROP GRAPH` |
| Resources through ResearchSpace's LDP container API | create (201), found in its container, read as Turtle with ResearchSpace's provenance, delete |
| Pages | the start page is served after login |
| NRESE's answers | every request ResearchSpace sent during startup and the checks (about 4,400) was answered 200 or 204 |

## What doesn't work yet

| What | Why | Until then |
|---|---|---|
| Keyword search in the stock templates | Implemented since: Blazegraph's `bds:search` with relevance, rank, prefixes and phrases ([capability matrix](../spec/06-target-capability-matrix.md)); not yet re-run with ResearchSpace's templates | the end-to-end run of the G2 plan |
| The RDF4J repository type (`openrdf:HTTPRepository`) | Implemented since: the RDF4J REST protocol with repositories, statements, namespaces, contexts and transactions ([HTTP interface](../ops/http-api.md#rdf4j-protocol)); not yet re-run with ResearchSpace | the end-to-end run of the G2 plan |
| Blazegraph query hints (`hint:`) and other `bd:` extensions | Query hints are ignored since 2 October (dropped before planning, so a hinted query answers as without them). Other `bd:` extensions (`bd:serviceParam`, `bd:sample`) are not implemented | Remove them from adapted templates |
| Federation (`SERVICE`) to outside endpoints | Not enabled | ResearchSpace's own Ephedra federation runs inside ResearchSpace and is unaffected |

## What changed in NRESE for ResearchSpace

Found by running it, and now covered by tests:

- One endpoint for queries and updates, and content negotiation for RDF4J's long weighted `Accept` lists.
- The default graph as the merge of all graphs.
- `CLEAR GRAPH` and `DROP GRAPH` of a graph that holds nothing succeed: RDF4J clears a graph before it writes it.
- Request size limits follow the policy (updates up to 16 MiB by default): ResearchSpace writes its larger system graphs in single updates.
