//! Bulk loads (E5): initial loads and restores that bypass the per-commit path.
//!
//! A [`BulkLoad`] holds the writer slot for its whole lifetime. Callers add batches of
//! quads, from any number of threads, each at its place in the input
//! ([`BulkLoad::add_at`]). Terms the dictionary holds are found under its shared lock;
//! new ones go to tables of their own, a lock per shard, and the quads take provisional
//! ids ([`crate::term::pending`]). [`BulkLoad::finish`] numbers the new terms in the order
//! they first occur in the input (dense ids, the same at any thread count, their text left
//! where it was interned), renumbers the quads, sorts once in parallel, builds the base
//! run directly (no transaction hash sets, no per-quad existence checks against an empty
//! base) and publishes one revision. A load dropped unfinished leaves the dictionary as
//! it was.
//!
//! Memory: in a durable store with `map_checkpoints`, a load into an empty store or one
//! replacing its data is bounded by `bulk_load_memory`: past it, quads are sorted in chunks
//! spilled to disk and merged into each permutation as it goes into the checkpoint
//! ([`super::spill`]).
//!
//! Durability: a bulk load is not written to the WAL, whose records are capped at 4 GiB.
//! Instead, a checkpoint of the new revision is written *before* the revision is published,
//! so nothing is ever visible that a crash could lose. Its cost is O(dataset), which is why
//! small changes should use transactions.

use std::sync::Arc;
use std::time::Instant;

use nrese_rdf::Quad;
use parking_lot::{Mutex, MutexGuard};
use rayon::prelude::*;

use super::spill::{Spill, Spiller};
use super::{CommitSummary, Inner, ReadModel, Snapshot, Stack, Version};
use crate::durability::checkpoint;
use crate::error::{EngineError, EngineResult};
use crate::index::keys::PackedKeys;
use crate::index::run::{PermutationBuilder, Run};
use crate::index::{IndexVersion, Layout};
use crate::quad::{EncodedQuad, EncodedTriple, Permutation, QuadPattern};
use crate::term::TermId;
use crate::term::pending::{Adopted, Pending};

/// Batches copied between releases of their memory ([`crate::memory`]): about 256 MiB at
/// the stores' batch size.
const RELEASE_EVERY: usize = 256;

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
    batches: Mutex<Batches>,
    /// Quads per spilled chunk, if the load spills past its memory budget.
    chunk: Option<usize>,
    /// Started with the first spilled chunk; held while one is handed over.
    spiller: Mutex<Option<Spiller>>,
    /// Spilling failed (setting it up, or the thread): the load fails at `finish`.
    spill_failed: Mutex<Option<std::io::Error>>,
    /// The new terms, numbered at `finish`.
    pending: Pending,
    /// Batches [`add`](Self::add) has numbered.
    added: std::sync::atomic::AtomicU32,
}

/// Interned quads not yet spilled.
#[derive(Default)]
struct Batches {
    batches: Vec<Vec<EncodedQuad>>,
    quads: usize,
}

