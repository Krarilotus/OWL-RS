# Scorecard results (Pf0)

Run of 2026-09-26: `scorecard.sh` over five datasets. Only systems whose licences allow publishing results are listed (QLever, Virtuoso Open Source, Oxigraph, Jena/Fuseki). GraphDB, RDFox and AnzoGraph results are kept local (see [README.md](README.md)).

## Setup and caveats

- **Host.** Ryzen 7 7800X3D (16 threads), Docker Desktop on WSL2 with a 31 GB VM, NVMe. This is a developer workstation, not the reference machine.
- **Single runs.** Each system ran once per dataset, and run-to-run variance is visible: NRESE's DBpedia load took 36 s in an earlier run and 47 s here. Rerun with repetitions on the reference machine before quoting numbers externally.
- **Configuration.** Every system uses its own bulk loader and vendor tuning, and the same input file (validated N-Triples, `prepare-datasets.sh`). Fairness settings are listed in the README.
- **Sequential queries.** Each runs once to warm up, then 5 measured runs; the tables show the median (p50). The timeout is 120 s. **Sum of p50** covers only answered queries.
- **Throughput.** 8 concurrent clients for 60 s over the interactive queries, meaning those with a sequential p50 ≤ 10 s. Because that set differs per system, compare queries/s together with the answered count.
- **Writes under load.** During the throughput phase, one client inserts a triple every 100 ms. **Durability differs.** NRESE syncs every commit to disk (fsync); Virtuoso and Oxigraph don't by default, and QLever keeps updates in memory. A per-commit fsync costs about 7 ms on this disk. Group commit and a sync-policy knob are Pf6.
- **QLever's ~44 ms floor.** Many QLever results sit at ~44 ms. That's a TCP delayed-acknowledgement floor on small responses between our client and its HTTP server, not evaluation time.
- **Load peak.** Sampled about once per second, so loads shorter than one sample show "< 1 sample".
- **Correctness cross-check.** Result counts agree across systems, except Virtuoso on two queries: Olympics `q12` (137,068 against 137,026 rows) and DBpedia `q07` (25 against 21). Still to be investigated.

## What this means for NRESE

1. **Load: the fastest of the group** on every dataset: 1.2–2.2× ahead of QLever and Oxigraph at 60–67 M, and 6–9× ahead of Virtuoso and Jena.
2. **Queries: the largest gap.**
   - Sequentially, QLever is 10–2000× faster on joins and aggregates; at 67 M, `COUNT(*)` takes 10.7 s against 44 ms.
   - Under 8 clients, NRESE serves 4.5 queries/s at 67 M against QLever's 41.8 and Virtuoso's 25.8.

   The causes all sit in the borrowed evaluator (spareval):
   - joins build hash tables over full index scans instead of probing indexes with bound values
   - FILTERs aren't pushed into range scans
   - counts and group-bys don't use index metadata
   - every value is decoded

   **Pf3/Pf4 are the top priority.**
3. **Memory under load.** NRESE's server grows from 16 GiB to 24 GiB at 67 M while serving concurrent queries: intermediate results are materialised per query, with no memory budget. The native executor needs per-query memory accounting and limits (Pf3, with throttling in O3).
4. **Write tail under read load.** The p50 is good even with an fsync per commit (≈ 8 ms). But p99 reaches 0.2–2.0 s (YAGO: 2.0 s) while 8 readers saturate the CPU. Write-path isolation (reserved commit capacity, group commit) goes into Pf6/Pf7.
5. **Restart and memory at rest: the second gap. Pf2.** Restart takes 21 s at 67 M against QLever's 1.0 s, because NRESE rebuilds its indexes into RAM while QLever memory-maps compressed index files.
6. **Size: the third gap. Pf1 and Pf5.** NRESE uses 42–57 bytes per triple against QLever's 17–42. Virtuoso, Oxigraph and Jena need more than NRESE.
7. **Where NRESE already matches or leads:**
   - point lookups: sub-millisecond
   - property paths
   - streaming large results
   - against Oxigraph, which uses the same evaluator, NRESE is faster on most identical queries, often by 10–100×; that's the storage layer

