# NRESE against QLever as a read engine: the gaps by layer (13 October 2026)

The owner, 13 Oct: *"it still has memory performance lead for also parallel requests that
reuse cache from last one … it's still the read king, we need to learn a bit here."* This
plan names each gap, the layer that owns it ([ARCHITECTURE.md](../ARCHITECTURE.md)), what
it costs us measured, and a proposed fix, in order of cost.

**Status: draft.** The ranking below is provisional until the concurrent-read measurements
of §6 report; numbers marked *to measure* are not evidence yet.

**Sources.**
- QLever's code at `d91db7284e96` (13 Oct 2026; a shallow clone, read only), file:line,
  and the checked research notes [R5](../../literature/wiki/research/R5%20QLever%20memory%20on%20large%20joins.md)
  and [R6](../../literature/wiki/research/R6%20cache%20invalidation%20under%20updates.md).
- QLever as measured: `adfreiburg/qlever@sha256:d39c3520e715…` (revision `3d7f1e96`, 29 Sep
  2026), the same image on Office-PC and Phuoc-Yu. It predates the external sort's parallel
  merge (#3463, 30 Sep), which the code at `d91db72` has.
- Our runs: Sparqloscope on DBLP (605.9 M statements; `sparql-sparqloscope-memory-phuoc-2`,
  QLever's reproduction `sparql-sparqloscope-reproduce-phuoc-4`), the fast cases against
  QLever (`2026-10-10-competitors-dc2114a`, Office), the lab log
  ([performance.md](../design/performance.md)).

## 1. Where we stand

| | NRESE | QLever | Evidence |
|---|---|---|---|
| Sparqloscope DBLP, first executions, geometric mean | 0.0595 s (`1d6dca4e`) | 0.208 s | ahead on 82 of 105; paper protocol, Phuoc |
| Point lookups and small joins, 1–64 clients | 7,300–75,000 queries/s | 32–2,000 queries/s | Office, 10 Oct: QLever's numbers are its 42 ms per request on a reused connection (a response in two writes without `TCP_NODELAY` waits for the client's delayed ACK, fixed in the harness 12 Oct with `--connections fresh`: 0.7 ms). **Not a comparison until rerun** |
| Store on disk, DBLP | 22.3 GB, 36.7 B per statement, 4 permutations and the dictionary | 13.0 GB, 21.5 B per statement, 6 permutations, vocabulary 1.5 GB internal (3.7 GB uncompressed) and 79.4 M words external | `size` rows of the two runs above; QLever's build log |
| Store on disk, DBpedia core | 3.8 GB | 2.8 GB | [capability matrix](../spec/06-target-capability-matrix.md) |
| Serving memory, fast cases (social, 300 K people) | 172–232 MiB | 24–48 MiB | `docker stats`, 10 Oct: counts our mapped index pages but not QLever's page cache. *To measure* as `anon-kernel` |
| Load, DBLP | 190 s, peak 17.5–23 GB | 358 s, peak 12.6 GB | the same runs |

## 2. The cache (owner: `nrese-sparql` cache, `native::cached`)

**What QLever does** (R5, R6): every operator's result is a cache candidate, keyed by the
operator subtree's string and the delta-triples snapshot index (`QueryExecutionContext.h:79–90`,
`Operation.cpp:739–753`); concurrent requests for one key wait for one producer
(`computeOnce`, `QueryExecutionContext.h:92–96`); pinnable; 30 GB, 5 GB per entry, 1,000
entries; a lazy (streamed) subresult is kept only while it stays under 5 MB, a lazy root
up to 5 GB (`RuntimeParameters.h:53–58`, `:120`; `Operation.cpp:266–307`). One `-m` budget
(4 GB default) covers queries and cache; an allocation that fails evicts unpinned entries
first (`Qlever.cpp:44–50`). **Every update clears the whole cache, pinned entries included**
(`Qlever.cpp:247–273`), and so does an index swap, whose snapshot counter restarts
(`Qlever.cpp:698–706`).

**What we have** (more than the brief assumed): plan parts are cached, not only whole
answers. Every operator's result is an entry keyed by its algebra with variables numbered
(so renamed parts share), the snapshot's identity, read model, dataset, active graph and
options; concurrent requests wait for one producer; pins at startup; GreedyDual-Size-
Frequency ranking with admission by benefit ([cache.rs](../../crates/nrese-sparql/src/cache.rs)
module docs). Whole serialised answers are kept beside them. Default size: 2 % of memory
(64 MiB to 8 GiB, `budgets.result_cache`).

