# Storage and Transaction Model

This is the contract the storage engine (`nrese-engine`, layer L1) and the mutation pipeline (`nrese-store`, L3) give to everything above them. The design rationale is in [ADR-0002](../adr/0002-engine-storage-lsm-permutations.md); implementation status is in the [capability matrix](06-target-capability-matrix.md).

## Data model

- **Dataset:** a set of quads `(subject, predicate, object, graph)`. The default graph is a graph like any other, with a reserved id.
- **Term identity is RDF term identity.**
  - Literals keep their lexical form exactly as written: `"030"^^xsd:integer` and `"30"^^xsd:integer` are different terms.
  - Graph patterns match terms. `FILTER` comparisons compare values.
- **Named graphs exist iff they contain at least one quad.**
  - `CREATE GRAPH` stores nothing.
  - `CLEAR`/`DROP` of an empty named graph fails unless `SILENT` is given.
- **Asserted and inferred statements are stored separately** (ADR-0003, roadmap E6).
  - The *asserted* stack holds what clients wrote, in any graph.
  - The *inferred* stack holds what the reasoner derived, always in the default graph.
  - The two are **disjoint**: a statement that is both asserted and derivable counts as asserted, as in GraphDB.
  - Backups contain asserted statements only.
- **Blank nodes are scoped to one payload.** Every write request (update, Graph Store write, `TELL`, restore) gets fresh blank nodes. Within one payload, labels stay consistent. The startup ontology preload keeps the file's labels, so re-preloading is idempotent.

## Reads

- **Snapshot reads.** Every query, Graph Store read and export runs on one snapshot, a consistent committed revision.
  - A snapshot never observes a partial commit.
  - Readers never wait for a commit, compaction or checkpoint to finish. Taking the latest snapshot reads the current version under a brief lock (`Versions::load`), held only while a finished version is published; reading a snapshot already taken takes no lock. The dictionary's lookups and the first build of a derived structure (a text order, a statistics set, a cache) can take locks of their own.
- **Read models.** Every read takes one:
  - `Materialised` (the default): asserted plus inferred statements
  - `Asserted`: explicit statements only
  - `Inferred`: implicit statements only

  A request-level switch and GraphDB's `onto:explicit`/`onto:implicit` pseudo-graphs come with R4.
- **Pattern cost.** Every triple pattern is a contiguous range in one of a stack's index permutations: O(r·log n + k) for k results over r runs, with r = O(log n). A stack has one of two layouts (`index::Layout`):
  - **Quads:** SPOG, POSG, OSPG, GSPO, GPOS, GOSP and GPSO, seven permutations for quads in any graph.
  - **Default graph:** SPOG, POSG, OSPG and PSOG, four permutations. A graph-first order over one constant graph sorts like the matching graph-last one, so the graph-first ones would be copies. The inferred stack always has it; the asserted stack has it until it holds a quad in a named graph (checkpoint format 9 and later).

## Writes

Every write goes through the mutation pipeline:

1. **Plan.** The command is applied to an engine transaction. Nothing is visible yet.
   - Later operations of one request see earlier ones.
   - The pending delta is exact: inserts are absent from the base, and deletes are present in it.
2. **Validate.** The gates run on the transaction's state: the reasoner maintains the inferred stack and rejects inconsistencies incrementally, from the delta (`mutation/pipeline.rs`; under `owl2-dl` also its consistency gate), and SHACL validates the changed focus nodes (`shacl.rs`).
3. **Claim.** The request's ticket decides the race against a timeout. A timed-out request is guaranteed not to be committed, and cancelling it also stops SPARQL evaluation.
4. **Commit.** The delta is appended to the WAL and synced, then published atomically as the next revision.

**Guarantees:**
- **Atomic inferences.** Asserted and inferred changes commit together, under one revision.
- **Disjointness is enforced by the engine.**
  - The reasoner can't infer an asserted statement.
  - Asserting an inferred statement makes it explicit.
  - Retracting an asserted statement leaves the inferred stack alone. Re-deriving a statement that is still supported is the reasoner's job.
  - These rules apply to the transaction's final state, so asserting and retracting a statement within one request changes nothing.
