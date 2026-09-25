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
- **Blank nodes are scoped to one payload.** Every write request (update, Graph Store write, `TELL`, restore) gets fresh blank nodes. Within one payload, labels stay consistent. The startup ontology preload keeps the file's labels, so re-preloading is idempotent.

## Reads

- **Snapshot reads.** Every query, Graph Store read and export runs on one snapshot, a consistent committed revision.
  - A snapshot never observes a partial commit.
  - Readers never wait for writers, compaction or checkpoints.
- **Pattern cost.** Every triple pattern is a contiguous range in one of six index permutations: O(r·log n + k) for k results over r runs, with r = O(log n).

## Writes

Every write goes through the mutation pipeline:

1. **Plan.** The command is applied to an engine transaction. Nothing is visible yet.
   - Later operations of one request see earlier ones.
   - The pending delta is exact: inserts are absent from the base, and deletes are present in it.
2. **Validate.** The gates run on the transaction's state. Today this is the v1 reasoner gate, which reads the whole dataset when enabled; SHACL comes in M2.
3. **Claim.** The request's ticket decides the race against a timeout. A timed-out request is guaranteed not to be committed, and cancelling it also stops SPARQL evaluation.
4. **Commit.** The delta is appended to the WAL and synced, then published atomically as the next revision.

**Guarantees:**
- **Single writer.** Writes are serialised by the engine's writer slot. Commit cost is O(d log d) for a delta of d quads, independent of the dataset size.
- **Revisions** increase by one per commit with a net change. They are persistent in on-disk mode and don't change for no-op writes.
- **Aborts.** Any error before the commit discards the whole request. There are no partial writes.

## Durability (on-disk mode)

- **Acknowledgement.** A commit is acknowledged only after its WAL record is synced (`SyncPolicy::EveryCommit`, the default).
- **Checkpoints** run in the background once the WAL grows past a threshold. Writers keep committing while a checkpoint is written, and covered WAL segments are deleted afterwards.
- **Recovery** loads the newest checkpoint and then replays later WAL records.
  - A torn record at the end of the log (a crash mid-write) is truncated; it was never acknowledged.
  - Invalid data anywhere else is a startup error, never silently skipped.
- **Directory lock.** The data directory is locked against a second process.

## Limits (current milestone)

- **Memory.** Indexes are held in memory, at about 190 bytes per quad uncompressed. Compressed and larger-than-RAM indexes are Pf1/Pf2.
- **Commit size.** One commit holds at most 4 GiB of WAL payload, about 130 M quads. The bulk loader (E5) handles larger loads.
- **v1 reasoner cost.** With the v1 reasoner enabled (`rules-mvp`), every write costs O(dataset) until the M3 reasoner replaces it.

## Evidence

| Guarantee | Test |
|---|---|
| Visibility rule, all pattern shapes, compaction | `crates/nrese-engine/src/index/model_tests.rs` |
| Snapshot isolation, overlay reads, concurrent readers | `crates/nrese-engine/tests/engine_tests.rs` |
| Crash recovery, torn tails, corruption, checkpoints | `crates/nrese-engine/tests/durability_tests.rs` |
| SPARQL semantics against an oracle | `crates/nrese-sparql/tests/differential_tests.rs` |
| Pipeline: cancel vs. commit, gate rejection | `crates/nrese-store/tests/mutation_pipeline_tests.rs` |
| Write cost independent of size (HTTP, end to end) | `benches/baselines/v2-write-scaling-*.json` |