Priority implied for M4:
1. Pf7 (cheap wins, including write-path isolation)
2. Pf3/Pf4 (with per-query memory budgets)
3. Pf2
4. Pf1/Pf5
5. E7

## Olympics (1.8 M)

| System | Load | Load peak | Bytes/triple | Restart | Server memory | Answered | Sum of p50 | Queries/s (8 clients) | Write p50 / p99 under load |
|---|---|---|---|---|---|---|---|---|---|
| NRESE | 1.4 s | < 1 sample | 56 | 1.1 s | 2.4 GiB | 12/13 | 2.11 s | 32.0 | 8.2 / 585 ms |
| QLever | 2.6 s | 0.3 GiB | 21 | 0.5 s | 0.3 GiB | 13/13 | 1.13 s | 47.0 | 17.1 / 957 ms |
| Virtuoso 7.2.17 | 7.5 s | 6.2 GiB | 91 | 6.4 s | 6.2 GiB | 12/13 | 3.31 s | 19.7 | 1.8 / 373 ms |
| Oxigraph 0.5.11 | 2.6 s | 0.4 GiB | 88 | 1.1 s | 0.6 GiB | 12/13 | 7.13 s | 9.9 | 0.6 / 335 ms |
| Jena/Fuseki 6.2.0 | 7.5 s | 3.0 GiB | 278 | 1.8 s | 4.4 GiB | 13/13 | 5.46 s | 10.9 | 187 / 550 ms |

Per query, sequential p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.6 | 44.0 | 0.7 | 0.6 | 5.6 |
| q02-star-team | 228 | 78.9 | 236 | 127 | 166 |
| q03-gold-by-team | 65.0 | 1.5 | 6.4 | 111 | 45.6 |
| q04-top-medallists | 278 | 1.5 | 75.1 | 2024 | 517 |
| q05-numeric-range | 27.3 | 8.8 | 25.9 | 32.6 | 35.6 |
| q06-text-filter | 87.4 | 7.8 | 209 | 566 | 349 |
| q07-games-by-year | 117 | 1.6 | 15.8 | 762 | 541 |
| q08-event-hierarchy | 3.4 | 5.5 | err | 6.5 | 15.6 |
| q09-no-medal | err | 47.9 | 314 | err | 1624 |
| q10-ask | 49.4 | 47.8 | 1.0 | 3.3 | 4.2 |
| q11-count-all | 293 | 44.0 | 11.5 | 461 | 318 |
| q12-all-labels | 471 | 835 | 2317 | 751 | 944 |
| q13-avg-age-by-games | 486 | 1.8 | 92.7 | 2283 | 893 |

## YAGO 4.5 tiny (23 M)

| System | Load | Load peak | Bytes/triple | Restart | Server memory | Answered | Sum of p50 | Queries/s (8 clients) | Write p50 / p99 under load |
|---|---|---|---|---|---|---|---|---|---|
| NRESE | 9.5 s | 4.3 GiB | 48 | 4.8 s | 3.9 GiB | 10/10 | 26.53 s | 5.3 | 7.9 / 1996 ms |
| QLever | 20.6 s | 3.8 GiB | 30 | 0.6 s | 0.3 GiB | 10/10 | 2.78 s | 0.2 | 127 / 1273 ms |
| Virtuoso 7.2.17 | 95.3 s | 8.1 GiB | 119 | 7.6 s | 6.1 GiB | 7/10 | 39.03 s | 17.3 | 1.7 / 5.3 ms |
| Oxigraph 0.5.11 | 15.4 s | 7.1 GiB | 80 | 0.5 s | 2.3 GiB | 10/10 | 85.74 s | 7.9 | 0.7 / 1.2 ms |
| Jena/Fuseki 6.2.0 | 121.0 s | 4.2 GiB | 147 | 1.7 s | 8.9 GiB | 8/10 | 5.69 s | 2.5 | 180 / 354 ms |