impl<'e> BulkLoad<'e> {
    pub(super) fn new(engine: &'e Inner, slot: MutexGuard<'e, ()>, mode: BulkMode) -> Self {
        // The writer slot is held from here on: whether the load can be streamed into the
        // checkpoint (and so spilled) stays as decided now.
        let chunk = engine
            .shared
            .durable
            .as_ref()
            .filter(|_| streamable(engine, mode))
            .and_then(|durable| durable.config.bulk_load_memory)
            // In flight at once: a chunk filling, one being spilled and its sorted copy.
            .map(|budget| (budget as usize / 3 / size_of::<EncodedQuad>()).max(1));
        Self {
            engine,
            _slot: slot,
            mode,
            batches: Mutex::default(),
            chunk,
            spiller: Mutex::new(None),
            spill_failed: Mutex::new(None),
            pending: Pending::default(),
            added: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Adds a batch of quads as the next of this load's own numbering (chunk 0): in the
    /// order the calls come, which is the input's where one thread adds.
    pub fn add(&self, quads: &[Quad]) {
        let batch = self
            .added
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.add_at(0, batch, quads);
    }

    /// Adds batch `batch` of chunk `chunk` of the input, chunks and their batches numbered
    /// in input order (chunk 0 is [`add`](Self::add)'s): new terms take their ids at
    /// `finish` in the order they first occur so, whatever the threads' order. Callable
    /// from several threads at once. Past the memory budget, it waits while the last
    /// chunk is spilled.
    pub fn add_at(&self, chunk: u32, batch: u32, quads: &[Quad]) {
        let encoded =
            self.engine
                .shared
                .dictionary
                .intern_quads_pending(quads, &self.pending, chunk, batch);
        let full = {
            let mut batches = self.batches.lock();
            batches.quads += encoded.len();
            batches.batches.push(encoded);
            self.chunk.is_some_and(|chunk| batches.quads >= chunk)
        };
        if full {
            self.spill(false);
        }
    }

    /// Hands the batches over to the spilling thread, starting it if needed: all of them
    /// with `rest`, else only once a chunk is full (another thread may have taken it).
    fn spill(&self, rest: bool) {
        let (Some(chunk), Some(durable)) = (self.chunk, &self.engine.shared.durable) else {
            return;
        };
        let mut spiller = self.spiller.lock();
        let batches = {
            let mut batches = self.batches.lock();
            if batches.quads == 0 || (!rest && batches.quads < chunk) {
                return;
            }
            std::mem::take(&mut *batches).batches
        };
        if self.spill_failed.lock().is_some() {
            return;
        }
        if spiller.is_none() {
            match Spiller::start(durable.root()) {
                Ok(started) => *spiller = Some(started),
                Err(error) => {
                    *self.spill_failed.lock() = Some(error);
                    return;
                }
            }
        }
        let sent = spiller
            .as_ref()
            .is_some_and(|spiller| spiller.send(batches));
        if !sent {
            // The thread stopped on an error, which `finish` reports.
            let failed = self.spill_failed.lock().is_some();
            if !failed && let Some(stopped) = spiller.take() {
                let error = match stopped.finish() {
                    Err(error) => error,
                    Ok(_) => std::io::Error::other("the spilling thread stopped"),
                };
                *self.spill_failed.lock() = Some(error);
            }
        }
    }

    /// Publishes the loaded quads as one new revision (see the module docs for durability).
    /// Returns the revision and the asserted and inferred quads added and removed.
    pub fn finish(self) -> EngineResult<CommitSummary> {
        // A load that spilled sends what is left as its last chunk.
        if self.spiller.lock().is_some() {
            self.spill(true);
        }
        let Self {
            engine,
            _slot,
            mode,
            batches,
            spiller,
            spill_failed,
            pending,
            ..
        } = self;
        let shared = &engine.shared;
        // No commit can run (writer slot) and no compaction can replace runs while the new
        // version is built from the current one and installed.
        let _compaction = shared.versions.compaction_slot.lock();
        let base = shared.snapshot();
        let started = Instant::now();
        let spilled = match (spill_failed.into_inner(), spiller.into_inner()) {
            (Some(error), _) => return Err(EngineError::Io(error)),
            (None, Some(spiller)) => Some(spiller.finish()?),
            (None, None) => None,
        };
        let current = base.version();
        let mut spilled = spilled;
        // The batches into one array, each freed once copied (not all held twice).
        let batches = batches.into_inner().batches;
        let mut quads: Vec<EncodedQuad> = Vec::with_capacity(batches.iter().map(Vec::len).sum());
        // What the copied batches held stays with the threads that interned them until
        // released: without, the batches would count twice at the peak.
        for (i, batch) in batches.into_iter().enumerate() {
            quads.extend_from_slice(&batch);
            if i % RELEASE_EVERY == RELEASE_EVERY - 1 {
                drop(batch);
                crate::memory::release_all();
            }
        }
        // The new terms numbered, the quads (and spilled chunks) renumbered to them.
        let mut renumbered = Ok(());
        shared.dictionary.adopt_pending(pending, |adopted| {
            remap(adopted, &mut quads);
            if let Some(spill) = spilled.as_mut() {
                renumbered = spill.sort_raw(&|quads: &mut [EncodedQuad]| remap(adopted, quads));
            }
        });
        renumbered?;
        if let Some(spill) = spilled {
            return publish_spilled(engine, spill, current, started, _compaction);
        }
        // By radix over the bits the ids use ([`nrese_exec::sort`]).
        let kept = nrese_exec::sort::sort_dedup_keys(crate::quad::as_keys_mut(&mut quads));
        quads.truncate(kept);

        // Nothing else in the new version: its permutations go straight into the checkpoint.
        let streamed = streamable(engine, mode);
        let (next, summary) = match mode {
            _ if streamed => {
                let summary = CommitSummary {
                    revision: current.revision + 1,
                    inserted: quads.len() as u64,
                    deleted: current.asserted.len(),
                    inferred_inserted: 0,
                    inferred_deleted: current.inferred.len(),
                };
                let layout = Layout::holding(&quads);
                let next = Next::Streamed {
                    layout,
                    builder: PermutationBuilder::planned(quads, layout.permutations()),
                    revision: summary.revision,
                    dictionary_len: shared.dictionary.len(),
                };
                (next, summary)
            }
            BulkMode::Replace => {
                let summary = CommitSummary {
                    revision: current.revision + 1,
                    inserted: quads.len() as u64,
                    deleted: current.asserted.len(),
                    inferred_inserted: 0,
                    inferred_deleted: current.inferred.len(),
                };
                let next = Version {
                    content: super::Content::fresh(),
                    asserted: IndexVersion::from_quads(Layout::holding(&quads), quads),
                    inferred: IndexVersion::empty(Layout::DefaultGraph),
                    revision: summary.revision,
                    dictionary_len: shared.dictionary.len(),
                    equality: Default::default(),
                };
                (Next::Built(next), summary)
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
                // The first quad in a named graph turns a default-graph stack into a quad
                // stack.
                let asserted = match Layout::holding(&inserts) {
                    Layout::Quads => current.asserted.with_quads_layout(),
                    Layout::DefaultGraph => current.asserted.clone(),
                };
                let run = Run::from_quads(asserted.layout(), inserts);
                let next = Version {
                    content: super::Content::fresh(),
                    asserted: asserted.with_run(run),
                    inferred: current.inferred.with_delta(&[], &explicit),
                    revision: summary.revision,
                    dictionary_len: shared.dictionary.len(),
                    equality: Default::default(),
                };
                (Next::Built(next), summary)
            }
        };
        if summary.inserted + summary.deleted + summary.inferred_deleted == 0 {
            return Ok(CommitSummary {
                revision: current.revision,
                ..summary
            });
        }

        let built = started.elapsed();
        publish(engine, next)?;
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

/// `quads` with their provisional ids replaced by the ids `adopted` numbered them to.
fn remap(adopted: &Adopted, quads: &mut [EncodedQuad]) {
    let id = |id: TermId| match id.kind().is_dictionary() {
        true => TermId::new(id.kind(), adopted.remap(id.payload())),
        false => id,
    };
    quads.par_iter_mut().for_each(|quad| {
        *quad = EncodedQuad::new(
            id(quad.subject),
            id(quad.predicate),
            id(quad.object),
            id(quad.graph),
        );
    });
}

/// Whether a load in `mode` publishes a version holding nothing but the loaded quads,
/// written into the checkpoint permutation by permutation ([`Next::Streamed`]).
fn streamable(engine: &Inner, mode: BulkMode) -> bool {
    let current = engine.shared.snapshot();
    let current = current.version();
    engine
        .shared
        .durable
        .as_ref()
        .is_some_and(|durable| durable.config.map_checkpoints)
        && (mode == BulkMode::Replace || current.asserted.len() + current.inferred.len() == 0)
}

/// [`BulkLoad::finish`] of a load that spilled: its chunks merged into the checkpoint.
fn publish_spilled(
    engine: &Inner,
    spill: Spill,
    current: &Version,
    started: Instant,
    compaction: MutexGuard<'_, ()>,
) -> EngineResult<CommitSummary> {
    let revision = current.revision + 1;
    let dictionary_len = engine.shared.dictionary.len();
    let layout = spill.layout();
    let inserted = publish_streamed(
        engine,
        Source::Spilled(spill),
        layout,
        revision,
        dictionary_len,
    )?;
    drop(compaction);
    let summary = CommitSummary {
        revision,
        inserted,
        deleted: current.asserted.len(),
        inferred_inserted: 0,
        inferred_deleted: current.inferred.len(),
    };
    tracing::info!(
        revision,
        inserted,
        ms = started.elapsed().as_millis() as u64,
        "bulk load published from spilled chunks"
    );
    engine.after_commit(0);
    Ok(summary)
}

/// The packed permutations of a streamed load.
enum Source {
    /// Sorted in memory.
    Memory(PermutationBuilder),
    /// Merged from chunks on disk.
    Spilled(Spill),
}

impl Source {
    fn packed(&mut self, permutation: Permutation) -> EngineResult<PackedKeys> {
        match self {
            Self::Memory(builder) => Ok(builder.packed(permutation)),
            Self::Spilled(spill) => Ok(spill.merged(permutation)?),
        }
    }
}

/// The version a bulk load publishes.
enum Next {
    /// Built in memory.
    Built(Version),
    /// Asserted quads and nothing else: their permutations are built one at a time, each
    /// written to the checkpoint and dropped before the next (durable, `map_checkpoints`).
    /// The load's peak then holds one packed permutation instead of all of them.
    Streamed {
        /// The asserted stack's: the default-graph one unless a quad is in a named graph.
        layout: Layout,
        builder: PermutationBuilder,
        revision: u64,
        dictionary_len: u64,
    },
}

/// Installs `next` as the latest version, the bulk way: in durable mode a checkpoint of it
/// is written first, so no WAL record is needed, and with `map_checkpoints` the version
/// installed is the checkpoint's, mapped (same content: memory then holds what queries
/// touch, as after a restart). The caller holds the writer and compaction slots.
fn publish(engine: &Inner, next: Next) -> EngineResult<()> {
    let shared = &engine.shared;
    let next = match next {
        Next::Built(version) => version,
        Next::Streamed {
            layout,
            builder,
            revision,
            dictionary_len,
        } => {
            let source = Source::Memory(builder);
            return publish_streamed(engine, source, layout, revision, dictionary_len).map(|_| ());
        }
    };
    let next = Arc::new(next);
    match &shared.durable {
        Some(durable) => {
            let revision = next.revision;
            let _checkpoint = durable.checkpoint_slot.lock();
            let image = Snapshot::new(
                Arc::clone(&next),
                Arc::clone(&shared.dictionary),
                Arc::clone(&shared.statistics),
                None,
            );
            let path = checkpoint::write(durable.root(), &image)?;
            shared
                .checkpoints
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            drop(image);
            let mut base = None;
            let next = match durable
                .config
                .map_checkpoints
                .then(|| checkpoint::map_written(&path))
            {
                Some(Ok((dictionary, [asserted, inferred]))) => {
                    base = Some(dictionary);
                    Arc::new(Version {
                        // The same statements, mapped from the checkpoint.
                        content: next.content,
                        asserted,
                        inferred,
                        equality: Arc::clone(&next.equality),
                        revision,
                        dictionary_len: next.dictionary_len,
                    })
                }
                Some(Err(error)) => {
                    tracing::warn!(%error, "checkpoint not mapped; data stays in memory");
                    next
                }
                None => next,
            };
            // The checkpoint covers every logged revision: later commits start a new
            // segment, and all older segments can go.
            let mut wal = durable.wal.lock();
            wal.rotate(revision + 1)?;
            shared.versions.install(next);
            wal.release_through(revision)?;
            drop(wal);
            if let Some(base) = base
                && let Err(error) = shared.dictionary.rebase(base)
            {
                tracing::warn!(%error, "checkpoint dictionary not mapped; it stays in memory");
            }
            checkpoint::remove_older_than(durable.root(), revision)?;
        }
        None => shared.versions.install(next),
    }
    Ok(())
}

/// [`publish`] of [`Next::Streamed`]: the checkpoint written from `source`, the asserted
/// stack in `layout`, then installed mapped. In memory (no checkpoint), the permutations
/// are built into a version. Returns the number of quads.
fn publish_streamed(
    engine: &Inner,
    mut source: Source,
    layout: Layout,
    revision: u64,
    dictionary_len: u64,
) -> EngineResult<u64> {
    let shared = &engine.shared;
    let Some(durable) = &shared.durable else {
        let packed = layout
            .permutations()
            .iter()
            .map(|&permutation| Ok((permutation, source.packed(permutation)?)))
            .collect::<EngineResult<Vec<_>>>()?;
        let asserted =
            IndexVersion::from_packed(packed).map_err(crate::error::EngineError::Corruption)?;
        let next = Version {
            content: super::Content::fresh(),
            asserted,
            inferred: IndexVersion::empty(Layout::DefaultGraph),
            revision,
            dictionary_len,
            equality: Default::default(),
        };
        let len = next.asserted.len();
        publish(engine, Next::Built(next))?;
        return Ok(len);
    };
    let _checkpoint = durable.checkpoint_slot.lock();
    let empty = PackedKeys::from_sorted(&[]);
    // Every layout has SPOG: its length is the number of quads.
    let mut len = 0;
    let path = checkpoint::write_parts(
        durable.root(),
        revision,
        &shared.dictionary,
        dictionary_len,
        [layout, Layout::DefaultGraph],
        &mut |stack, permutation| {
            Ok(match stack {
                Stack::Asserted => {
                    let packed = source.packed(permutation)?;
                    if permutation == Permutation::Spog {
                        len = packed.len() as u64;
                    }
                    std::borrow::Cow::Owned(packed)
                }
                Stack::Inferred => std::borrow::Cow::Borrowed(&empty),
            })
        },
    );
    drop(source);
    let path = path?;
    shared
        .checkpoints
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (base, [asserted, inferred]) = match checkpoint::map_written(&path) {
        Ok(mapped) => mapped,
        Err(error) => {
            // Nothing else holds the data: the load fails, and the checkpoint goes so that
            // a restart doesn't recover what was never published.
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
    };
    let next = Arc::new(Version {
        content: super::Content::fresh(),
        asserted,
        inferred,
        revision,
        dictionary_len,
        equality: Default::default(),
    });
    let mut wal = durable.wal.lock();
    wal.rotate(revision + 1)?;
    shared.versions.install(next);
    wal.release_through(revision)?;
    drop(wal);
    if let Err(error) = shared.dictionary.rebase(base) {
        tracing::warn!(%error, "checkpoint dictionary not mapped; it stays in memory");
    }
    checkpoint::remove_older_than(durable.root(), revision)?;
    Ok(len)
}

/// A rematerialisation in progress: the reasoner reads [`base`](Self::base), computes the
/// complete inferred set, and [`finish`](Self::finish) replaces the inferred stack with it
/// as one revision. The batch counterpart of [`Transaction::insert_inferred`]
/// (crate::Transaction::insert_inferred), for initial reasoning after a bulk load and for
/// ruleset changes; durable the way bulk loads are.
///
/// It holds the writer slot for its lifetime, so the base can't change underneath. Dropping
/// it without `finish` aborts it.
pub struct Rematerialisation<'e> {
    engine: &'e Inner,
    _slot: MutexGuard<'e, ()>,
    base: Snapshot,
}

impl<'e> Rematerialisation<'e> {
    pub(super) fn new(engine: &'e Inner, slot: MutexGuard<'e, ()>) -> Self {
        let base = engine.shared.snapshot();
        Self {
            engine,
            _slot: slot,
            base,
        }
    }

    /// The state the inferences are derived from.
    pub fn base(&self) -> &Snapshot {
        &self.base
    }

    /// The id of `term`, interning it: rules mention constants the data may not contain.
    pub fn intern(&self, term: nrese_rdf::TermRef<'_>) -> crate::TermId {
        self.engine.shared.dictionary.intern(term)
    }

    /// Replaces the inferred stack with `inferred` (in the default graph). Statements that
    /// are asserted in the default graph are dropped: the stacks stay disjoint. Returns the
    /// revision and the inferred statements added and removed.
    pub fn finish(self, inferred: Vec<EncodedTriple>) -> EngineResult<CommitSummary> {
        let Self {
            engine,
            _slot,
            base,
        } = self;
        let shared = &engine.shared;
        let _compaction = shared.versions.compaction_slot.lock();
        let started = Instant::now();
        let mut quads: Vec<EncodedQuad> = inferred
            .into_par_iter()
            .map(EncodedTriple::in_default_graph)
            .filter(|quad| !base.stack_contains(Stack::Asserted, quad))
            .collect();
        quads.par_sort_unstable();
        quads.dedup();
        let mut old: Vec<EncodedQuad> = base
            .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
            .collect();
        old.par_sort_unstable();
        let inserted = quads
            .par_iter()
            .filter(|q| old.binary_search(q).is_err())
            .count();
        let deleted = old
            .par_iter()
            .filter(|q| quads.binary_search(q).is_err())
            .count();
        // Compaction may have replaced runs since `base` was taken, with the same content.
        let current = shared.snapshot();
        let current = current.version();
        let summary = CommitSummary {
            revision: current.revision + 1,
            inserted: 0,
            deleted: 0,
            inferred_inserted: inserted as u64,
            inferred_deleted: deleted as u64,
        };
        if inserted + deleted == 0 {
            return Ok(CommitSummary {
                revision: current.revision,
                ..summary
            });
        }
        let next = Version {
            content: super::Content::fresh(),
            asserted: current.asserted.clone(),
            inferred: IndexVersion::from_quads(Layout::DefaultGraph, quads),
            revision: summary.revision,
            dictionary_len: shared.dictionary.len(),
            equality: Default::default(),
        };
        publish(engine, Next::Built(next))?;
        drop(_compaction);
        tracing::info!(
            revision = summary.revision,
            inferred_inserted = summary.inferred_inserted,
            inferred_deleted = summary.inferred_deleted,
            ms = started.elapsed().as_millis() as u64,
            "inferred stack replaced"
        );
        engine.after_commit(0);
        Ok(summary)
    }
}
