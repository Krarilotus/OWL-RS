# Tutorial: from the first answer to your own data

This page picks up where the README's [quick start](../README.md#quick-start) ends. The server
`nrese` runs as started there, with OWL 2 DL reasoning and the SHACL gate on, and holds the
statements of the [first answer](../README.md#a-first-answer): cats and dogs are animals, every pet
is a cat or a dog, Tom is a cat and Rex a pet. Every step here runs against that server, and `ex:`
stands for `http://example.org/`. The query there, for every animal, answered `ex:tom` and
`ex:rex`, and the steps below ask it again:

```bash
curl http://localhost:8080/dataset/query --data-urlencode 'query=
  PREFIX ex: <http://example.org/>
  SELECT ?x WHERE { ?x a ex:Animal }'
```

## Retract, and the inferences go

Delete the premises, and what followed from them goes too, in the same commit:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  DELETE DATA { ex:tom a ex:Cat . ex:rex a ex:Pet }'
```

The query now answers no rows: `{"head":{"vars":["x"]},"results":{"bindings":[]}}`.

## Validate on commit

Now add a [shape](glossary.md#shape-shapes-graph-shacl-gate) that says every animal needs a name,
in the shapes graph NRESE reads by default, the one [RDF4J](https://rdf4j.org/documentation/programming/shacl/)
uses. A cat without a name is an animal by inference, so the gate refuses it:

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

{"detail":"the commit introduces 1 SHACL result(s) and 0 failure(s); first: focus node <http://example.org/felix>, <http://www.w3.org/ns/shacl#MinCountConstraintComponent>",
 "reasoner_reject":null,"request_id":"63f55350-5736-4b00-a1d4-1f63e22b0fe4","status":400,
 "title":"SHACL Reject","type":"https://nrese.dev/problems/shacl-reject"}
```

Its `detail` names `ex:felix` as the focus node and the `sh:minCount` constraint it fails. Three
more headers are left out here, and the body, which comes as one line, is broken between its
fields; the `detail` line is left whole.

Nothing of the rejected commit remains, so the query still answers no rows. With a name, the
commit passes:

```bash
curl http://localhost:8080/dataset/update --data-urlencode 'update=
  PREFIX ex: <http://example.org/>
  INSERT DATA { ex:felix a ex:Cat ; ex:name "Felix" }'
```

The query now answers `ex:felix`. [What a commit does](what-it-covers.md#what-a-commit-does)
explains when each part of a commit becomes visible and what the checks see. The shape stays in
the store, so to run the example again, [start over](#start-over) first.

## Start over

To begin again with an empty store, remove the container and its volume, and start the server
as the quick start does:

```bash
docker rm -f nrese && docker volume rm nrese-data
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-dl -e NRESE_SHACL_GATE=enforce nrese
until curl -sf http://localhost:8080/readyz; do sleep 1; done
```

## Your own data

Next, load your own file, `my.ttl` in [Turtle](https://www.w3.org/TR/turtle/), into the same
server. The upload replaces the [default graph](glossary.md#graph) with your file, so the
example's statements go. The SHACL gate checks an upload as it checks an update, and unless you
started over, the example's shape is still there, in its own graph: if your file says something
is an `ex:Animal` and gives it no `ex:name`, the upload is refused with a `400` that names it,
and nothing of the file is stored.

```bash
curl -X PUT -H 'Content-Type: text/turtle' --data-binary @my.ttl 'http://localhost:8080/dataset/data?default'
```

It answers `204 No Content`, so curl prints nothing; a refused upload prints the reason. Other
syntaxes take their media type as `Content-Type`. A `PUT` takes at most 128 MiB
([`budgets.upload_size`](ops/config-reference.md#budgets.upload_size)); larger files load offline
with `nrese-server load` ([loading data](ops/server-setup.md#loading-data)).

Count the statements you stored:

```bash
curl http://localhost:8080/dataset/query --data-urlencode infer=false \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'
```

`infer=false` reads the asserted statements only, so the count is the number of statements in
your file (for a file of three, `"value":"3"`). Without it the inferred ones count too, some of
them even in an empty store ([why](spec/reasoning-semantics.md#owl2-rl-owl-2-rlrdf-rules)).

Restart the server and count again:

```bash
docker restart nrese
until curl -sf http://localhost:8080/readyz; do sleep 1; done
curl http://localhost:8080/dataset/query --data-urlencode infer=false \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'
```

The count is the same. On Docker's stop signal the server lets the requests in flight finish,
closes the store and exits ([stopping](ops/server-setup.md#stopping)). Every commit is synced to
the log before it is acknowledged, so not even a kill loses one
([durability](spec/02-storage-and-transactions.md#durability-on-disk-mode)).

Queries are standard SPARQL ([SPARQL 1.1 Query Language](https://www.w3.org/TR/sparql11-query/#basicpatterns)).
This one gives the names of the animals older than 5, in order:

```bash
curl http://localhost:8080/dataset/query --data-urlencode 'query=
  PREFIX ex: <http://example.org/>
  SELECT ?name WHERE { ?a a ex:Animal ; ex:age ?age ; ex:name ?name . FILTER(?age > 5) }
  ORDER BY ?name'
```

## Change a setting

Settings come from environment variables (or a file) when the server starts
([every setting](ops/config-reference.md)). To change one, recreate the container
with the same volume, and the data stays. For example, to cap the memory that running queries
and updates may take together at 2 GiB:

```bash
docker rm -f nrese
docker run -d --name nrese -p 127.0.0.1:8080:8080 -v nrese-data:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-dl -e NRESE_SHACL_GATE=enforce \
  -e NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES=2GiB nrese
until curl -sf http://localhost:8080/readyz; do sleep 1; done
curl http://localhost:8080/version
```

`/version` shows the settings in force, here `"total_query_memory":2147483648` under
`budgets`. [Budgets](ops/config-reference.md#budgets) says which memory limit bounds what.

You can change a [repository](glossary.md#repository)'s reasoning while the server runs, without a
restart:

```bash
curl -X PATCH -H 'Content-Type: application/json' --data '{"reasoning":"rdfs"}' \
  http://localhost:8080/api/v1/repositories/nrese
```

`nrese` is the default repository, and its inferences are recomputed at once. The change is kept
in the data directory, so it holds after a restart and wins over `NRESE_REASONING_MODE`.
`{"reasoning":null}` goes back to the configured mode
([repositories](ops/http-api.md#engine-api-apiv1)).

## This example is a test

The quick start's first query, the retraction, the refused write and a checked upload are also a
test, [first-example.toml](../scripts/verify/scenarios/first-example.toml), which sends each
of them and checks the answer. To run it you also need [Python](https://www.python.org/downloads/)
3.11 or later. Start a fresh server as the [quick start](../README.md#quick-start) says, then run this from
the repository's root:

```bash
python3 scripts/verify/verify.py run --url http://127.0.0.1:8080 scripts/verify/scenarios/first-example.toml
```

[The record](dev/first-example-record.md) says when it last ran, on which commit, and what each
step sent, expected and got.

## Next

- [Commands](ops/server-setup.md#commands) lists every command of `nrese-server`, offline loads
  and queries included, and the console's own command-line client is described under
  [console CLI](dev/getting-started.md#frontend-cli).
- [Features and standards](what-it-covers.md) says what NRESE does, with the limits of each part.
- [Running a server](ops/server-setup.md) is about running it for others, and
  [backups](ops/backups.md) about keeping its data.
