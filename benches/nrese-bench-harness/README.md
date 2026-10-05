# nrese-bench-harness

A Rust HTTP client that drives SPARQL endpoints for the benchmark kits. Its own Cargo
workspace (own `Cargo.lock`, checked by `scripts/check-locks.sh`); the gate runs its tests.

```sh
cargo run --release --manifest-path benches/nrese-bench-harness/Cargo.toml -- <command> [options]
```

| Command | What | Used by |
|---|---|---|
| `query-mix` | runs a directory of queries against an endpoint: warm-up, repeated runs, fixed or seeded shuffled order, timings and result counts | the suite (`benches/suite`) |
| `write-scaling` | update latency as the store grows, with and without readers | the suite |
| `bench` | a query and update workload against NRESE and optionally a reference engine | by hand |
| `compat`, `pack`, `pack-validate`, `pack-matrix` | protocol compatibility cases and workload packs (`fixtures/compat/`, `fixtures/packs/`) compared between NRESE and a reference engine | by hand |
| `seed`, `generate` | load a seed dataset into both sides; generate a synthetic dataset | by hand |
| `catalog-sync` | refresh the vendored ontologies of `fixtures/catalog/ontologies.toml` into `fixtures/catalog-cache/` (see its README for provenance and licences) | by hand |

`cargo run … -- help` prints every option. Reports go where `--report-json` or `--report-dir` say; nothing is
written into the repository.
