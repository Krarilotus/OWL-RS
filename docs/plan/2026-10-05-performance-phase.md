# Performance phase (from 5 October 2026)

The owner's order: before more DL features, analyse and optimise performance with
discipline, and write the ideas down for the paper ([performance.md](../design/performance.md),
whose §6 is this phase's lab log). Then the DL path continues, comparing as many
algorithmic alternatives as possible.

## Where the time and memory go now

From the office batches (run records office-a and office-b) and their logs:
- **Speed:** NRESE is ahead of every free system on every shared query, and it
  materialises OWL 2 RL 60× faster than Nemo on LUBM 100. The speed work left is the
  heaviest absolute costs, not gaps.
- **Memory and size** are where others lead (QLever's store and serving memory), and
  where scale breaks first. NRESE's LUBM 1000 load:

  | Phase | Time | |
  |---|---:|---|
  | parse and encode | 20.8 s | 138 M parsed, 3.3 M quads/s overall |
  | permutations from spilled chunks | 20.6 s | |
  | OWL 2 RL, 4 rounds | 30.4 s | 87.0 M inferred |
  | inferred stack written | 12.5 s | |
  | **peak** | **21.7 GiB** | of 31 GiB; store on disk 5.3 GB |

## Targets, in order

| # | Target | Now | Goal | First step |
|---|---|---|---|---|
| P1 | Materialisation memory at scale | LUBM 1000 peak 21.7 GiB (about 100 B per final statement) | half, at no more than 10 % more time | a memory profile over the phases (resident memory every 100 ms, heap by allocation site): which structures hold the peak |
| P2 | Serving memory and store size | QLever smaller (DBpedia 2.7 against 3.6 GB; LUBM-mat 100 251 against 536 MB); OWL2Bench RL-1 serves at 1.0 GiB on a 16 MiB store | the store within 1.2× of QLever's; serving memory explained and bounded | the RL-1 serving peak first (an anomaly); then the measured compression options (block FOR/palette/deltas 15-24 %, a front-coded or FSST vocabulary) |
| P3 | The DL pipeline outside the engine | ore_ont_1066: 982 ms whole, 258 ms engine; normaliser 0.5 s; the Horn context core 9× slower than the EL classifier | the pipeline below the engine's time; context core within 2× of EL | profile reader and normaliser on the ORE development set |
| P4 | The heaviest queries | LUBM 1000 q06 1.7 s, q14 1.4 s (millions of rows), q09 0.6 s (a triangle); OWL2Bench RL-1 q20 0.39 s (1.3 M rows) | each cost explained by profile; serialisation and transfer at memory bandwidth | profile q06 and q09 at LUBM 1000 |
| P5 | Load throughput | 3.3 M quads/s overall at LUBM 1000 | parse and index build each scaling with cores | per-phase CPU use (`cpu-use.py`): the serial stretches |
| P6 | First executions | DBpedia: first 250 ms, repeated 148 ms | the gap explained | profile cold runs |

## Discipline

- One target at a time on the measuring machine; work that needs no timing can run in
  parallel.
- Every change:
  1. a hypothesis from a profile;
  2. a differential test;
  3. interleaved A/B, medians of at least three runs, on its own target and on the
     standard sets (perf lab query sets, LUBM 1/100 reasoning, the DL development set),
     so that no other path gets slower;
  4. a lab log line in performance.md §6, kept or rejected, with the numbers.
- Large measurements (LUBM 1000) run on the main PC (64 GB) under a memory cap, or on the
  office PC as a queued batch; the suite's containers are capped by default (b5bf68e).
- The phase ends when P1-P3 have reached their goals or their limits are explained; the
  write-up then gets its summary for the paper.