**Gaps.**
1. *Surviving writes.* Both engines lose every entry on a write: QLever clears, we make
   them unreachable (the revision is in the key) and they age out. Proposed: R6's option C,
   per-dependency epochs. Each entry records what it read (the predicates and graphs of its
   scans, or "everything" for a part that can't say); a commit bumps the epochs of what its
   evaluated write set touched; a lookup is valid while its entry's epochs are current. Under
   reasoning a base write moves derived predicates too: the rule dependency graph supplies
   them (the robustness thread designs the entailment side). The oracle: cache on against
   off over random update histories.
2. *Size.* 2 % of memory against QLever's 30 GB: on a 32 GB host, 640 MiB. *To measure:*
   hit rates of the Sparqloscope multi-client run with the cache on.
3. *Results not kept.* Sparqloscope 084 is 053 written with `WHERE` and a final dot (the
   same algebra, so the same key); QLever answered it from its cache (1.0 ms), we computed it
   again (21 ms). It is not the spelling: 084's own second run took 2.58 ms, and 102's
   3.08 ms, against 0.25–0.41 ms for the hits around them (053, 085, 101, 103), so these
   results were not kept at all. Next: the admission and eviction decisions per part, logged
   in that run; the test `aggregates_written_differently_share_the_result_cache` covers the
   key.
4. *Streamed parts.* QLever keeps a lazy subresult while it stays under 5 MB. We
   materialise every part, so every part is a candidate; when parts stream (§4), the same
   rule applies: keep a streamed part only up to a bound, abandon the copy past it.

## 3. Index and vocabulary (owner: `nrese-engine` index and dictionary; with the v2.0 core thread)

**QLever:** six permutations of blocks of 31,250 rows (`ConstantsIndexBuilding.h:106`),
each column of a block Zstd-compressed (`CompressedRelationWriter.cpp:60`), read with
`pread` (`util/File.h:188`), not mapped; a large relation's blocks are written per relation
(`CompressedRelationWriter.cpp:191–207`). Vocabulary: compressed internal words (DBLP: 1.5 GB
of 3.7 GB) and an external part; an FSST wrapper exists (`CompressionWrappers.h:89`), which
compression the default uses is not checked here.

**We:** four permutations for default-graph data, blocks of 128 keys, each position
frame-of-reference bit-packed (palettes optional), every key readable in O(1) without
decompressing its block ([keys.rs](../../crates/nrese-engine/src/index/keys.rs)); the
checkpoint mapped and used in place.

**Gaps.**
1. *Bytes per statement:* 36.7 against 21.5 on DBLP (1.71×) though QLever keeps six
   permutations to our four. Roughly 7 B per key per permutation against 3: our columns are
   packed by their range within 128 keys, where Zstd over 31,250 sorted values finds the
   repeats and small steps. *To measure:* the split of our 22.3 GB into permutations and
   dictionary (the suite now lists each store's files, `store-files.tsv`).
   Proposed, for the immutable base only (§5): per block, the column after the leading one
   delta-coded within a run of equal leading values, and larger blocks for the base with a
   two-level directory, keeping O(1) access inside a decoded 128-key frame. The cost to
   watch: random access (probes, galloping lookups) gets a decode per block.
2. *First executions on a mapped store pay page faults.* Sparqloscope 102 took 137.5 ms on
   its first run and 3.1 ms on its second; a walk cut into 9,200 parallel parts read a page
   or two per part. Fixed for group walks (parts cut where values change, this branch);
   the general rule: on mapped data, count pages touched, not keys.
3. *Metadata QLever keeps:* counts of distinct subjects, predicates, objects (101–103:
   120–390 ms against 0.5–45 ms). Proposed: per permutation, the number of distinct leading
   values recorded when a run is built and kept in the checkpoint.
4. *Text order:* QLever's vocabulary is sorted, so a prefix is one id range (071–073:
   1.4–9.4 ms against our 63–271). Ours finds a prefix's terms by binary search in a text
   order, then checks, sorts and looks up each. Proposed: per checkpoint, the text order's
   ranks as an id-sorted side index, so a prefix becomes a rank range and a merge with the
   pattern's groups.

## 4. Execution (owner: `nrese-sparql::native`, `nrese-exec`; the current work)

**QLever:** index scans lazy since 2023 (20 blocks in flight), joins and GROUP BY since 2024,
OPTIONAL/MINUS/EXISTS since 2025; join output in 100,000-row blocks (`JoinHelpers.h:34`),
children cached or estimated under 1 M rows materialised (`RuntimeParameters.h:107`);
external sort since Jan 2026, Zstd spill past 5 GB (`Sort.cpp:83–245`). A `COUNT(*)` over a
join forces the full join without materialising it.

**We:** operators materialise their tables; merge and galloping joins on sorted keys, hash
joins otherwise, worst-case optimal joins for cycles; counts of join trees by message
passing without rows (beyond QLever: counts in O(values), not O(rows)); the request
envelope refuses what doesn't fit with a memory error instead of losing the process.

**Gaps.**
1. *Large results to a consumer that streams:* a SELECT over a large join, GROUP BY with
   other aggregates over joins, ORDER BY without LIMIT. QLever answers them in flat memory;
   we refuse them past the envelope. Proposed: block-wise evaluation of a BGP's join into
   its consumer (QLever's rule: materialise what is small by estimate or cached, stream the
   rest), block size and the small cut-off as settings with an "unlimited" value.
2. *External sort:* none (`execution-core.md` §4). Proposed: QLever's tested template (R5):
   compressed blocks per column, merge parameters from the budget, a clear error when a
   minimal block doesn't fit; the spill location and new terms inside the budget.
3. *Within counts:* both sides of a star listed before their product (012/013: up to 1.3 GB
   of weights). Proposed: the product as a merge of the group walks, block by block.

## 5. Read-mostly design (owner: `nrese-engine` index, runs and checkpoint)

**QLever:** one immutable index, updates as located delta triples merged into scans
(`DeltaTriples.h`), a rebuild to fold them in, the whole cache cleared on each update.

**We:** already an immutable base and deltas: sorted runs, never mutated, tombstones for
deletes, compaction merging runs, the base mapped from the checkpoint and the WAL for what
came after (`index/run.rs`). What is missing is what an immutable base allows: a denser
format for the base only (§3.1), and the fast paths that hold for one run only
(`count_groups`, `scan_columns_in` decline where several runs or tombstones meet a range).
*To measure:* reads after N small commits against a fresh store (`writes-under-readers`).

## 6. Measurements (queue jobs)

The SPARQL group runs the concurrent-read comparison for research round 6 (`suite/sparql`,
`e2261316`, `428d62bd`), with QLever pinned to the image of the code read here
(`adfreiburg/qlever:commit-d91db7284e@sha256:b5c135c9…`). This analysis takes its numbers from
those jobs; the duplicates this branch had queued were cancelled.

| Job | What | Host |
|---|---|---|
| `sparql-sparqloscope-clients-phuoc` | Sparqloscope on DBLP from 1, 2, 4, 8, 16, 32, 64 clients, 60 s per level, NRESE (`8c13a74f`) and QLever (`-j 16`), cache on, fresh connections | Phuoc, exclusive |
| `sparql-clients-sweep-office` | the fast case `clients-sweep` (lookups and small joins), 1–64 clients | Office, night window |
| `sparql-qlever-memory-yago-tiny` | QLever's serving memory taken apart: read-ahead, mapped index, vocabulary, cache at idle, cold, warm, cleared | Office |
| `sparql-serving-memory-yago-tiny` | serving memory again, QLever pinned | Office |
| `sparql-sparqloscope-memory-phuoc-3` | this branch at `69b21851`: round 3 of the paper protocol, and each query's `memory_peak_bytes` from diagnose's plans | Phuoc, exclusive |

One difference to settle before the cache-on numbers count: the Sparqloscope job gives QLever
a 5 GB cache (the reproduction's setting) and NRESE its default, 2 % of memory. Equal budgets:
`NRESE_CACHE_MAX_SIZE` (`bench/read-gaps`, `dc9112b5`).

## 7. Gaps in order of cost (provisional)

| # | Gap | Cost to us | Owner | Proposed fix |
|---|---|---|---|---|
| 1 | Serving memory under concurrency | 4–8× QLever on the fast cases (*kind to remeasure*) | engine (mapped index, dictionary), exec | measure first; then §3.1, §4.1 |
| 2 | Bytes per statement | 1.71× on DBLP, 1.36× on DBpedia core | engine index and dictionary | §3.1, with the v2.0 core thread |
| 3 | Large streamed results | refused past the envelope where QLever answers | `native`, `nrese-exec` | §4.1, §4.2 |
| 4 | Cache across writes | every write empties it (both) | `nrese-sparql` cache | §2.1 (beyond QLever) |
| 5 | Metadata counts, text-ordered ids | 101–103, 071–073: up to 280× per query | engine index, dictionary | §3.3, §3.4 |
