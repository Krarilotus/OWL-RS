# ADR-0002: Dictionary encoding + LSM runs over six quad permutations + WAL

Status: accepted (2026-09-25)

## Context

Requirements:
- sub-linear reads for every triple pattern
- write cost proportional to the change, not the dataset
- readers never blocked by writers
- crash safety
- a path to QLever-scale compact indexes

State of the art considered:
- **Sorted permutation indexes over dictionary IDs** (RDF-3X, Neumann & Weikum 2008/2010; Hexastore; QLever, Bast & Buchhold 2017). Every pattern is a contiguous range, and merge joins come for free on shared sort orders.
- **Log-structured merge** (O'Neil et al. 1996; the tiering/levelling trade-offs in Dostoevsky, Dayan & Idreos 2018). It gives O(log N) amortised write cost with immutable runs, and those runs also give MVCC snapshots for free.
- **QLever's update model** (delta triples over a static index). It degrades until a full re-index: QLever issue #2449 reports a count query 13× slower after 245 k updates on 9 M triples. Continuous compaction avoids that failure mode.

## Decision

- **Term IDs.** `TermId(u64)` has a 4-bit kind tag and a 60-bit payload.
  - Dictionary terms (IRI, blank node, literal) index an append-only arena with a SwissTable lookup.
  - Canonical `xsd:integer` (fits in 60 bits signed), `xsd:boolean`, `xsd:decimal` (56-bit mantissa, ≤ 15 fraction digits), `xsd:date` and `xsd:dateTime` (years 0000–9999, millisecond precision, timezones in 15-minute steps) values are inlined. Non-canonical lexical forms stay dictionary terms, so RDF term identity is preserved.
- **Permutations.** Quads are stored in six permutations: `SPOG POSG OSPG` for an unbound graph, and `GSPO GPOS GOSP` for a bound graph. The default graph has the reserved id `0`. (Amended 2026-09-27, XC1: a seventh, `GPSO`, gives subject-sorted scans per predicate for merge joins; see `docs/design/execution-core.md`.)
- **Runs.** The index is a list of immutable sorted runs.
  - A commit appends one run of inserts plus tombstones. The run is computed exactly against the snapshot, so counts stay exact.
  - Reads merge the runs, and the newest wins.
  - Size-tiered compaction merges the newest runs while `older.len <= FANOUT * newer.len`. Merging into the oldest run drops tombstones.
- **Durability.**
  - A redo-only WAL records committed deltas plus newly interned terms.
  - Checkpoints write the dictionary and the base run atomically.
  - Recovery = checkpoint + replay.
  - The revision is part of every WAL record, so it survives restarts.
- **Named graphs** exist iff they contain at least one quad, the model QLever and GraphDB/RDF4J use. `CREATE GRAPH` is a no-op; this is documented as an intentional deviation from Oxigraph.

## Consequences

- Write cost is O(d log d) for a delta of d quads, plus amortised compaction.
- Memory is 6 × 32 bytes per quad while uncompressed. Block compression (delta + bit-packing per block, QLever/RDF-3X style) is the next planned storage step. It's invisible to upper layers, because scans go through the run API.
- The index is memory-resident in this phase. On-disk memory-mapped runs come after compression.

## Implementation notes (2026-09-25, E2–E4)

- **One merge.** Scans and compaction share a single k-way sign-summing merge (`index/merge.rs`), so the visibility rule exists once. Because partial sign sums over any contiguous window of runs are −1, 0 or +1, compaction needs no special case for the oldest run: cancelled entries vanish, and unmatched tombstones survive only while an older run could still hold their insert.
- **Compaction placement.** Windows of up to `inline_entry_limit` entries (default 32 Ki) are merged inside the commit. Larger ones, and checkpoints, run on one background maintenance thread. Measured result: commit latency is independent of dataset size (see `benches/baselines/README.md`).
- **Commit protocol.** The WAL mutex is held across append and publish. A checkpoint snapshots under that mutex and rotates the segment, so every older segment is provably covered and can be deleted.
- **Failure model.**
  - A failed WAL append poisons the log until reopen, so a torn frame can never be followed by valid ones.
  - On recovery, a torn tail in the last segment is truncated; an invalid frame anywhere else is a `Corruption` error, never skipped.
  - A single commit is capped at 4 GiB of WAL payload (about 130 M quads); larger loads go through the bulk loader (E5).
- **Dictionary continuity.** Terms interned by aborted transactions stay in the dictionary and are logged with the next commit. This keeps logged ids one contiguous range, so replay is deterministic. Unused terms are a later GC concern; they're never visible as data.
- **Platform.** On Unix, renames and segment creation are followed by a directory `fsync`. Windows can't open directories as files, so that step is skipped there and NTFS metadata journaling is relied on instead.
- **Second stack (E6).** A version holds an asserted stack and an inferred stack (decision D2). Both use the same run, merge and compaction code. A stack's `Layout` decides which permutations its runs hold:
  - The inferred stack holds default-graph quads only, so it keeps SPOG/POSG/OSPG.
  - Graph-first plans over the inferred stack are rotated to the matching graph-last permutation, or answered as empty for named graphs.
  - Disjointness of the two stacks keeps the materialised count an O(1) sum, and lets scans concatenate the stacks without deduplication.

## Observable differences from v1 (Oxigraph), confirmed by differential tests

Both differences are intentional; they are pinned by tests in `crates/nrese-sparql/tests/differential_tests.rs` and must be listed in the release notes when engine v2 replaces v1 (P2).

1. **Literal lexical forms are preserved.** Oxigraph canonicalises typed literals on storage (`"030"^^xsd:integer` is stored as `"30"`, `"1"^^xsd:boolean` as `"true"`, `"25.0"^^xsd:decimal` as `"25"`). RDF 1.1 treats these as distinct terms, and NRESE keeps them. Consequences:
   - Graph patterns match by term: `?s ex:age 30` doesn't match `"030"`.
   - `FILTER(?a = 30)` compares values and matches all of them.
   - Stored data comes back byte-for-byte as it was written, which ResearchSpace forms and provenance workflows rely on.
2. **A named graph exists iff it holds a quad** (QLever and RDF4J/GraphDB semantics). Oxigraph keeps an explicit registry, so `CREATE GRAPH` persists an empty graph and a graph stays listed after its last quad is deleted. In NRESE:
   - `CREATE GRAPH` stores nothing.
   - `CLEAR`/`DROP` of an empty named graph fails without `SILENT`.