- **Request paths write asserted statements only.** SPARQL Update, the Graph Store Protocol and restore can't reach the inferred stack. `DELETE`, `CLEAR` and `DROP` retract asserted statements, never inferred ones.
- **Single writer.** Writes are serialised by the engine's writer slot. Commit cost is O(d log d) for a delta of d quads, independent of the dataset size.
- **Revisions** increase by one per commit with a net change. They are persistent in on-disk mode and don't change for no-op writes.
- **Aborts.** Any error before the commit discards the whole request. There are no partial writes.

## Bulk loads

Initial loads and full restores bypass the per-commit path (`Engine::bulk_load`; `nrese-server load`):

- **One revision.** A bulk load holds the writer slot and publishes one revision. `Append` adds to the asserted data; `Replace` swaps it and clears the inferred stack.
- **Durable before visible.** The load isn't written to the WAL. A checkpoint of the new revision is written before the revision is published, so a crash either loses the whole unacknowledged load or none of it. Its cost is O(dataset), which is why regular writes use transactions.
- **Parallel.** Batches are parsed and interned from many threads. The dictionary encodes, hashes and deduplicates a batch before taking its lock.
- **Blank nodes** are fresh per load, and consistent across the chunks of one load.
- **No gates.** Bulk loads are operator actions and don't run validation gates.

## Durability (on-disk mode)

- **Acknowledgement.** A commit is acknowledged only after its WAL record is synced (`SyncPolicy::EveryCommit`, the default).
- **Checkpoints** run in the background once the WAL grows past a threshold. Writers keep committing while a checkpoint is written, and covered WAL segments are deleted afterwards.
- **Recovery** loads the newest checkpoint and then replays later WAL records.
  - A torn record at the end of the log (a crash mid-write) is truncated; it was never acknowledged.
  - Invalid data anywhere else is a startup error, never silently skipped.
- **Directory lock.** The data directory is locked against a second process.
- **Format versions.** WAL segments and checkpoints carry a format version in their magic bytes (`durability/wal.rs`, `durability/checkpoint.rs`, which lists what each version added). Older formats this build still reads (WAL 4 to 6, checkpoints 4 to 11) are read; any other is rejected at startup with `UnsupportedFormat` instead of being reported as corrupt.

## Limits

- **Memory.** Index runs are packed (compressed keys) in memory and in checkpoints. In on-disk mode a restart, a bulk load and a background checkpoint map the checkpoint and use its dictionary and packed runs in place (`durability/checkpoint.rs`, `crate::mapped`): the OS pages in what queries touch. Runs written since the last checkpoint are on the heap.
- **Commit size.** One commit holds at most 4 GiB of WAL payload, about 130 M quads. Larger loads go through the bulk loader.
- **Reasoning cost.** A commit's reasoning is incremental (the delta executor) unless the program changes; the `rules-mvp` reasoner, whose every write cost O(dataset), was removed (the server refuses the name).

## Evidence

| Guarantee | Test |
|---|---|
| Visibility rule, all pattern shapes, compaction | `crates/nrese-engine/src/index/model_tests.rs` |
| Snapshot isolation, overlay reads, concurrent readers | `crates/nrese-engine/tests/it/engine_tests.rs` |
| Bulk loads equal transactional loads (parallel, append/replace, durable) | `crates/nrese-engine/tests/it/bulk_load_tests.rs`, `crates/nrese-store/tests/it/bulk_load_tests.rs` |
| Inferred stack: read models, disjointness, both stacks against a model across compaction, checkpoints and reopening | `crates/nrese-engine/tests/it/inferred_stack_tests.rs` |
| Crash recovery, torn tails, corruption, checkpoints | `crates/nrese-engine/tests/it/durability_tests.rs` |
| SPARQL semantics against an oracle | `crates/nrese-sparql/tests/it/differential_tests.rs`, `native_differential_tests.rs` |
| Pipeline: cancel vs. commit, gate rejection | `crates/nrese-store/tests/it/mutation_pipeline_tests.rs` |
| Write cost independent of size (HTTP, end to end) | `benches/baselines/v2-write-scaling-*.json` |
