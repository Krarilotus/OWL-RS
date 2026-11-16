# The first example: test and last run

The first example, the README's [first answer](../../README.md#a-first-answer) and the
[tutorial](../tutorial.md)'s retraction, validation and upload, is also a test:
`scripts/verify/scenarios/first-example.toml` sends each of its steps to a running server and
checks the answer. This page records what it sends, what it expects, and what the last run got.

## The last run

- The run was on 16 November 2026, 16:03 (CET).
- The code was commit `617d4665ecbe75bb9e799d55d953c0d7569d65df`; the server reported version 1.5.0.
- The server was a debug build of `nrese-server` from that commit (built with `--locked`, Rust
  1.98.1) on a Linux x86-64 machine. The runner started it with the scenario's settings,
  `reasoner.mode = owl2-dl` and `shacl.gate = enforce`, on an on-disk store, and stopped it at the
  end.
- The runner was `python3 scripts/verify/verify.py run --server PATH scripts/verify/scenarios/first-example.toml`,
  with Python 3.12.3.
- 15 of 15 steps passed.

## The quick start, last followed

- On 15 November 2026, 23:37 (CET), at commit `53ec69a59603e37c935ec6dae10e912a27259d09`.
- The server was built and started as the README's [quick start](../../README.md#quick-start) says
  (`docker build`, with `--build-arg CARGO_BUILD_JOBS=4`, in 382 s, then `docker run` with
  `NRESE_REASONING_MODE=owl2-dl` and `NRESE_SHACL_GATE=enforce`, a fresh volume and another host
  port) on a Linux x86-64 machine, and was ready at the first try of the README's wait line.
- The README's update and its two queries were sent as written there, with `curl -i`. The query
  answered `200`, `nrese-completeness: complete; regime=owl2-dl`, rows `ex:tom` and `ex:rex`, and
  with `infer=false` `complete` and no rows.
- The scenario's first 10 steps, all it had then, passed against that server, run with
  `python3 scripts/verify/verify.py run --url http://localhost:PORT scripts/verify/scenarios/first-example.toml`.

## The steps

`ex:` is `http://example.org/`. Updates and queries are a `POST` with a form body; an upload is a
`PUT` of the default graph (`/dataset/data?default`) with a Turtle body. "All animals" is the query
`SELECT ?x WHERE { ?x a ex:Animal }`. A refusal is expected as a `400` with
`application/problem+json`, title `SHACL Reject`, type `https://nrese.dev/problems/shacl-reject`,
and a detail naming the focus node.

| # | Sends | Expects | Got |
|---|---|---|---|
| 1 | update: `INSERT DATA { ex:Cat rdfs:subClassOf ex:Animal . ex:Dog rdfs:subClassOf ex:Animal . ex:Pet rdfs:subClassOf [ owl:unionOf (ex:Cat ex:Dog) ] . ex:tom a ex:Cat . ex:rex a ex:Pet }` | 200 or 204 | 204 |
| 2 | query: all animals | 200, `NRESE-Completeness` equal to `complete; regime=owl2-dl`, two rows, `ex:tom` and `ex:rex` | 200, `complete; regime=owl2-dl`, two rows: `ex:tom`, `ex:rex` |
| 3 | query: all animals, `infer=false` | 200, `NRESE-Completeness` equal to `complete`, no rows | 200, `complete`, no rows |
| 4 | update: `DELETE DATA { ex:tom a ex:Cat . ex:rex a ex:Pet }` | 200 or 204 | 204 |
| 5 | query: all animals | 200, no rows | 200, no rows |
| 6 | update: insert the shape `ex:AnimalShape` (every `ex:Animal` needs an `ex:name`) into the shapes graph `<http://rdf4j.org/schema/rdf4j#SHACLShapeGraph>` | 200 or 204 | 204 |
| 7 | update: `INSERT DATA { ex:felix a ex:Cat }` | a refusal naming `ex:felix` | 400: `the commit introduces 1 SHACL result(s) and 0 failure(s); first: focus node <http://example.org/felix>, <http://www.w3.org/ns/shacl#MinCountConstraintComponent>` |
| 8 | query: all animals | 200, no rows | 200, no rows |
| 9 | update: `INSERT DATA { ex:felix a ex:Cat ; ex:name "Felix" }` | 200 or 204 | 204 |
| 10 | query: all animals | 200, one row, `ex:felix` | 200, one row: `ex:felix` |
| 11 | upload: `ex:oscar a ex:Animal .` | a refusal naming `ex:oscar` | 400: `the commit introduces 1 SHACL result(s) and 0 failure(s); first: focus node <http://example.org/oscar>, <http://www.w3.org/ns/shacl#MinCountConstraintComponent>` |
| 12 | query: all animals | 200, one row, `ex:felix` (nothing of the refused upload is stored) | 200, one row: `ex:felix` |
| 13 | upload: `ex:oscar a ex:Animal ; ex:name "Oscar" .` | 204 | 204 |
| 14 | query: all animals | 200, one row, `ex:oscar` (the upload replaced the default graph) | 200, one row: `ex:oscar` |
| 15 | update: `INSERT DATA { ex:felix a ex:Animal }` | a refusal naming `ex:felix` (the shape outlived the upload) | 400: `the commit introduces 1 SHACL result(s) and 0 failure(s); first: focus node <http://example.org/felix>, <http://www.w3.org/ns/shacl#MinCountConstraintComponent>` |

## Run it yourself

You need Python 3.11 or later (the runner uses only its standard library). Start a fresh server as
the README's [quick start](../../README.md#quick-start) says, then run this from the repository's
root:

```bash
python3 scripts/verify/verify.py run --url http://localhost:8080 scripts/verify/scenarios/first-example.toml
```

It prints one line per step and writes every request and response, with a summary, to a new folder
under `tmp/verify/`. It exits 0 when every step passed, 1 when one failed.