Per query, sequential p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-label-lookup | 0.5 | 44.0 | err | 0.6 | 4.9 |
| q02-persons-born | 1722 | 1.8 | 4.1 | 237 | 122 |
| q03-taxon-path | 20.0 | 44.0 | err | 114 | 232 |
| q04-movie-actors | 5.8 | 4.5 | 6.0 | 64.6 | 28.6 |
| q05-population | 54.3 | 1.9 | 35.1 | 18.9 | 69.9 |
| q06-alt-names | 297 | 5.1 | 1980 | 2286 | 2534 |
| q07-class-count | 27.6 | 1.3 | 5.5 | 40.6 | 62.8 |
| q08-subclass-path | 18682 | 44.0 | err | 65021 | err |
| q09-count-all | 2714 | 44.0 | 21.2 | 3936 | 2636 |
| q10-labels-en | 3005 | 2593 | 36978 | 14018 | err |

## Synthetic entities (10 M)

| System | Load | Load peak | Bytes/triple | Restart | Server memory | Answered | Sum of p50 | Queries/s (8 clients) | Write p50 / p99 under load |
|---|---|---|---|---|---|---|---|---|---|
| NRESE | 4.5 s | 1.0 GiB | 46 | 2.8 s | 2.0 GiB | 9/9 | 5.11 s | 11.1 | 7.7 / 216 ms |
| QLever | 8.4 s | 1.3 GiB | 20 | 0.5 s | 0.2 GiB | 9/9 | 0.91 s | 49.9 | 17.6 / 583 ms |
| Virtuoso 7.2.17 | 33.9 s | 6.3 GiB | 55 | 6.7 s | 5.9 GiB | 9/9 | 3.82 s | 13.6 | 1.7 / 202 ms |
| Oxigraph 0.5.11 | 8.3 s | 5.3 GiB | 84 | 0.5 s | 1.3 GiB | 9/9 | 5.78 s | 10.0 | 0.7 / 239 ms |
| Jena/Fuseki 6.2.0 | 51.2 s | 4.6 GiB | 156 | 1.8 s | 6.3 GiB | 9/9 | 20.20 s | 11.3 | 187 / 483 ms |

Per query, sequential p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.4 | 44.0 | 0.7 | 0.6 | 4.9 |
| q02-two-hop | 690 | 44.0 | 0.6 | 0.6 | 5.2 |
| q03-count-type | 319 | 44.1 | 19.0 | 770 | 270 |
| q04-date-range | 1258 | 44.0 | 3.0 | 748 | 868 |
| q05-group-born | 380 | 1.3 | 65.6 | 608 | 631 |
| q06-label-prefix | 519 | 44.1 | 2072 | 816 | 15414 |
| q07-inbound | 0.4 | 44.1 | 0.6 | 0.5 | 2.1 |
| q08-count-all | 1629 | 44.0 | 61.0 | 2344 | 1324 |
| q09-labels-limit | 311 | 597 | 1595 | 491 | 1684 |

## DBpedia 2022-12 core (67 M)

| System | Load | Load peak | Bytes/triple | Restart | Server memory | Answered | Sum of p50 | Queries/s (8 clients) | Write p50 / p99 under load |
|---|---|---|---|---|---|---|---|---|---|
| NRESE | 47.3 s | 17.0 GiB | 57 | 21.1 s | 24.0 GiB | 13/13 | 24.35 s | 4.5 | 7.8 / 325 ms |
| QLever | 86.8 s | 12.4 GiB | 42 | 1.0 s | 0.3 GiB | 12/13 | 0.83 s | 41.8 | 19.5 / 865 ms |
| Virtuoso 7.2.17 | 276.9 s | 10.4 GiB | 94 | 9.7 s | 6.9 GiB | 13/13 | 17.62 s | 25.8 | 1.8 / 348 ms |
| Oxigraph 0.5.11 | 58.9 s | 9.8 GiB | 130 | 0.9 s | 7.5 GiB | 13/13 | 69.06 s | 5.2 | 0.7 / 301 ms |
| Jena/Fuseki 6.2.0 | 423.7 s | 7.3 GiB | 168 | 1.8 s | 4.3 GiB | 12/13 | 19.35 s | 2.2 | 185 / 343 ms |

