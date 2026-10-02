# Benchmark suite results, 2 October 2026

Main PC (Windows 11, Docker Desktop, 64 GB), `benches/suite/suite.py run`, one machine for
every system, each in its own container from a fresh store. Every system with a result
cache ran twice, cache off and cache on (its defaults); the queries ran in seeded shuffled
orders, the same for every system, one warm-up then the measured runs; first executions
are reported apart. The answer counts agree across systems, except Virtuoso's on two
queries (DBpedia q07 25 rows where all others have 21, Olympics q12 137,068 for 137,026).

Two batches:
- **Batch C** (all systems): NRESE as built at its start (the night's first commits).
- **Batch D** (NRESE alone, `960f1f0`): the current code, same workloads and settings.

GraphDB Free ran too; its results stay on the main PC (publication needs Ontotext's
permission, `benches/competitors/README.md`). RDFox, Stardog and AnzoGraph have no licence
here. RDF4J has no suite adapter yet. YAGO tiny's preparation failed in batch C (the
converter read Turtle as N-Triples, fixed in `fbdd899`); batch D ran it, the competitors
haven't yet.

## Basic workloads (load, store, queries)

Times in milliseconds, memory in MiB. "Queries" is the sum of the per-query medians of the
repeated runs; cache off unless marked.

### DBpedia core (67 M statements, 13 queries)

| System | Load | Load peak | Store | Restart | Queries | Queries, cache on | Serve peak |
|---|---:|---:|---:|---:|---:|---:|---:|
| **NRESE (now)** | **26,886** | **5,737** | 3,788 | **361** | **180** | **84** | 460 |
| NRESE (batch C) | 29,437 | 10,803 | 4,711 | 387 | 471 | 318 | 359 |
| QLever | 79,360 | 12,984 | **2,711** | 768 | 244,206 | 788 | 341 |
| Virtuoso | 253,722 | 10,629 | 6,054 | 6,700 | 18,104 | – | 6,675 |
| Oxigraph | 51,172 | 9,841 | 8,321 | 478 | 64,922 | – | 7,067 |
| Jena (TDB2) | 391,460 | 7,179 | 10,746 | 1,480 | 178,407 | – | 2,769 |

### Wikidata lexemes (60 M statements, 10 queries)

| System | Load | Load peak | Store | Restart | Queries | Queries, cache on | Serve peak |
|---|---:|---:|---:|---:|---:|---:|---:|
| **NRESE (now)** | **12,218** | **3,865** | 1,945 | **365** | **170** | **137** | 341 |
| NRESE (batch C) | 13,322 | 8,139 | 2,777 | 577 | 667 | 606 | 187 |
| QLever | 40,025 | 4,803 | **990** | 707 | 2,888 | 1,759 | 380 |
| Virtuoso | 166,391 | 7,851 | 2,538 | 8,014 | 5,624 | – | 6,081 |
| Oxigraph | 35,734 | 9,273 | 4,535 | 402 | 22,817 | – | 4,250 |
| Jena (TDB2) | 123,708 | 5,725 | 9,473 | 1,454 | 12,772 | – | 2,255 |

### Olympics (1.8 M statements, 13 queries)

| System | Load | Store | Queries | Queries, cache on |
|---|---:|---:|---:|---:|
| **NRESE (now)** | **841** | 82.8 | **139** | **92** |
| QLever | 2,068 | **36.7** | 1,422 | 1,090 |
| Virtuoso | 6,336 | 156 | 3,245 (12 of 13 answered) | – |
| Oxigraph | 1,777 | 151 | 7,124 (12 of 13) | – |
| Jena (TDB2) | 6,678 | 474 | 5,049 | – |

### Synthetic entities (10 M statements, 9 queries)

| System | Load | Load peak | Store | Queries | Queries, cache on |
|---|---:|---:|---:|---:|---:|
| **NRESE (now)** | **3,018** | **922** | 372 | **58** | **48** |
| QLever | 7,652 | 1,599 | **200** | 1,011 | 824 |
| Virtuoso | 31,787 | 6,467 | 542 | 3,976 | – |
| Oxigraph | 6,966 | 5,419 | 808 | 5,446 | – |
| Jena (TDB2) | 46,756 | 4,265 | 1,495 | 19,482 | – |

### Per query, cache off (medians, ms; NRESE now against batch C's competitors)

DBpedia core:

| Query | NRESE | QLever | Virtuoso | Oxigraph | Jena |
|---|---:|---:|---:|---:|---:|
| q01 point lookup | **0.7** | 3.5 | 2.5 | 1.5 | 5.4 |
| q02 born in Berlin | **6.5** | 60.4 | 58.8 | 44.7 | 64.3 |
| q03 top birthplaces | **4.4** | 14.9 | 31.4 | 465.5 | 1,041 |
| q04 date range | **0.8** | 44.4 | 1.4 | 294.4 | 666.7 |
| q05 population | **3.1** | 5.5 | 10.5 | 849.2 | 364.4 |
| q06 film cast | **6.8** | 37.9 | 725.6 | 4,499 | 60.1 |
| q07 subdivision path | **0.6** | 11.0 | 1.4 | 1.0 | 6.6 |
| q08 `CONTAINS` on labels | **28.0** | 242,941 | 15,231 | 28,572 | 157,834 |
| q09 OPTIONAL, 100 k rows | **99.4** | 614.8 | 1,876 | 1,663 | 1,838 |
| q10 types count | **1.1** | 40.0 | 51.0 | 1,866 | 1,583 |
| q11 count all | **0.6** | 489.8 | 68.4 | 16,119 | 8,589 |
| q12 goals per team | **27.1** | 137.9 | 82.9 | 9,401 | 5,307 |
| q13 no death date | **4.3** | 66.3 | 7.1 | 1,248 | 559.4 |

