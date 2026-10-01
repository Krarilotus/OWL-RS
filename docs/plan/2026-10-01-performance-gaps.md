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
