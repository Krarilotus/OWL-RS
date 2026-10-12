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

- **GraphDB** has a Free licence on Office-PC (checked 12 Oct 2026 through its REST API:
  product `GRAPHDB_LITE`, valid, no expiry, at most 1 CPU core and 5 repositories, two
  concurrent queries). Its comparisons are the matched one-CPU scenario of
  [../../competitors/README.md](../../competitors/README.md); NRESE's own capacity is a line of
  its own. Results of GraphDB and RDFox stay on the host (`publish = permission`).
- **RDFox** has no licence: its pairs are skipped rows that name the missing licence, so the
  same jobs run it the day `RDFox.lic` is in the licences directory.
- **`profile-cspa`** (Phuoc-Yu, where profiling is allowed): perf over NRESE's CSPA
  materialisation, the attribution of its loss to Soufflé.
