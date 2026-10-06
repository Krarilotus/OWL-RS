# Benchmark baselines

Recorded reference numbers that later milestones are measured against. Each file is a raw harness report; this README records how it was produced.

**Retention.** A baseline committed here is small: the run's commit, machine and
configuration, its counts and correctness, and its key timings. A tool whose full reports
grow with every run keeps them outside git and commits a compact record that names the full
report by path, host and SHA-256. The fast suite does this (`fast/`, see
[../fast/README.md](../fast/README.md), "What is kept"): its full reports are in
`~/nrese-bench/reports/fast/`.

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

## Engine v2, end to end over HTTP (M1 gate P1)

- **What:** the release server on engine v2 with reasoning `disabled`, run through the same `write-scaling` command as the v1 baseline. The load goes through Graph Store POSTs; the one-triple inserts are SPARQL `INSERT DATA` requests through the full mutation pipeline. 200 samples per step; latencies in µs.
- **Files:**
  - `v2-write-scaling-reasoning-disabled.json` (in memory)
  - `v2-write-scaling-durable-reasoning-disabled.json` (`NRESE_STORE_MODE=on-disk`, fsync per commit, background checkpoints on)
- **Date / machine:** 2026-09-25, same machine as above.
- **Reproduce:** as for v1, with `--samples 200` and, for the durable run, `NRESE_STORE_MODE=on-disk` plus `NRESE_DATA_DIR`.

| Triples | Mode | HTTP load | 1-triple insert p50 | p99 | COUNT query |
|---|---|---|---|---|---|
| 1 M | in memory | 456 k t/s | 121 µs | 215 µs | 38 ms |
| 10 M | in memory | 431 k t/s | 216 µs | 417 µs | 560 ms |
| 1 M | durable | 414 k t/s | 2,054 µs | 2,570 µs | 37 ms |
| 10 M | durable | 401 k t/s | 2,048 µs | 3,859 µs | 367 ms |

**Notes:**
- **Gate P1** (< 5 ms at 10 M) is met in both modes. v1 needed 3,009 ms at 1 M and 6,790 t/s load.
- **Crash test:** after the durable run, the server was killed with `taskkill /F` and restarted. It recovered revision 1427 with exactly 10,000,800 quads (10 M loaded plus 800 inserts).
- **COUNT** grows linearly because spareval counts by scanning; native aggregation is Pf3/Pf4.

## Streaming a 10 M-row result (M1 gate Q1)

- **What:** `SELECT * WHERE { ?s ?p ?o }` over the 10 M-triple entity dataset (bulk-loaded, on disk), fetched with `curl` as TSV from the release server. Server memory is the working set sampled every 100 ms.
- **Date / machine:** 2026-09-26, same machine as above.

| Rows | Result size | Time | Server memory before | Peak during query |
|---|---|---|---|---|
| 10,000,000 | 1,019 MiB | 8.5 s | 2.023 GiB | 2.025 GiB (+1.9 MiB) |

Before this change, the whole serialised result (here 1 GB) was buffered in memory before the first byte was sent.

## Engine v2 bulk load (M1 gate E5)

- **What:** `nrese-server load` into an on-disk store. It parses N-Triples on all cores, interns in parallel batches, sorts once, builds the base run directly, and writes a checkpoint before publishing. The WAL isn't involved. Times are end to end, from process start to the published revision. Numbers come from the command's `bulk load published` / `bulk load complete` log lines; peak memory is the process's peak working set.
- **Data:** the `write-scaling` entity shape (type, language-tagged label, link, `xsd:date`; four triples per entity), written by the harness's `generate` command. 100 M triples is an 11 GB N-Triples file; 10 M is its first 10 M lines.
- **Date / machine:** 2026-09-25, same machine as above (16 threads, NVMe SSD).
- **Reproduce:**

  ```powershell
  cargo run --release --manifest-path benches/nrese-bench-harness/Cargo.toml -- generate --triples 100000000 --out entities-100m.nt
  $env:NRESE_STORE_MODE = "on-disk"; $env:NRESE_DATA_DIR = "store-100m"
  cargo run --release -p nrese-server -- load entities-100m.nt
  ```

| Triples | Total | Throughput | Parse + intern | Index build | Checkpoint | Peak memory | Checkpoint file | Restart (recovery) |
|---|---|---|---|---|---|---|---|---|
| 10 M | 3.4 s | 2.91 M t/s | 1.8 s | 0.83 s | 0.78 s | n/m | n/m | n/m |
| 100 M | 37.3 s | 2.68 M t/s | 21.5 s | 9.9 s | 5.9 s | 23.1 GB | 4.7 GB | 20.3 s |

**Notes:**
- For comparison, loading over HTTP through the mutation pipeline reaches about 430 k t/s (table above), and v1 reached 6,790 t/s.
- **Not yet compared with competitors.** QLever's index builder and GraphDB's ImportRDF still have to run on this machine with the same file. Until then, no claim relative to them is made.
- Restart time is dominated by rebuilding the six permutations and the dictionary hash table from the checkpoint. Persisted run files (Pf2) remove the rebuild.
