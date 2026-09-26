# Scorecard results (Pf0)

Run of 2026-09-26: `scorecard.sh` over five datasets. Only systems whose licences allow publishing results are listed (QLever, Virtuoso Open Source, Oxigraph, Jena/Fuseki). GraphDB, RDFox and AnzoGraph results are kept local (see [README.md](README.md)).

## Setup and caveats

- **Host.** Ryzen 7 7800X3D (16 threads), Docker Desktop on WSL2 with a 31 GB VM, NVMe. This is a developer workstation, not the reference machine; rerun there before quoting numbers externally.
- **Configuration.** Every system uses its own bulk loader and vendor tuning, and the same input file (validated N-Triples, `prepare-datasets.sh`). Fairness settings are listed in the README.
- **Query timing.** Each query runs once to warm up, then 5 measured runs; the tables show the median (p50). The timeout is 120 s.
- **Sum of p50** covers only answered queries, so compare it together with the answered count.
- **QLever's ~44 ms floor.** Many QLever results sit at ~44 ms. That's a TCP delayed-acknowledgement floor on small responses between our client and its HTTP server, not evaluation time. Its true latency for those queries is lower.
- **Correctness cross-check.** Result counts agree across systems, except Virtuoso on two queries: Olympics `q12` (137,068 against 137,026 rows) and DBpedia `q07` (25 against 21). Still to be investigated.
- **Not yet measured:** throughput with concurrent clients, and writes under read load.

## What this means for NRESE

1. **Load: the fastest of the group** on every dataset.
   - At 67 M triples: 1.7–2.4× faster than QLever and Oxigraph, 7–12× faster than Virtuoso and Jena.
   - E7 (loader v2) would widen the lead, but this isn't where the gap is.
2. **Queries: the largest gap, by far.** QLever is 10–2000× faster on joins and aggregates. The worst cases at 67 M:
   - `COUNT(*)`: 10.7 s against 44 ms
   - GROUP BY over types: 1.2 s against 1.3 ms
   - SUM over a 3-way join: 3.4 s against 1.5 ms

   Virtuoso also beats NRESE on most joins, e.g. a two-hop join from one entity: 0.7 ms against 724 ms. The causes all sit in the borrowed evaluator (spareval):
   - joins build hash tables over full index scans instead of probing indexes with bound values
   - FILTERs aren't pushed into range scans
   - counts and group-bys don't use index metadata
   - every value is decoded

   **Pf3 (native BGP executor) and Pf4 (statistics and optimiser) are the top priority.**
3. **Where NRESE already matches or leads:**
   - point lookups: sub-millisecond
   - property paths
   - streaming large results: 474 ms against QLever's 865 ms for 137 k labels
   - against Oxigraph, which uses the same evaluator, NRESE is faster on most identical queries, often by 10–100× (up to 136× on DBpedia `q05`); that's the storage layer. Oxigraph wins a few small ones, e.g. YAGO `q05` and the Olympics `ASK`
4. **Restart and memory: the second gap. Pf2.**
   - Restart takes 22 s at 67 M against 1.1 s for QLever, because NRESE rebuilds its indexes into RAM.
   - Serving takes 16 GiB against 0.2 GiB for QLever, because QLever memory-maps compressed index files.
5. **Size: the third gap. Pf1 and Pf5.** NRESE uses 42–57 bytes per triple against QLever's 17–42. Virtuoso, Oxigraph and Jena need more than NRESE.

Priority implied for M4:
1. Pf7 (cheap wins)
2. Pf3/Pf4
3. Pf2
4. Pf1/Pf5
5. E7

## Olympics (1.8 M)

| System | Load | Bytes/triple | Restart | Server memory | Queries answered | Sum of p50 |
|---|---|---|---|---|---|---|
| NRESE | 1.4 s | 56 | 1.1 s | 0.8 GiB | 12/13 | 2.04 s |
| QLever | 2.4 s | 21 | 0.6 s | 0.1 GiB | 13/13 | 1.15 s |
| Virtuoso 7.2.17 | 7.1 s | 90 | 6.3 s | 5.6 GiB | 12/13 | 3.31 s |
| Oxigraph 0.5.11 | 2.6 s | 88 | 0.5 s | 0.1 GiB | 12/13 | 25.26 s |
| Jena/Fuseki 6.2.0 | 7.2 s | 278 | 2.0 s | 2.0 GiB | 13/13 | 5.63 s |

