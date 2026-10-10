# nrese-bench-harness

A Rust HTTP client that drives SPARQL endpoints for the benchmark kits. Its own Cargo
workspace (own `Cargo.lock`, checked by `scripts/check-locks.sh`); the gate runs its tests.

```sh
cargo run --release --manifest-path benches/nrese-bench-harness/Cargo.toml -- <command> [options]
```

| Command | What | Used by |
|---|---|---|
| `query-mix` | runs a directory of queries against an endpoint: warm-up, repeated runs, fixed or seeded shuffled order, timings and result counts | the suite (`benches/suite`) |
| `write-scaling` | batch-load throughput, single inserts and sequential COUNT latency as the store grows | the suite |
| `bench` | a query and update workload against NRESE and optionally a reference engine | by hand |
| `compat`, `pack`, `pack-validate`, `pack-matrix` | protocol compatibility cases and workload packs (`fixtures/compat/`, `fixtures/packs/`) compared between NRESE and a reference engine | by hand |
| `seed`, `generate` | load a seed dataset into both sides; generate a synthetic dataset | by hand |
| `catalog-sync` | refresh the vendored ontologies of `fixtures/catalog/ontologies.toml` into `fixtures/catalog-cache/` (see its README for provenance and licences) | by hand |

`cargo run … -- help` prints every option. Reports go where `--report-json` or `--report-dir` say; nothing is
written into the repository.

`write-scaling` accepts NRESE, a reference endpoint, or both. For GraphDB alone,
pass `--reference-kind graphdb --reference-base-url <repository-url>`. It checks
the numeric Person count and every expected probe subject after each step; an HTTP
200 response alone cannot qualify a result. Requested sizes are rounded down to
whole four-triple entities. Reports distinguish requested triples, actual entity
triples, newly loaded triples and cumulative probes. These checks do not compare
every generated label, date and link or validate an inferred closure.

The default `--reset true` drops all data in each selected benchmark repository.
`--reset false` preserves unrelated background data but requires an empty benchmark
namespace and no existing Person instances; it is not a resume mode. Compare only
runs with the same initial-state policy. Data checks run outside per-operation
latency samples. Concurrent readers and incremental reasoning are separate workloads.
