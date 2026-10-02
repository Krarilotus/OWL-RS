# Where NRESE is behind, and why: top-down analysis (1 October 2026)

The owner's direction:
1. Where NRESE is behind, find out where the performance actually goes.
2. Fix the approach first: how a query is evaluated and how data is laid out.
3. Then data structures, memory and code generation, down to machine code.

Every finding below is checked with a profile (`perf` in the benchmark containers, see
§4) before it is built, and measured after.

## 1. The gaps

The query times are sums of medians from the 26 September scorecard, QLever beside our
in-process perf lab of 27 September. Those two are not strictly comparable, and the HTTP
rerun is part of this plan.

| Dataset | Query | NRESE | QLever | Ratio |
|---|---|---:|---:|---:|
| DBpedia core | q08 `CONTAINS` on labels | 1,882 ms | no answer | – |
| | q12 `SUM` per team over a 3-way star | 206 ms | 1.6 ms | 130× |
| | q05 `FILTER(?pop > 1e6)`, top 50 | 37 ms | 1.5 ms | 24× |
| YAGO tiny | q08 `COUNT(*)` of `rdfs:subClassOf*` | 11,432 ms | 44 ms | 260× |
| | q02 `YEAR(?birth) = 1879` with `LANG` = en | 1,522 ms | 1.8 ms | 850× |
| | q06 `STRSTARTS` on alternate names | 95 ms | 5.1 ms | 19× |
| Wikidata lexemes (26 Sep, HTTP) | q07 count per feature | 2,101 ms | 1.3 ms | – |
| | q04 3-way join, `LIMIT 100000` | 3,380 ms | 1,024 ms | 3.3× |
| | q02 count per language | 68 ms | 1.3 ms | – |
| | q06 `STRSTARTS` on lemmas | 148 ms | 1.3 ms | – |

| Resource (DBpedia core, 67 M) | NRESE | QLever |
|---|---:|---:|
| Memory while serving | 15.9 GB | 0.2 GB |
| Restart | 22 s | 1 s |
| Store per triple | 57 B | 42 B |

**A measurement fault, found while preparing this.** QLever ran with a result cache
capped at 1 MB. Every small result fitted, counts and top-20 lists among them, so every
measured run after the warm-up was a cache hit: the many 1.3 ms and 44 ms answers. NRESE
ran with its result cache off. So these figures compare a cache with an evaluation.

The answer is not to switch QLever's cache off. The owner's direction is that every
system's strengths count, and NRESE should win on the others' turf too. So the suite
(`benches/suite`, README):
- runs every system that has a result cache both ways: cache off, then cache on with the
  system's defaults (NRESE's 64 MiB cache, QLever's own);
- shuffles the query order, the same seeded orders for every system;
- reports each query's first execution apart.

The gaps above are remeasured that way. The approach-level causes below stand regardless:
they are about evaluation, which is what a first execution and a cache miss cost.

## 2. Causes at the level of the approach

