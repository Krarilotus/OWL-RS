# Tutorial: from the first answer to your own data

This goes on from the README's [quick start](../README.md#quick-start): the server `nrese` runs as
started there, with OWL 2 RL reasoning and the SHACL gate on, and the [first
query](../README.md#a-first-answer-inference) answered `ex:tom`. Each step here runs against that
server; `ex:` is `http://example.org/`.

## Retract, and the inference goes

Delete the premise, and the inference goes with it, in the same commit:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  DELETE DATA { ex:tom a ex:Cat }'
```

The same query now answers no rows: `{"head":{"vars":["x"]},"results":{"bindings":[]}}`.

## Validate on commit

Now a [shape, stored in the shapes graph](glossary.md#shape-shapes-graph-shacl-gate): every animal needs a name. A cat without one is
an animal by inference, so the gate rejects it:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  PREFIX sh: <http://www.w3.org/ns/shacl#>
  INSERT DATA { GRAPH <http://rdf4j.org/schema/rdf4j#SHACLShapeGraph> {
    ex:AnimalShape a sh:NodeShape ; sh:targetClass ex:Animal ;
      sh:property [ sh:path ex:name ; sh:minCount 1 ] } }'

curl -i http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  INSERT DATA { ex:felix a ex:Cat }'
```

```text
HTTP/1.1 400 Bad Request
content-type: application/problem+json

{"detail":"the commit introduces 1 SHACL result(s) and 0 failure(s); first: focus node <http://example.org/felix>, <http://www.w3.org/ns/shacl#MinCountConstraintComponent>","reasoner_reject":null,"request_id":"…","status":400,"title":"SHACL Reject","type":"https://nrese.dev/problems/shacl-reject"}
```

Nothing of the rejected commit remains: the query still answers no rows. With a name
(`INSERT DATA { ex:felix a ex:Cat ; ex:name "Felix" }`) the commit passes, and the query
answers `ex:felix`. When each part of a commit becomes visible, and what the checks see:
[What a commit does](what-it-covers.md#what-a-commit-does). The shape stays in the store: to run the example
again, [start over](#change-a-setting-start-over) first.

## This example is a test

Optional. The quick start's first query and the steps above are also a test:
[first-example.toml](../scripts/verify/scenarios/first-example.toml) sends each of them and
checks its answer. To run it you also need [Python](https://www.python.org/downloads/) 3.11
or later; start a fresh server as the [quick start](../README.md#quick-start) says, then, from
the repository's root:
`python3 scripts/verify/verify.py run --url http://127.0.0.1:8080 scripts/verify/scenarios/first-example.toml`.
Last run: 13 November 2026, on commit `78b4a49b7`, all 9 steps passed; what each step sends,
expects and got: [the record](dev/first-example-record.md).

## Your own data

Your own file, `my.ttl` in [Turtle](https://www.w3.org/TR/turtle/), into the same
server, counted, and still there after a restart:

```bash
curl -X PUT -H 'Content-Type: text/turtle' --data-binary @my.ttl 'http://localhost:8080/dataset/data?default'
curl http://localhost:8080/dataset/query --data-urlencode infer=false \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'
docker restart nrese
curl -sSf --max-time 5 --retry 60 --retry-delay 1 --retry-max-time 60 --retry-all-errors http://localhost:8080/readyz
curl http://localhost:8080/dataset/query --data-urlencode infer=false \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'
```

- The `PUT` answers `204 No Content` and replaces the [default graph](glossary.md#graph) with your file.
- `infer=false` reads the asserted statements only, so the count is your file's number of
  statements (for a file of three: `"value":"3"`). Without it, the inferred ones count too;
  under `owl2-rl` they include 32 that every store holds, an empty one too: each datatype the
  profile supports is an `rdfs:Datatype` ([why](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).
- After the restart the count is the same. On Docker's stop signal the server lets the
  requests in flight finish, closes the store and exits
  ([stopping](ops/server-setup.md#64-stopping)). Every commit is synced to the log
  before it is acknowledged, so not even a kill loses one
  ([durability](spec/02-storage-and-transactions.md#durability-on-disk-mode)).
- Other syntaxes take their media type as `Content-Type`. Large files load offline with
  `nrese-server load` ([server setup](ops/server-setup.md)).
- Queries are standard SPARQL ([SPARQL 1.1 Query Language](https://www.w3.org/TR/sparql11-query/#basicpatterns)).
  The names of the animals older than 5, in order, sent as `query=` like the count:
  `PREFIX ex: <http://example.org/> SELECT ?name WHERE { ?a a ex:Animal ; ex:age ?age ; ex:name ?name . FILTER(?age > 5) } ORDER BY ?name`.

## Change a setting, start over

Settings come from environment variables (or a file) when the server starts
([every setting](ops/config-reference.md)). To change one, recreate the container
with the same volume: the data stays. For example, to cap the memory that running queries
and updates may take together at 2 GiB:

```bash
docker rm -f nrese
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-rl -e NRESE_SHACL_GATE=enforce \
  -e NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES=2GiB nrese
curl -sSf --max-time 5 --retry 60 --retry-delay 1 --retry-max-time 60 --retry-all-errors http://localhost:8080/readyz
curl http://localhost:8080/version
```

`/version` shows the settings in force, here `"total_query_memory":2147483648` under
`budgets`. Which memory limit bounds what: [budgets](ops/config-reference.md#budgets).

A [repository](glossary.md#repository)'s reasoning changes while the server runs, without a restart:

```bash
curl -X PATCH -H 'Content-Type: application/json' --data '{"reasoning":"rdfs"}' \
  http://localhost:8080/api/v1/repositories/nrese
```

`nrese` is the default repository. The inferences are recomputed at once. The change is
kept in the data directory and holds after a restart, over `NRESE_REASONING_MODE`;
`{"reasoning":null}` goes back to the configured mode
([repositories](ops/http-api.md#engine-api-apiv1)).

To start over with no data: `docker rm -f nrese && docker volume rm nrese-data`, then
`docker run` as in the [quick start](../README.md#quick-start).

## Next

- Every command of `nrese-server`, offline loads and queries included:
  [commands](ops/server-setup.md#commands); the console's command-line client:
  [console CLI](dev/getting-started.md#frontend-cli).
- What it does, with the limits of each part: [what it covers](what-it-covers.md).
- Running it as a service: [server setup](ops/server-setup.md).
