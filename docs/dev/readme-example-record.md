# The README's first example: test and last run

The README's [first example](../../README.md#a-first-example-inference-retraction-validation)
is also a test: `scripts/verify/scenarios/readme-first-example.toml` sends each of its steps to a
running server and checks the answer. This page records what it sends, what it expects, and what
the last run got.

## The last run

- **When:** 13 November 2026, 22:57 (CET).
- **Code:** commit `78b4a49b7becb0df1decc3f858d8ddf7c8166286`, package version 0.1.0.
- **Server:** built and started as the README's quick start says (`docker build -t nrese .`, then
  `docker run` with `NRESE_REASONING_MODE=owl2-rl` and `NRESE_SHACL_GATE=enforce`, a fresh volume)
  on a Linux x86-64 machine.
- **Runner:** `python3 scripts/verify/verify.py run --url http://127.0.0.1:8080 scripts/verify/scenarios/readme-first-example.toml`.
- **Result:** 9 of 9 steps passed.

## The steps

Every step is a `POST` with a form body; `ex:` is `http://example.org/`. "All animals" is the query
`SELECT ?x WHERE { ?x a ex:Animal }`.

| # | Sends | Expects | Got |
|---|---|---|---|
| 1 | update: `INSERT DATA { ex:Cat rdfs:subClassOf ex:Animal . ex:tom a ex:Cat }` | 200 or 204 | 204 |
| 2 | query: all animals | 200, `NRESE-Completeness` containing `sound-only; regime=owl2-rl`, one row, `ex:tom` | 200, `sound-only; regime=owl2-rl; …`, one row: `ex:tom` |
| 3 | update: `DELETE DATA { ex:tom a ex:Cat }` | 200 or 204 | 204 |
| 4 | query: all animals | 200, no rows | 200, no rows |
| 5 | update: insert the shape `ex:AnimalShape` (every `ex:Animal` needs an `ex:name`) into the shapes graph `<http://rdf4j.org/schema/rdf4j#SHACLShapeGraph>` | 200 or 204 | 204 |
| 6 | update: `INSERT DATA { ex:felix a ex:Cat }` | 400, `application/problem+json`, title `SHACL Reject`, type `https://nrese.dev/problems/shacl-reject`, detail naming `focus node <http://example.org/felix>` | 400: `the commit introduces 1 SHACL result(s) and 0 failure(s); first: focus node <http://example.org/felix>, <http://www.w3.org/ns/shacl#MinCountConstraintComponent>` |
| 7 | query: all animals | 200, no rows | 200, no rows |
| 8 | update: `INSERT DATA { ex:felix a ex:Cat ; ex:name "Felix" }` | 200 or 204 | 204 |
| 9 | query: all animals | 200, one row, `ex:felix` | 200, one row: `ex:felix` |

## Run it yourself

You need Python 3.11 or later (the runner uses only its standard library). Start a fresh server as
the README's quick start says, then run the runner command above from the repository's root. It
prints one line per step and writes every request and response, with a summary, to a new folder
under `tmp/verify/`. It exits 0 when every step passed, 1 when one failed.