| # | Queries | What happens now (to confirm by profile) | Approach |
|---|---|---|---|
| A1 | q08, YAGO q06, Wikidata q06 | Each label triple's string is fetched from the dictionary at a random place and tested | **Filter the dictionary first.** A string test on a variable bound only by one predicate's objects runs once per distinct term, sequentially over the dictionary arena (`memchr`'s SIMD substring search). The matching ids then semi-join the index scan. Cost: the size of the strings, read once in order, instead of one cache miss per triple |
| A2 | YAGO q02 | `YEAR(?birth) = 1879` is evaluated per row after the join | **Rewrite filters to index ranges.** Inline date ids sort by value, so `YEAR(?d) = c` becomes `?d` in [c-01-01, c+1-01-01), a range scan. `LANG(?l) = "en"` is read from the key's bytes, without decoding |
| A3 | Wikidata q07, q02 | The pattern's rows are built, then grouped | **Count on the index.** `GROUP BY` one variable of one triple pattern with `COUNT` reads the run lengths of the permutation sorted on that variable; nothing is materialised |
| A4 | YAGO q08 | The closure is materialised pair by pair, then counted | **Closures by components.** Strongly connected components, a topological order and bitset reachability; `COUNT` of a closure is computed from the component sizes without emitting pairs. Zero-length pairs count every node once |
| A5 | Wikidata q04, every `LIMIT` | Joins build their whole output before `LIMIT` cuts it | **Pipelined execution (completion plan 2.1).** Morsels flow from scans through joins to the sink, which stops at the limit |
| A6 | Serving memory, restart, store size | All permutations and the dictionary on the heap; a restart reads and rebuilds them | **On-disk base, in-memory delta.** Immutable, compressed, block-indexed runs read on demand (memory-mapped), a small block cache, and recent writes in memory until merged. The dictionary likewise: a front-coded string arena on disk with an in-memory index. Restart maps files instead of rebuilding them (completion plan 2.4, Pf2/Pf5) |

## 3. Below the approach

After each approach change, the hot loops of the profile:
- data layout: columns, ids, cache lines;
- branch-free and SIMD kernels where the profile shows a loop;
- allocation per operator;
- code generation:
  - a build for `x86-64-v3` (AVX2) beside the portable one, chosen at start-up or at
    packaging, hardware-aware as the project wants;
  - profile-guided optimisation (PGO) measured on the perf-lab workloads.

## 4. How it is measured

- **Profiles:** `perf record -g` in a container built from `rust:<toolchain>` with
  `linux-perf` and `inferno`. The perf lab runs in-process, with frame pointers, under
  the `profiling` profile, on the benchmark datasets in `nrese-bench-data`. Profiles and
  flame graphs stay under `tmp/` and aren't committed.
- **End to end:** the suite, all systems, fresh stores. Licensed systems' results stay
  local (GraphDB Free counts as licensed).
- **Every change:** perf lab before and after on the dataset of its gap, and the
  differential tests.

## 5. Progress (1–2 October 2026)

Perf lab, DBpedia core (67 M quads), office PC (Ryzen 9 5950X, 32 GB), on-disk store,
native build; medians of 3 runs. "Before" is the 27 September perf lab on the main PC.

| | Before | Now | What changed |
|---|---:|---:|---|
| q08 `CONTAINS` on labels | 1,882 ms | 36 ms | A1: dictionary-first string test, one parallel `memmem` pass over the arena |
| q03 top birthplaces (group count) | 26 ms | 14 ms | A3: group walk on the index (galloping over constant blocks, column decode) |
| q12 `SUM` per team, 3-way star | 206 ms | 41 ms | columnar scans: a block and a column at a time, not a quad at a time |
| q09 OPTIONAL, `LIMIT 100000` | 96 ms | 58 ms | A5 (part): LIMIT pushed into the OPTIONAL's left side and BGP morsels |
| q13 `FILTER NOT EXISTS` count | 25 ms | 9 ms | columnar scans |
| Sum of query medians | 2,279 ms | 187 ms | |
| Restart (open) | 4.5 s | 0.000 s | A6: checkpoint format 6 used in place (memory map) |
| Resident after open | 5.8 GB (peak 10 GB) | 13 MiB | A6: index runs, dictionary text and hash table stay in the file |
| Resident after all queries | 5.5 GB | 2.8 GB | the pages the queries touched (file-backed, reclaimable) |
| Bulk load: peak | 10 GB | 6.0 GB | exact batch arrays, lean text order, permutations streamed into the checkpoint |
| Bulk load: resident after it | 5.8 GB | 57 MiB | the checkpoint served mapped; freed memory given back on every pool thread |

YAGO tiny (16.5 M quads), same machine:

| | 27 September | Now | What changed |
|---|---:|---:|---|
| q08 `COUNT(*)` of `rdfs:subClassOf*` | 11,432 ms | 24 ms | A4, and the graph's node count (zero-length pairs) as a per-version statistic |
| q02 `YEAR(?birth) = 1879`, `LANG` = en | 1,522 ms | 0.6 ms | A2 |
| q06 `STRSTARTS` on alternate names | 95 ms | 0.5 ms | the text order |
| q10 `LANG(?l) = "en"` on 6.8 M labels | 602 ms | 267 ms | A1 for languages: the passing terms from the dictionary, matched by a bitmap in one columnar scan |
| Sum of query medians | 13,678 ms | 310 ms | |
| Bulk load: peak / resident after it | 4.1 GB / 1.9 GB | 2.4 GB / 41 MiB | as for DBpedia |

Wikidata lexemes (60 M quads), same machine; "before" is the 26 September scorecard over
HTTP on the main PC, so the comparison is indicative:

| | 26 September | Now | What changed |
|---|---:|---:|---|
| q04 3-way join, `LIMIT 100000` | 3,380 ms (QLever 1,024) | 21 ms | A5 (part): adaptive morsels by observed fan-out; small morsels probe the index |
| q07 count per feature | 2,101 ms | 0.18 ms | A3 |
| q05 senses, OPTIONAL, `LIMIT 100000` | – | 38 ms | the OPTIONAL's left side cut at the limit |
| q06 `STRSTARTS` on 452 k lemmas | 148 ms | 0.06 ms | the dictionary's text order (checkpoint format 7): a prefix is two binary searches |
| Sum of query medians | – | 80 ms | |

2 October, in the same perf lab (DBpedia and YAGO sums above include them):

| | Before | Now | What changed |
|---|---:|---:|---|
| YAGO q10, 407 k rows (TSV) | 269 ms | 90 ms | planning without a seek per passing term (estimated counts); results serialised on every core |
| DBpedia q09, 100 k rows | 58 ms | 31 ms | results serialised on every core |
| DBpedia q05 populations (`xsd:nonNegativeInteger`) | 19 ms | 1.3 ms | integer-derived literals inline (kind 12), so the range hint narrows them |
| Olympics (1.8 M quads), sum | 92 ms | 69 ms | the same (q05 6.0 → 0.23 ms, q13 `AVG` of `xsd:int` ages 22 → 13 ms); ORDER BY + LIMIT computes tie-break keys for the candidates only (q04 17.5 → 9 ms) |
| LUBM-100 after OWL 2 RL, 14 counting queries | 73 ms | 60 ms | WCOJ chunks sized to the threads (q2 18 → 4.8 ms) |

Query sets at the end of 2 October (perf lab, office PC, TSV results; on-disk stores
loaded fresh, so integer-derived literals are inline):

| Workload | 27 September | Now | Peak during the load / resident after it |
|---|---:|---:|---|
| DBpedia core, 67 M quads, 13 queries | 2,279 ms | 128 ms | 5.6 GB / 57 MiB |
| YAGO tiny, 16.5 M quads, 10 queries | 13,678 ms | 131 ms | 2.4-2.9 GB / 38 MiB |
| Wikidata lexemes, 60 M quads, 10 queries | (HTTP) | 53 ms | 3.3 GB / 50 MiB |
| Olympics, 1.8 M quads, 13 queries | – | 56 ms | 0.8 GB |
| Entities, 10 M quads, 9 queries | – | 10 ms | 1.3 GB |
| LUBM-100 after OWL 2 RL (8.7 M inferred in 3.2 s), 14 queries with results | – | 120 ms | – |
| OWL2Bench RL-1 after OWL 2 RL (1.4 M inferred in 0.5 s), q20 (1.3 M rows) | 86 ms | 55 ms | – |

What moved them on 2 October, besides the table above: integer-derived literals inline,
columnar range scans and date comparisons on ids (entities q04 20 -> 2.4 ms), one-pass
grouped integer aggregates, DISTINCT over a BGP as a set (Olympics q07 9.7 -> 0.03 ms),
ORDER BY with LIMIT computing tie-break keys lazily, columnar scans merged across the
asserted and inferred stacks, EXISTS patterns as sets with sorted merges for semi- and
anti-joins, results serialised on every core.

Done, with differential tests against the reference evaluator:
- **A1** CONTAINS / STRSTARTS / STRENDS on a large pattern's object or STR of it, and
  `LANG(?v) = "tag"`, alone or with one of them. Only for patterns that will be scanned:
  one many times larger than the smallest pattern is probed instead, and the pass would
  be wasted. Many passing terms are matched by a bitmap in one scan, not a seek each.
- **A2** `YEAR(?d) op c` as id ranges of inline dates and dateTimes (YAGO q02's filter).
- **A3** group counts and distinct values by a group walk instead of a search per group.
- **A4** `COUNT` of open `p*`/`p+` closures from the closure size per strongly connected
  component plus the node count, no pairs built; open closures per component.
- **A6, first part**: checkpoint format 6 is mapped: packed index runs (headers, first keys,
  bits) and the dictionary (arena, end offsets, an open-addressing table with the fixed
  `key_hash`). New terms and runs live on the heap on top. Opening checks the structure
  only; `store.verify_on_open` checks everything (CRC, every block, every key).

Still open:
- A5, the rest: a pipelined executor. LIMIT without ORDER BY already stops early in basic
  graph patterns (morsels of the first pattern, each twice the last, through the joins)
  and below OPTIONAL and projections; other operators still evaluate whole.
- Columnar scans answer one tombstone-free run; matches in both stacks (asserted and
  inferred) or several runs still go quad by quad: merge them column-wise next.
- The text order could also answer string ranges (`?s >= "M"`) and ORDER BY on strings
  (a rank per entry instead of decoding and comparing terms).
- A6, second part: done for checkpoints and bulk loads (`store.map_checkpoints`, on by
  default: what a checkpoint holds is served from the file once written). Compaction
  into the base still writes a heap run until the next checkpoint; merged base runs
  written to files (LSM) would avoid that.
- Bulk loads with bounded memory, first part done (2 October): the loaded quads.
  `budgets.bulk_load_memory` (default 25 % of the machine) bounds them: past it they are
  sorted in chunks of a third of it on a spilling thread with its own pool, each
  permutation packed into its own file, and merged into each permutation while the
  checkpoint is written (`engine/spill.rs`). DBpedia core (67 M) on the office PC:

  | Budget | Load | Peak RSS |
  |---|---:|---:|
  | none | 38.8 s | 5.46 GB |
  | 2 GiB | 43.9 s | 4.43 GB |
  | 1 GiB | 43.5 s | 4.11 GB |
  | 512 MiB | 44.6 s | 4.21 GB |

  The floor left is the heap dictionary (2.3 GB, needed for interning) plus one merged
  packed permutation. Bounding the dictionary takes ids assigned after an external sort
  of the terms, as QLever's vocabulary merge does (per-batch vocabularies, merged, ids
  rewritten), or an arena in a file the OS can page.
- The remeasurement of every gap in the suite, with cache on and off, after batch A.
