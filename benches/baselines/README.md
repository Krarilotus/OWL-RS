# Benchmark baselines

Recorded reference numbers that later milestones are measured against. Each file is a raw harness report; this README records how it was produced.

## `v1-write-scaling-reasoning-disabled.json`

- **What:** engine v1 (Oxigraph-backed), reasoning `disabled`, in-memory store, write-scaling to 1 M triples.
- **Why:** the Milestone 1 gate P1 requires a one-triple insert at 10 M triples to take under 5 ms. v1 needs about 3 s at 1 M, growing linearly (audit finding F1).
- **Date / commit:** 2026-09-25, branch `refactor/engine-v2` (Milestone 0 working tree, before any engine v2 code).
- **Machine:** AMD Ryzen 7 7800X3D (8 cores), 63 GB RAM, Windows 11, release build.
- **Reproduce:**

  ```powershell
  $env:NRESE_BIND_ADDR = "127.0.0.1:18600"
  $env:NRESE_MAX_RDF_UPLOAD_BYTES = "200000000"
  $env:NRESE_UPDATE_TIMEOUT_MS = "600000"
  $env:NRESE_GRAPH_WRITE_TIMEOUT_MS = "600000"
  cargo run --release -p nrese-server
  # second terminal
  cargo run --release --manifest-path benches/nrese-bench-harness/Cargo.toml -- `
    write-scaling --nrese-base-url http://127.0.0.1:18600 `
    --steps 10000,100000,500000,1000000 `
    --report-json benches/baselines/v1-write-scaling-reasoning-disabled.json
  ```

| Triples | Load step | Load throughput | 1-triple insert p50 | COUNT query |
|---|---|---|---|---|
| 10,000 | 38 ms | 263,157 t/s | 19 ms | 0 ms |
| 100,000 | 1,078 ms | 83,487 t/s | 238 ms | 5 ms |
| 500,000 | 21,833 ms | 18,320 t/s | 1,430 ms | 23 ms |
| 1,000,000 | 73,634 ms | 6,790 t/s | 3,009 ms | 46 ms |

## Engine v2, storage layer only (`nrese-engine` example `insert_latency`)

- **What:** the new engine without SPARQL, HTTP or reasoning. The dataset is loaded through one transaction, then 2,000 sequential one-quad commits run. Durable runs `fsync` the WAL on every commit (`SyncPolicy::EveryCommit`), with background compaction and checkpointing on.
- **Why:** it separates the storage cost from the end-to-end P1 gate, which is measured once the pipeline (P1) uses the engine.
- **Date / machine:** 2026-09-25, same machine as above (NVMe SSD, NTFS).
- **Reproduce:** `cargo run --release -p nrese-engine --example insert_latency -- <quads> 2000 [data-dir]`

| Quads | Mode | Load (one txn) | 1-quad commit p50 | p99 | max | Checkpoint | Recovery |
|---|---|---|---|---|---|---|---|
| 1 M | in memory | 0.55 s | 2.6 µs | 24.9 µs | 0.3 ms | n/a | n/a |
| 10 M | in memory | 8.0 s | 3.8 µs | 39.7 µs | 0.5 ms | n/a | n/a |
| 1 M | durable | 0.64 s | 1.9 ms | 9.1 ms | 36.5 ms | 0.10 s | 0.13 s |
| 10 M | durable | 8.6 s | 1.9 ms | 4.4 ms | 240 ms | 0.92 s | 2.0 s |

**Notes:**
- Durable latency is dominated by `FlushFileBuffers`; the engine's own work is a few µs.
- The 240 ms maximum at 10 M coincides with the background checkpoint triggered by the 500 MB load record.
- Index memory is about 190 bytes/quad (six uncompressed permutations); block compression is Pf1.
- For comparison, v1 needs about 3 s per one-triple insert at 1 M (table above).