Per query, p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.7 | 44.0 | 0.7 | 0.6 | 5.9 |
| q02-star-team | 228 | 82.1 | 246 | 342 | 153 |
| q03-gold-by-team | 65.3 | 1.5 | 6.5 | 475 | 49.4 |
| q04-top-medallists | 273 | 1.6 | 72.5 | 8484 | 551 |
| q05-numeric-range | 27.0 | 9.3 | 25.4 | 36.6 | 52.7 |
| q06-text-filter | 92.8 | 8.2 | 218 | 1596 | 370 |
| q07-games-by-year | 118 | 1.6 | 15.8 | 2469 | 554 |
| q08-event-hierarchy | 3.3 | 6.1 | err | 15.7 | 20.8 |
| q09-no-medal | err | 43.9 | 323 | err | 1665 |
| q10-ask | 48.9 | 44.0 | 1.1 | 11.0 | 4.6 |
| q11-count-all | 267 | 44.2 | 11.9 | 504 | 301 |
| q12-all-labels | 474 | 865 | 2292 | 1016 | 960 |
| q13-avg-age-by-games | 448 | 2.0 | 100 | 10307 | 940 |

## YAGO 4.5 tiny (23 M)

| System | Load | Bytes/triple | Restart | Server memory | Queries answered | Sum of p50 |
|---|---|---|---|---|---|---|
| NRESE | 9.0 s | 48 | 5.6 s | 3.8 GiB | 10/10 | 26.42 s |
| QLever | 21.2 s | 30 | 0.6 s | 0.1 GiB | 10/10 | 2.79 s |
| Virtuoso 7.2.17 | 95.0 s | 120 | 7.4 s | 6.0 GiB | 7/10 | 38.94 s |
| Oxigraph 0.5.11 | 14.7 s | 80 | 0.5 s | 8.0 GiB | 9/10 | 113.98 s |
| Jena/Fuseki 6.2.0 | 118.6 s | 147 | 1.6 s | 6.3 GiB | 8/10 | 5.52 s |

Per query, p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-label-lookup | 0.4 | 44.0 | err | 0.4 | 6.0 |
| q02-persons-born | 1750 | 1.7 | 4.0 | 2099 | 122 |
| q03-taxon-path | 19.6 | 44.1 | err | 1664 | 225 |
| q04-movie-actors | 5.8 | 4.6 | 6.0 | 983 | 28.7 |
| q05-population | 54.5 | 2.0 | 35.4 | 21.2 | 68.3 |
| q06-alt-names | 314 | 5.1 | 1979 | 18854 | 2544 |
| q07-class-count | 28.1 | 1.3 | 5.3 | 53.3 | 74.2 |
| q08-subclass-path | 18672 | 43.9 | err | err | err |
| q09-count-all | 2581 | 44.0 | 19.0 | 4755 | 2451 |
| q10-labels-en | 2994 | 2603 | 36891 | 85549 | err |

## Synthetic entities (10 M)

| System | Load | Bytes/triple | Restart | Server memory | Queries answered | Sum of p50 |
|---|---|---|---|---|---|---|
| NRESE | 4.2 s | 46 | 2.8 s | 2.0 GiB | 9/9 | 5.20 s |
| QLever | 8.5 s | 20 | 0.5 s | 0.1 GiB | 9/9 | 0.91 s |
| Virtuoso 7.2.17 | 34.6 s | 56 | 7.1 s | 5.6 GiB | 9/9 | 3.92 s |
| Oxigraph 0.5.11 | 9.0 s | 84 | 0.6 s | 0.2 GiB | 9/9 | 8.11 s |
| Jena/Fuseki 6.2.0 | 50.7 s | 156 | 1.8 s | 2.1 GiB | 9/9 | 20.16 s |

Per query, p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.5 | 44.0 | 0.7 | 0.6 | 5.4 |
| q02-two-hop | 724 | 44.0 | 0.7 | 0.6 | 5.7 |
| q03-count-type | 340 | 44.0 | 19.1 | 758 | 284 |
| q04-date-range | 1300 | 44.0 | 3.2 | 931 | 854 |
| q05-group-born | 400 | 1.2 | 67.7 | 871 | 614 |
| q06-label-prefix | 538 | 44.0 | 2134 | 885 | 15548 |
| q07-inbound | 0.4 | 44.0 | 0.6 | 0.6 | 2.5 |
| q08-count-all | 1583 | 44.0 | 61.7 | 2718 | 1157 |
| q09-labels-limit | 314 | 606 | 1634 | 1949 | 1692 |

