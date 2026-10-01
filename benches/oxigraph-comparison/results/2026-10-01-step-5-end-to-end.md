# Step 5: the engine on its own RDF bundle against the baseline on Oxigraph

Run 1 October 2026 on the main development PC (AMD Ryzen 7 7800X3D, 16 threads, 64 GB,
Windows 11, Docker on WSL 2) with the benchmark suite:

```sh
benches/suite/suite.py run --systems nrese,nrese-oxigraph --workloads lubm,owl2bench,basics-mix \
  --tier lubm=1 --tier lubm=100 --tier owl2bench=rl-1 --tier basics-mix=olympics
```

- **nrese** is the server at commit `296ebc9`, the engine on `nrese-rdf`, `nrese-rdf-io`,
  `nrese-sparql-syntax` and `nrese-sparql-results`.
- **nrese-oxigraph** is the same server at `baseline/pre-oxigraph-migration` (`79983fe`),
  on the Oxigraph libraries.

Both were built in the same Rust image. Each pair ran three times, each time from a fresh
on-disk store:
1. load, with OWL 2 RL reasoning for LUBM and OWL2Bench;
2. restart;
3. count every statement;
4. the workload's queries through the harness: one warm-up, then three measured runs each.

Both are NRESE, so these numbers may be published.

## Results (medians of the three runs)

| Workload | System | Load ms | Statements | Queries correct | Sum of query medians ms | Serve peak MiB |
|---|---|---:|---:|---:|---:|---:|
| LUBM(1), OWL 2 RL | nrese | 641 | 168,089 | 14/14 | 51.5 | 65 |
| | nrese-oxigraph | 636 | 168,089 | 14/14 | 54.7 | 64 |
| LUBM(100), OWL 2 RL | nrese | 6,640 | 22,132,616 | 14/14 | 2,675 | 1,453 |
| | nrese-oxigraph | 7,129 | 22,132,616 | 14/14 | 2,678 | 1,440 |
| OWL2Bench RL-1 | nrese | 1,009 | 1,454,463 | 22/22 | 3,136 | 137 |
| | nrese-oxigraph | 1,048 | 1,454,463 | 22/22 | 3,089 | 143 |
| Olympics (basics mix) | nrese | 810 | 1,781,625 | 13/13 | 521 | 497 |
| | nrese-oxigraph | 936 | 1,781,625 | 13/13 | 520 | 486 |

- **The same results.** Every workload gave the same inferred and total statements on
  both, and every query answered correctly; the suite also checked that their answer
  counts agree.
- **Loading, reasoning included, is as fast or faster:** 0.93 of the baseline's time on
  LUBM(100), 0.87 on Olympics, 0.96 on OWL2Bench, equal on LUBM(1). The parsers are
  `nrese-rdf-io`'s (step 3).
- **Queries take the same time.** The sums differ by under 2%, which is within the spread
  of the runs.
  - OWL2Bench is dominated by its query 20: about 3.1 s on both.
  - Its query 2 took 16.2 ms against 15.3 ms.
- **Memory is the same** within the run-to-run spread.
- **Counting LUBM(100)'s statements** took 14–25 ms in one or two runs of each system and
  about 1.5 ms in the others: a cold cache, not a difference between them.

The migration's yardstick for step 5 is the same results or better and no regression on
these workloads. Both hold.
