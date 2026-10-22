# /verify: HTTP evidence from a real server

`verify.py` drives a real nrese-server through a scenario file and keeps the evidence: how agents
prove HTTP-level behaviour instead of claiming it. The skill (when and where to run it, how to
cite it): [../../.claude/skills/verify/SKILL.md](../../.claude/skills/verify/SKILL.md). The phases,
the commands and the exit codes: `verify.py`'s docstring (`python scripts/verify/verify.py -h`).

| File | What |
|---|---|
| `verify.py` | The command: `run`, `selftest`, `doctor`, `check`, `jobs` |
| `verifykit/scenario.py` | Reads and checks a scenario file (the schema below) |
| `verifykit/checks.py` | The predicates, JSON paths and row counts, and their failure messages |
| `verifykit/client.py` | One HTTP exchange and its evidence |
| `verifykit/server.py` | Launch (temporary data directory, free port, settings) and cleanup |
| `verifykit/selftest.py` | Every check held and broken, and broken HTTP on purpose: each failure with its message |
| `verifykit/jobs.py`, `build-on-host.sh` | The fleet jobs: a debug build on gamergrill, then the run |
| `scenarios/` | Scenarios that prove something about today's server |

Tests: `benches/suite/tests/test_verify.py` (the suite's contracts; no server built).

## Scenario files

TOML, one file per scenario. Every key is known: a misspelt one is an error naming it, so a typo
can't drop an expectation. `verify.py check FILE...` reads them without starting anything.

```toml
name = "gsp-named-graph"          # optional: the file's stem; names its evidence directory
description = """What it proves, and where that behaviour is specified."""

[server]                          # optional: the server this scenario needs
set = { "reasoner.mode" = "owl2-dl" }   # config file keys, as `--set KEY=VALUE` (over store.mode = "on-disk")
env = { NRESE_AUTH_ADMIN_TOKEN = "t" }  # NRESE_*, RUST_LOG, RUST_BACKTRACE: for what --set refuses (secrets)

[vars]                            # optional: ${name} anywhere in a step
graph = "http://example.com/g1"

[[step]]
name = "PUT creates it"           # required: the summary lists each step by it
method = "PUT"                    # GET (default), HEAD, POST, PUT, PATCH, DELETE, OPTIONS
path = "/dataset/data"            # from /: the runner gives the server's base
params = { graph = "${graph}" }   # the URL's query, encoded (a SPARQL query goes here)
headers = { Content-Type = "text/turtle" }
auth = { basic = ["alice", "${password}"] }   # or { bearer = "${token}" }
body = """..."""                  # one payload at most: body (text), body_file (beside the file),
                                  # json = { ... } (Content-Type set), form = { ... } (urlencoded)
timeout_s = 60                    # the whole exchange: connect, send, headers, body (default 60)
read_limit_mib = 16               # what the checks read of the body (default 16; all of it is hashed)

[step.expect]                     # required, with status
status = 201                      # or [200, 204]
headers = { location = "/x", nrese-completeness = { contains = "regime=owl2-dl" } }
body = { contains = "..." }       # or a list of predicates
json = { "$.results.bindings[*].x.value" = { contains = "http://example.com/x" }, "$.id" = "x" }
rows = 2                          # SPARQL result rows (JSON, XML, CSV, TSV) or N-Triples/N-Quads statements

[step.capture]                    # values for later steps: ${repo_path}
repo_path = { json = "$.path" }
txid = { header = "location", regex = "transactions/([^/?]+)" }   # the first group, else the match
token = { body = true, regex = "\"token\":\"([^\"]+)\"" }

[[step]]
name = "restart on the same data"
restart = true                    # stops the server and starts it again on its data (not with --url)
```

**Predicates** (a table; every key in it must hold):

| Key | Holds when |
|---|---|
| `equals` | the value is this (a bare value means `equals`; for an object write `{ equals = {...} }`) |
| `contains`, `not_contains` | text: the substring is (not) in it; a list: the element; an object: the key |
| `matches` | a regular expression (Python's `re.search`) matches the text |
| `length` | a list, object or text has this length |
| `present` | `true`: the header or JSON path exists (a `[*]` path: selects something); `false`: it doesn't (headers and JSON only) |

**JSON paths:** `$`, then `.key`, `['key with dots']`, `[0]` and `[*]` (every element: the value
is then the list of what each element holds, elements without the rest of the path left out).
A `[*]` path's list may be empty, and `contains`/`not_contains` on an empty list say nothing of
existence: to claim that something is there, say so with `present = true` (a nonempty
selection), `length = N`, or `contains` the value itself:

```toml
json = { "$.results.bindings[*].x" = { present = true },          # at least one row binds x
         "$.results.bindings[*].x.value" = { contains = "http://example.com/x" } }
```

**Complete exchanges only.** An exchange fails, before any expectation is read or any value
captured, when it didn't complete within `timeout_s` (the deadline covers connecting, sending,
every header and every body byte: a server trickling its bytes can't hold a step past it) or its
body is shorter than its framing announced (Content-Length, chunks). Such a step's failure is the
exchange's (`no response: …` before the headers ended, `incomplete response: …` after), and a
step needing a value it would have captured isn't run.

**Variables:** `${base}` and `${port}` (the runner's, changed by a restart if the port had to
change), `[vars]`, and what earlier steps captured. A name none of them defines is an error
before the server starts; a step whose captured value is missing (its step failed) is not run.

**What a run never does:** judge time. Timings are recorded as evidence only; a scenario proves
behaviour. Timing belongs to the fleet's timed hosts and the release gate.

## Evidence

```
EVIDENCE/summary.md, summary.json        every scenario's steps, passed or failed and why
EVIDENCE/<scenario>/scenario.toml        the file as run
                    launch.json          every attempt to start the server (argv, port, pid,
                                         outcome, exit code if it failed, seconds until ready),
                                         its settings, the cleanup
                    doctor.json          /healthz, /readyz, /version, /api/v1/repositories
                    server.log           the server's output, every attempt marked
                    steps/NN-<name>.json the request and response (headers, the body's SHA-256,
                                         length and first 64 KiB in .request.body/.response.body),
                                         timings, the checks' failures, what was captured
```

A response body's record says how much of it the evidence holds and how much of it came:
`saved_complete` (the saved file is every byte received), `transport_complete` (what was received
is the whole message its framing announced), `declared_bytes` (its Content-Length, if any).

An interrupted run (SIGTERM from the fleet, Ctrl-C) stops its servers, removes their directories
and writes its summary, titled INTERRUPTED: the scenario it cut has failed, later ones weren't run.

summary.json also says where it ran (host, fleet job) and, on Linux, the peak memory of the
runner's cgroup: under fleet the step's, the runner and its servers together.