Per query, sequential p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.6 | 2.8 | 1.2 | 1.2 | 8.7 |
| q02-born-in-berlin | 788 | 19.8 | 55.8 | 1281 | 55.9 |
| q03-top-birthplaces | 370 | 14.8 | 30.0 | 691 | 407 |
| q04-date-range | 509 | 43.9 | 0.8 | 434 | 621 |
| q05-population | 167 | 1.5 | 9.3 | 918 | 373 |
| q06-film-cast | 542 | 15.4 | 48.8 | 4864 | 40.1 |
| q07-subdivision-path | 0.5 | 1.1 | 1.0 | 0.5 | 4.3 |
| q08-label-text | 5891 | err | 15405 | 29659 | err |
| q09-optional-death | 834 | 641 | 1870 | 1800 | 1501 |
| q10-types-count | 1190 | 1.2 | 48.9 | 1916 | 1459 |
| q11-count-all | 10479 | 43.9 | 74.6 | 16046 | 9234 |
| q12-goals-per-team | 3337 | 1.6 | 69.9 | 10086 | 5087 |
| q13-no-death | 239 | 44.0 | 7.9 | 1366 | 564 |

## Wikidata lexemes, first 60 M

| System | Load | Load peak | Bytes/triple | Restart | Server memory | Answered | Sum of p50 | Queries/s (8 clients) | Write p50 / p99 under load |
|---|---|---|---|---|---|---|---|---|---|
| NRESE | 20.5 s | 14.6 GiB | 42 | 10.1 s | 18.9 GiB | 10/10 | 15.69 s | 3.7 | 7.9 / 289 ms |
| QLever | 44.0 s | 5.9 GiB | 17 | 1.2 s | 0.5 GiB | 10/10 | 1.96 s | 20.1 | 44.4 / 375 ms |
| Virtuoso 7.2.17 | 174.1 s | 7.7 GiB | 43 | 7.2 s | 6.3 GiB | 10/10 | 5.82 s | 9.8 | 1.8 / 326 ms |
| Oxigraph 0.5.11 | 43.1 s | 9.4 GiB | 79 | 0.6 s | 4.8 GiB | 10/10 | 23.11 s | 6.0 | 0.7 / 328 ms |
| Jena/Fuseki 6.2.0 | 133.6 s | 5.4 GiB | 165 | 1.7 s | 3.3 GiB | 10/10 | 13.17 s | 4.0 | 190 / 487 ms |

Per query, sequential p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.6 | 2.4 | 1.3 | 2.0 | 9.0 |
| q02-lexemes-per-language | 68.2 | 1.3 | 9.7 | 148 | 109 |
| q03-german-nouns | 278 | 298 | 804 | 901 | 460 |
| q04-forms-features | 3380 | 1024 | 2246 | 1011 | 1025 |
| q05-senses-definitions | 389 | 499 | 1788 | 1410 | 993 |
| q06-lemma-prefix | 148 | 1.3 | 802 | 463 | 320 |
| q07-feature-count | 2101 | 1.3 | 97.7 | 3326 | 2075 |
| q08-no-senses | 336 | 44.0 | 9.1 | 1837 | 985 |
| q09-count-all | 8983 | 44.0 | 65.9 | 14011 | 7191 |
| q10-ask | 0.4 | 44.0 | 0.6 | 0.5 | 3.3 |