NRESE is the fastest on every query of the four datasets (Wikidata, Olympics and the
entities likewise; `suite.py report` on both result files has them all).

## Reasoning workloads (OWL 2 RL closure at load)

| Workload | System | Load + closure | Peak | Statements | Queries |
|---|---|---:|---:|---:|---:|
| LUBM(1) | **NRESE (now)** | **528** | – | 168,089 | **22** |
| | Nemo | 3,191 | 36 | 168,057 | – |
| | owlrl (reference) | 41,078 | 244 | 185,447 | – |
| LUBM(10) | **NRESE (now)** | **1,014** | 323 | 2,102,871 | **83** |
| | Nemo | 35,111 | 342 | 2,102,839 | – |
| LUBM(100) | **NRESE (now)** | **6,337** | 2,684 | 22,132,616 | **669** |
| | NRESE (batch C) | 7,158 | 3,461 | 22,132,616 | 2,496 |
| | Nemo | 393,475 | 3,384 | 22,132,584 | – |
| OWL2Bench RL(1) | **NRESE (now)** | **991** | 105 | 1,454,463 | **686** |
| | Nemo | 1,794,271 | 25,620 | 1,454,431 | – |

### LUBM with the closure precomputed (batch E)

The SPARQL engines without a reasoner on their own turf: LUBM(N) closed under OWL 2 RL
beforehand (by NRESE, checked against the published LUBM(1) answers and across systems:
every system answers all 14 queries correctly), loaded without inference.

| Tier | System | Load | Load peak | Store | Queries | Queries, cache on |
|---|---|---:|---:|---:|---:|---:|
| LUBM(1), 168 k | **NRESE** | **646** | – | 4.1 | **22** | **17** |
| | QLever | 727 | – | **2.0** | 354 | 332 |
| | Virtuoso | 1,169 | 5,908 | 114 | 242 | – |
| | Oxigraph | 795 | – | 12.6 | 295 | – |
| LUBM(10), 2.1 M | **NRESE** | **750** | – | 50.5 | **84** | **66** |
| | QLever | 2,001 | 265 | **24.1** | 643 | 576 |
| | Virtuoso | 6,522 | 6,220 | 180 | 1,486 | – |
| | Oxigraph | 1,867 | 1,088 | 148 | 3,517 | – |
| LUBM(100), 22 M | **NRESE** | **3,908** | **1,347** | 549 | **655** | **582** |
| | QLever | 13,613 | 2,288 | **251** | 4,288 | 3,992 |
| | Virtuoso | 62,615 | 6,804 | 611 | 15,563 | – |
| | Oxigraph | 13,775 | 7,925 | 1,558 | 40,828 | – |

Jena (TDB2) runs after these; GraphDB's results stay local.

Nemo and owlrl compute closures without a query interface. NRESE's closures have 32
statements more than Nemo's on every tier (not yet broken down; owlrl's reference closure
of LUBM(1) is larger still, it adds OWL's axiomatic triples). Jena's OWL Horst
rule reasoner answered LUBM(1) (25.7 s for the queries) and ran out of time on LUBM(10).
The SPARQL engines without a reasoner (QLever, Oxigraph, Virtuoso) are compared on LUBM by
the `lubm-materialised` workload, below.

## Where NRESE is behind

- **Store size.** QLever's stores are 1.4 times (DBpedia) to 2 times (Wikidata) smaller.
  QLever builds all six permutations, yet they take about 2.25 bytes per key against our
  4.5 (frame-of-reference bit packing per block of 128 keys, which doesn't use that the
  columns are sorted), and its vocabulary is compressed to 28 % (176 MB for Wikidata's
  618 MB), where our dictionary is mapped uncompressed (862 MB). Delta-encoding the sorted
  columns within blocks and a compressed vocabulary would close it; both change the
  checkpoint format and the point-lookup path, so they are planned, not started.

  Measured on the office PC's checkpoints (`index/compression_study.rs`, an ignored test):
  choosing per block and key position the cheapest of the current frame of reference, a
  palette of the block's distinct values, and deltas for the first position that varies,
  the four permutations of a default-graph store would take, in bytes per quad:

  | Store | SPOG | POSG | OSPG | PSOG | Together | With the choice |
  |---|---:|---:|---:|---:|---:|---:|
  | Wikidata lexemes | 9.16 → 6.37 | 2.92 → 2.49 | 3.39 → 2.69 | 3.52 → 2.89 | 19.0 | 14.4 (−24 %) |
  | DBpedia core | 10.07 → 8.62 | 4.36 → 3.89 | 5.56 → 4.36 | 4.73 → 4.17 | 24.7 | 21.0 (−15 %) |

  Block headers and first keys add 0.81 bytes per key in every permutation; the first keys
  can be derived from the blocks (−0.25), and blocks of 256 keys would halve the rest. The
  objects in SPOG (identifiers in insertion order, random within a subject) are what
  stays expensive: QLever's identifiers follow its sorted vocabulary.
- **First count after a restart.** On LUBM(100), YAGO tiny and Wikidata the first count
  took 14 to 22 ms in Docker (one sample each), against 1.5 ms elsewhere; on the office
  PC the same store answers it in 0.9 ms over HTTP. To be rechecked with more runs.

## Next runs

- Jena on `lubm-materialised` (the rest of batch E).
- The competitors on YAGO tiny, and an RDF4J adapter.
