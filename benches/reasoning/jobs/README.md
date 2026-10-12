# The reasoning home-turf jobs

Measurement-queue jobs ([../../suite/queue.py](../../suite/queue.py)) for this week's reasoning
families: `lubm` (RDFox's L and LE programs), `claros`, `ldbc-spb`, `datalog-graphs`,
`commit-latency`. Every system of a job runs on Office-PC (Phuoc-Yu has no Docker), one job
at a time.

```sh
python benches/suite/queue.py submit benches/reasoning/jobs/qualify-lubm-rdfox.toml
```

- **`qualify-*`** (window `any`: they count, they don't time) prepare the data and run each
  engine once with `--qualify`: every derived relation counted and checked. They write
  `~/owlrs/results/<id>/qualify/qualified.json`, the totals the timed runs must reproduce.
- **`timed-*`** (window `quiet`: nights, or while the owner frees the PC) refuse to start
  without that file (`--qualified`), then run three interleaved repetitions at 4 CPUs and
  the same memory cap for every engine.
- The binaries: `NRESE_BIN` is the server built from `16c3bfb` (the server's sources are
  unchanged on this branch since), SHA-256 in `~/nrese-reasoning-suite/bin/16c3bfb/SHA256SUMS`
  on the host.

Results of GraphDB stay on the host (`publish = permission`).