## DBpedia 2022-12 core (67 M)

| System | Load | Bytes/triple | Restart | Server memory | Queries answered | Sum of p50 |
|---|---|---|---|---|---|---|
| NRESE | 35.8 s | 57 | 22.2 s | 15.5 GiB | 13/13 | 24.55 s |
| QLever | 87.5 s | 42 | 1.1 s | 0.2 GiB | 12/13 | 0.85 s |
| Virtuoso 7.2.17 | 275.0 s | 94 | 7.8 s | 6.6 GiB | 13/13 | 18.67 s |
| Oxigraph 0.5.11 | 61.9 s | 130 | 0.8 s | 4.4 GiB | 10/13 | 114.52 s |
| Jena/Fuseki 6.2.0 | 439.5 s | 168 | 1.7 s | 3.8 GiB | 12/13 | 19.93 s |

Per query, p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.6 | 2.7 | 1.3 | 1.2 | 8.5 |
| q02-born-in-berlin | 768 | 21.1 | 60.5 | 1243 | 36.5 |
| q03-top-birthplaces | 385 | 15.6 | 35.9 | 655 | 424 |
| q04-date-range | 507 | 44.0 | 0.8 | 404 | 628 |
| q05-population | 161 | 1.6 | 9.2 | 21950 | 372 |
| q06-film-cast | 559 | 16.1 | 734 | err | 42.2 |
| q07-subdivision-path | 0.6 | 1.1 | 1.1 | 2.5 | 4.3 |
| q08-label-text | 5725 | err | 15671 | err | err |
| q09-optional-death | 886 | 661 | 1923 | 32684 | 1467 |
| q10-types-count | 1209 | 1.3 | 51.6 | 2444 | 1600 |
| q11-count-all | 10709 | 44.0 | 78.4 | 23220 | 9646 |
| q12-goals-per-team | 3398 | 1.5 | 93.4 | err | 5123 |
| q13-no-death | 239 | 44.2 | 7.0 | 31912 | 574 |

## Wikidata lexemes, first 60 M

| System | Load | Bytes/triple | Restart | Server memory | Queries answered | Sum of p50 |
|---|---|---|---|---|---|---|
| NRESE | 21.1 s | 42 | 10.2 s | 12.6 GiB | 10/10 | 16.75 s |
| QLever | 44.7 s | 17 | 1.0 s | 0.3 GiB | 10/10 | 1.95 s |
| Virtuoso 7.2.17 | 175.4 s | 43 | 6.8 s | 5.9 GiB | 10/10 | 5.84 s |
| Oxigraph 0.5.11 | 44.2 s | 79 | 0.7 s | 0.4 GiB | 10/10 | 129.46 s |
| Jena/Fuseki 6.2.0 | 132.9 s | 165 | 2.0 s | 1.4 GiB | 10/10 | 13.77 s |

Per query, p50 in ms (`err` = error or timeout after 120 s):

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena/Fuseki |
|---|---|---|---|---|---|
| q01-point-lookup | 0.6 | 2.6 | 1.4 | 2.1 | 8.0 |
| q02-lexemes-per-language | 73.1 | 1.2 | 10.2 | 135 | 95.2 |
| q03-german-nouns | 289 | 296 | 802 | 18725 | 485 |
| q04-forms-features | 3418 | 1029 | 2271 | 18650 | 1005 |
| q05-senses-definitions | 400 | 487 | 1799 | 25966 | 1002 |
| q06-lemma-prefix | 151 | 1.3 | 817 | 6931 | 331 |
| q07-feature-count | 2141 | 1.2 | 62.3 | 4729 | 2103 |
| q08-no-senses | 341 | 44.0 | 8.9 | 37863 | 1205 |
| q09-count-all | 9941 | 44.0 | 70.2 | 16459 | 7531 |
| q10-ask | 0.5 | 44.0 | 0.7 | 0.6 | 3.2 |
