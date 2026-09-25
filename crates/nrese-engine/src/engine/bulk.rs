//! Bulk loads (E5): initial loads and restores that bypass the per-commit path.
//!
//! A [`BulkLoad`] holds the writer slot for its whole lifetime. Callers add batches of
//! quads, from any number of threads; each batch is interned with one lock acquisition
//! ([`Dictionary::intern_quads`](crate::term::Dictionary)). [`BulkLoad::finish`] then
//! sorts once in parallel, builds the base run directly (no transaction hash sets, no
//! per-quad existence checks against an empty base) and publishes one revision.
//!
//! Durability: a bulk load is not written to the WAL, whose records are capped at 4 GiB.
//! Instead, a checkpoint of the new revision is written *before* the revision is published,
//! so nothing is ever visible that a crash could lose. Its cost is O(dataset), which is why
//! small changes should use transactions.

use std::sync::Arc;
use std::time::Instant;

use oxrdf::Quad;
use parking_lot::{Mutex, MutexGuard};
use rayon::prelude::*;

use super::{CommitSummary, Inner, Snapshot, Stack, Version};
use crate::durability::checkpoint;
use crate::error::EngineResult;
use crate::index::IndexVersion;
use crate::index::run::Run;
use crate::quad::EncodedQuad;

/// What a bulk load does with the existing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkMode {
    /// Adds the loaded quads to the asserted data. Quads already asserted are skipped; loaded
    /// quads that were inferred become explicit.
    Append,
    /// Replaces the asserted data with the loaded quads and clears the inferred stack, whose
    /// contents were derived from the replaced data.
    Replace,
}

/// A bulk load in progress. Dropping it without [`finish`](Self::finish) aborts it; terms
/// it interned stay unreferenced in the dictionary, as with an aborted transaction.
pub struct BulkLoad<'e> {
    engine: &'e Inner,
    _slot: MutexGuard<'e, ()>,
    mode: BulkMode,
    batches: Mutex<Vec<Vec<EncodedQuad>>>,
}

impl<'e> BulkLoad<'e> {
    pub(super) fn new(engine: &'e Inner, slot: MutexGuard<'e, ()>, mode: BulkMode) -> Self {
        Self {
            engine,
            _slot: slot,
            mode,
            batches: Mutex::default(),
        }
    }

    /// Adds a batch of quads. Callable from several threads at once; batches of 10⁴–10⁵
    /// quads amortise the dictionary lock well.
    pub fn add(&self, quads: &[Quad]) {
        let encoded = self.engine.shared.dictionary.intern_quads(quads);
        self.batches.lock().push(encoded);
    }

    /// Publishes the loaded quads as one new revision (see the module docs for durability).
    /// Returns the revision and the asserted and inferred quads added and removed.
    pub fn finish(self) -> EngineResult<CommitSummary> {
        let Self {
            engine,
            _slot,
            mode,
            batches,
        } = self;
        let shared = &engine.shared;
        // No commit can run (writer slot) and no compaction can replace runs while the new
        // version is built from the current one and installed.
        let _compaction = shared.versions.compaction_slot.lock();
        let base = shared.snapshot();
        let started = Instant::now();
        let mut quads: Vec<EncodedQuad> = batches.into_inner().concat();
        quads.par_sort_unstable();
        quads.dedup();

        let current = base.version();
        let (next, summary) = match mode {
            BulkMode::Replace => {
                let summary = CommitSummary {
                    revision: current.revision + 1,
                    inserted: quads.len() as u64,
                    deleted: current.asserted.len(),
                    inferred_inserted: 0,
                    inferred_deleted: current.inferred.len(),
                };
                let next = Version {
                    asserted: IndexVersion::from_quads(Stack::Asserted.layout(), quads),
                    inferred: IndexVersion::empty(Stack::Inferred.layout()),
                    revision: summary.revision,
                    dictionary_len: shared.dictionary.len(),
                };
                (next, summary)
            }
            BulkMode::Append => {
                let inserts: Vec<EncodedQuad> = quads
                    .into_par_iter()
                    .filter(|quad| !base.stack_contains(Stack::Asserted, quad))
                    .collect();
                // Disjointness: newly asserted statements leave the inferred stack.
                let explicit: Vec<EncodedQuad> = inserts
                    .par_iter()
                    .filter(|quad| {
                        quad.graph.is_default_graph() && base.stack_contains(Stack::Inferred, quad)
                    })
                    .copied()
                    .collect();
                let summary = CommitSummary {
                    revision: current.revision + 1,
                    inserted: inserts.len() as u64,
                    deleted: 0,
                    inferred_inserted: 0,
                    inferred_deleted: explicit.len() as u64,
                };
                let asserted = Run::from_quads(Stack::Asserted.layout(), inserts);
                let inferred = Run::from_delta(Stack::Inferred.layout(), &[], &explicit);
                let next = Version {
                    asserted: current.asserted.with_run(asserted),
                    inferred: current.inferred.with_run(inferred),
                    revision: summary.revision,
                    dictionary_len: shared.dictionary.len(),
                };
                (next, summary)
            }
        };
        if summary.inserted + summary.deleted + summary.inferred_deleted == 0 {
            return Ok(CommitSummary {
                revision: current.revision,
                ..summary
            });
        }

        let built = started.elapsed();
        let next = Arc::new(next);
        match &shared.durable {
            Some(durable) => {
                let revision = next.revision;
                let _checkpoint = durable.checkpoint_slot.lock();
                let image = Snapshot::new(Arc::clone(&next), Arc::clone(&shared.dictionary));
                checkpoint::write(durable.root(), &image)?;
                // The checkpoint covers every logged revision: later commits start a new
                // segment, and all older segments can go.
                let mut wal = durable.wal.lock();
                wal.rotate(revision + 1)?;
                shared.versions.install(next);
                wal.release_through(revision)?;
                drop(wal);
                checkpoint::remove_older_than(durable.root(), revision)?;
            }
            None => shared.versions.install(next),
        }
        drop(_compaction);
        tracing::info!(
            revision = summary.revision,
            inserted = summary.inserted,
            index_build_ms = built.as_millis() as u64,
            checkpoint_ms = (started.elapsed() - built).as_millis() as u64,
            "bulk load published"
        );
        engine.after_commit(0);
        Ok(summary)
    }
}
