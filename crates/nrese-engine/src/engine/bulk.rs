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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use nrese_rdf::Quad;
use parking_lot::{Mutex, MutexGuard};
use rayon::prelude::*;

use super::spill::{Spill, Spiller};
use super::{CommitSummary, Inner, ReadModel, Snapshot, Stack, Version};
use crate::durability::{Durable, checkpoint, later};
use crate::error::{EngineError, EngineResult};
use crate::index::keys::PackedKeys;
use crate::index::run::{PermutationBuilder, Run};
use crate::index::{IndexVersion, Layout};
use crate::quad::{EncodedQuad, EncodedTriple, Permutation, QuadPattern};
use crate::term::dictionary::BatchScratch;
use crate::term::pending::Pending;

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
    /// What interning a batch works in, kept for the next ones: one per thread adding.
    scratch: Mutex<Vec<BatchScratch>>,
    /// The writer slot. Last, because fields drop in order: a load dropped unfinished joins
    /// its spilling thread and removes the spill directory (`spiller`) before the next load
    /// can start and create its own there (B-303).
    _slot: MutexGuard<'e, ()>,
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
            mode,
            batches: Mutex::default(),
            chunk,
            spiller: Mutex::new(None),
            spill_failed: Mutex::new(None),
            pending: Pending::default(),
            added: std::sync::atomic::AtomicU32::new(0),
            scratch: Mutex::default(),
            _slot: slot,
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
        let mut scratch = self.scratch.lock().pop().unwrap_or_default();
        let encoded = self.engine.shared.dictionary.intern_quads_pending(
            quads,
            &self.pending,
            (chunk, batch),
            &mut scratch,
        );
        self.scratch.lock().push(scratch);
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
            scratch,
            ..
        } = self;
        drop(scratch);
        let shared = &engine.shared;
        // No commit can run (writer slot): `base` stays the latest content, though a merge or
        // a mapped checkpoint may replace its runs with the same statements. The new version
        // is built from it without the compaction slot, which `publish` takes to install it:
        // nothing that waits for that slot waits for the load's parallel work.
        let base = shared.snapshot();
        let started = Instant::now();
        let spilled = match (spill_failed.into_inner(), spiller.into_inner()) {
            (Some(error), spiller) => {
                // Joined, its directory gone, while the writer slot is held.
                drop(spiller);
                return Err(EngineError::Io(error));
            }
            (None, Some(spiller)) => Some(spiller.finish()?),
            (None, None) => None,
        };
        let current = base.version();
        let mut spilled = spilled;
        // The batches into one array, each freed once copied (not all held twice).
        let batches = batches.into_inner().batches;
        // What the threads freed while interning (the shards' arrays outgrown by doubling,
        // freed by whichever thread grew them) goes back before the largest allocation:
        // without, LUBM 1000's committed peak was 0.9 GB higher, at this copy.
        crate::memory::release_all();
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
        nrese_exec::heap::phase("load: numbering");
        let mut renumbered = Ok(());
        shared.dictionary.adopt_pending(pending, |adopted| {
            nrese_exec::heap::phase("load: renumbering");
            adopted.renumber(&mut quads);
            if let Some(spill) = spilled.as_mut() {
                renumbered = spill.sort_raw(&|quads: &mut [EncodedQuad]| adopted.renumber(quads));
            }
        });
        renumbered?;
        if let Some(spill) = spilled {
            return publish_spilled(engine, spill, current, started);
        }
        // By radix over the bits the ids use ([`nrese_exec::sort`]).
        nrese_exec::heap::phase("load: sorting");
        let kept = nrese_exec::sort::sort_dedup_keys(crate::quad::as_keys_mut(&mut quads));
        quads.truncate(kept);

        // Nothing else in the new version: its permutations go straight into the checkpoint.
        nrese_exec::heap::phase("load: permutations");
        let streamed = streamable(engine, mode);
        let (next, summary) = match mode {
            _ if streamed => {
                let summary = CommitSummary {
                    revision: current.revision + 1,
                    inserted: quads.len() as u64,
                    deleted: current.asserted.len(),
                    inferred_inserted: 0,
                    inferred_deleted: current.inferred.len(),
                    // Replaced by the published version's once it is out.
                    asserted: current.asserted_content,
                };
                let layout = Layout::holding(&quads);
                let next = Next::Streamed {
                    layout,
                    builder: PermutationBuilder::planned(quads, layout.permutations()),
                    revision: summary.revision,
                    stamp: Stamp {
                        previous: current.committed_micros,
                    },
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
                    // Replaced by the published version's once it is out.
                    asserted: current.asserted_content,
                };
                let next = Version {
                    content: super::Content::fresh(),
                    asserted_content: super::AssertedIdentity::fresh(),
                    asserted: IndexVersion::from_quads(Layout::holding(&quads), quads),
                    inferred: IndexVersion::empty(Layout::DefaultGraph),
                    revision: summary.revision,
                    // Stamped as it commits ([`publish`]).
                    committed_micros: None,
                    dictionary_len: shared.dictionary.len(),
                    equality: Default::default(),
                    canonical: Default::default(),
                };
                let stamp = Stamp {
                    previous: current.committed_micros,
                };
                (Next::Built(next, stamp), summary)
            }
            BulkMode::Append => {
                // Into a store asserting nothing every quad is new: moved, not filtered into
                // a copy, a second array of 32 bytes a quad beside the first while the
                // index is built (DBpedia core's load peaked 2 GB higher, as on main;
                // fleet pm-ib1801-1, 21 Oct 2026).
                let inserts: Vec<EncodedQuad> = match current.asserted.len() {
                    0 => quads,
                    _ => quads
                        .into_par_iter()
                        .filter(|quad| !base.stack_contains(Stack::Asserted, quad))
                        .collect(),
                };
                // Disjointness: newly asserted statements leave the inferred stack.
                let explicit: Vec<EncodedQuad> = match current.inferred.len() {
                    0 => Vec::new(),
                    _ => inserts
                        .par_iter()
                        .filter(|quad| {
                            quad.graph.is_default_graph()
                                && base.stack_contains(Stack::Inferred, quad)
                        })
                        .copied()
                        .collect(),
                };
                let summary = CommitSummary {
                    revision: current.revision + 1,
                    inserted: inserts.len() as u64,
                    deleted: 0,
                    inferred_inserted: 0,
                    inferred_deleted: explicit.len() as u64,
                    // Replaced by the published version's once it is out.
                    asserted: current.asserted_content,
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
                    asserted_content: super::AssertedIdentity::fresh(),
                    asserted: asserted.with_run(run),
                    inferred: current.inferred.with_delta(&[], &explicit),
                    revision: summary.revision,
                    // Stamped as it commits ([`publish`]).
                    committed_micros: None,
                    dictionary_len: shared.dictionary.len(),
                    equality: Default::default(),
                    canonical: Default::default(),
                };
                // It keeps what was there: of unknown time if that is.
                let stamp = Stamp {
                    previous: current.committed_micros,
                };
                (Next::Built(next, stamp), summary)
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
        // The writer slot is held: the latest version is the one just published.
        let summary = CommitSummary {
            asserted: engine.shared.snapshot().asserted_identity(),
            ..summary
        };
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
) -> EngineResult<CommitSummary> {
    let revision = current.revision + 1;
    let dictionary_len = engine.shared.dictionary.len();
    let layout = spill.layout();
    let inserted = publish_streamed(
        engine,
        Source::Spilled(spill),
        layout,
        revision,
        Stamp {
            previous: current.committed_micros,
        },
        dictionary_len,
    )?;
    let summary = CommitSummary {
        revision,
        inserted,
        deleted: current.asserted.len(),
        inferred_inserted: 0,
        inferred_deleted: current.inferred.len(),
        // The writer slot is held: the latest version is the one just published.
        asserted: engine.shared.snapshot().asserted_identity(),
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
    /// Built in memory, its time stamped as it commits.
    Built(Version, Stamp),
    /// Asserted quads and nothing else: their permutations are built one at a time, each
    /// written to the checkpoint and dropped before the next (durable, `map_checkpoints`).
    /// The load's peak then holds one packed permutation instead of all of them.
    Streamed {
        /// The asserted stack's: the default-graph one unless a quad is in a named graph.
        layout: Layout,
        builder: PermutationBuilder,
        revision: u64,
        stamp: Stamp,
        dictionary_len: u64,
    },
}

/// The time a bulk publication's version takes, stamped as it commits ([`Stamp::take`]): at
/// its checkpoint's end, just before the file is synced and renamed, not when the version was
/// built (a long checkpoint write could then commit after a time it was stamped before).
#[derive(Debug, Clone, Copy)]
struct Stamp {
    /// The time of the version it follows, which a replacement takes too, though it keeps
    /// nothing of it: while that time is unknown, a commit may have been made after any time
    /// the store's clock gives (a legacy store's version 4 log keeps no times, so a reopen
    /// loses them: review r2, R1-F2), and no later time is certified.
    previous: Option<u64>,
}

impl Stamp {
    /// The version's times, taken now: the commit is stamped by the store's clock
    /// ([`super::Shared::stamp`]: never before an earlier commit whose time it knows), and the
    /// version's time is the later of that and the previous version's, unknown if that is
    /// ([`later`]). Returns that and the clock ([`checkpoint::Times`]).
    fn take(self, shared: &super::Shared) -> checkpoint::Times {
        let commit = shared.stamp();
        (later(self.previous, Some(commit)), commit)
    }
}

/// Installs `next` as the latest version, the bulk way: in durable mode a checkpoint of it
/// is written first, so no WAL record is needed, and with `map_checkpoints` the version
/// installed is the checkpoint's, mapped (same content: memory then holds what queries
/// touch, as after a restart). The caller holds the writer slot. The checkpoint is written
/// under the checkpoint slot, on the caller's pool, and the version installed under the
/// compaction slot, held for nothing else: a merge or mapped checkpoint published since
/// `next` was built is replaced (the same statements: the writer slot kept them).
fn publish(engine: &Inner, next: Next) -> EngineResult<()> {
    let shared = &engine.shared;
    let (mut next, stamp) = match next {
        Next::Built(version, stamp) => (version, stamp),
        Next::Streamed {
            layout,
            builder,
            revision,
            stamp,
            dictionary_len,
        } => {
            let source = Source::Memory(builder);
            return publish_streamed(engine, source, layout, revision, stamp, dictionary_len)
                .map(|_| ());
        }
    };
    let Some(durable) = &shared.durable else {
        next.committed_micros = stamp.take(shared).0;
        let _compaction = shared.versions.compaction_slot.lock();
        shared.versions.install(Arc::new(next));
        return Ok(());
    };
    let next = Arc::new(next);
    let revision = next.revision;
    let _checkpoint = super::admit(&durable.checkpoint_slot, "checkpoint")?;
    let image = Snapshot::new(
        Arc::clone(&next),
        Arc::clone(&shared.dictionary),
        Arc::clone(&shared.statistics),
        None,
        Arc::clone(&shared.views.read()),
    );
    let mut committed_micros = None;
    let path = commit_image(durable, revision, || {
        checkpoint::write_stamped(
            durable.root(),
            &image,
            shared.admission.as_ref(),
            &mut || {
                let times = stamp.take(shared);
                committed_micros = times.0;
                times
            },
        )
    })?;
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
                asserted_content: next.asserted_content,
                asserted,
                inferred,
                equality: Arc::clone(&next.equality),
                revision,
                committed_micros,
                dictionary_len: next.dictionary_len,
                canonical: Default::default(),
            })
        }
        Some(Err(error)) => {
            tracing::warn!(%error, "checkpoint not mapped; data stays in memory");
            Arc::new(next.with_time(committed_micros))
        }
        None => Arc::new(next.with_time(committed_micros)),
    };
    // The checkpoint covers every logged revision: later commits go to the segment
    // `commit_image` started, and all older segments can go.
    let compaction = shared.versions.compaction_slot.lock();
    let mut wal = durable.wal.lock();
    shared.versions.install(next);
    release_covered(&mut wal, durable.root(), revision);
    drop(wal);
    drop(compaction);
    if let Some(base) = base
        && let Err(error) = shared.dictionary.rebase(base)
    {
        tracing::warn!(%error, "checkpoint dictionary not mapped; it stays in memory");
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
    stamp: Stamp,
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
            asserted_content: super::AssertedIdentity::fresh(),
            asserted,
            inferred: IndexVersion::empty(Layout::DefaultGraph),
            revision,
            committed_micros: None,
            dictionary_len,
            equality: Default::default(),
            canonical: Default::default(),
        };
        let len = next.asserted.len();
        publish(engine, Next::Built(next, stamp))?;
        return Ok(len);
    };
    let _checkpoint = super::admit(&durable.checkpoint_slot, "checkpoint")?;
    let empty = PackedKeys::from_sorted(&[]);
    // Every layout has SPOG: its length is the number of quads.
    let mut len = 0;
    let mut committed_micros = None;
    let path = commit_image(durable, revision, || {
        checkpoint::write_parts(
            durable.root(),
            shared.admission.as_ref(),
            revision,
            &mut || {
                let times = stamp.take(shared);
                committed_micros = times.0;
                times
            },
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
        )
    });
    drop(source);
    let path = path?;
    shared
        .checkpoints
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (base, [asserted, inferred]) = match checkpoint::map_written(&path) {
        Ok(mapped) => mapped,
        Err(error) => {
            // Committed (the checkpoint is in place), but nothing else holds the data to
            // serve it: no write may follow before a reopen recovers it.
            durable.wal.lock().poison();
            tracing::error!(%error, revision, "a committed bulk load's checkpoint couldn't be mapped; reopen the engine to serve it");
            return Err(EngineError::CommitUnsettled { revision });
        }
    };
    let next = Arc::new(Version {
        content: super::Content::fresh(),
        // New: always sound for a cache keyed by it (a rescan), kept only where certain.
        asserted_content: super::AssertedIdentity::fresh(),
        asserted,
        inferred,
        revision,
        committed_micros,
        dictionary_len,
        equality: Default::default(),
        canonical: Default::default(),
    });
    let compaction = shared.versions.compaction_slot.lock();
    let mut wal = durable.wal.lock();
    shared.versions.install(next);
    release_covered(&mut wal, durable.root(), revision);
    drop(wal);
    drop(compaction);
    if let Err(error) = shared.dictionary.rebase(base) {
        tracing::warn!(%error, "checkpoint dictionary not mapped; it stays in memory");
    }
    Ok(len)
}

/// Writes, with `write`, the checkpoint of `revision`, a new revision of a bulk load or
/// rematerialisation, and returns its path: the commit point (EEV-01, B-301). The caller
/// holds the writer, compaction and checkpoint slots.
///
/// Before it the log moves to a new segment, the one step that can leave it unfit for the
/// commits after; past it nothing may report the revision uncommitted, since a restart
/// recovers it. A write that failed with the checkpoint possibly in place (its rename's
/// outcome unknown, its directory unsynced) poisons the log: no commit of any kind can then
/// take the revision again before a reopen settles which one is on disk.
fn commit_image(
    durable: &Durable,
    revision: u64,
    write: impl FnOnce() -> EngineResult<PathBuf>,
) -> EngineResult<PathBuf> {
    durable.wal.lock().rotate(revision)?;
    let error = match write() {
        Ok(path) => return Ok(path),
        Err(error) => error,
    };
    match checkpoint::placed(durable.root(), revision) {
        Ok(false) => Err(error),
        placed => {
            durable.wal.lock().poison();
            tracing::error!(%error, revision, ?placed, "a bulk publication's outcome is unknown until the engine is reopened");
            Err(EngineError::CommitUnsettled { revision })
        }
    }
}

/// Removes what a published checkpoint at `revision` covers: the log segments before it and
/// the older checkpoints. After the version is installed this is cleanup, not part of the
/// commit, which has happened (its checkpoint is written and synced): a failure keeps those
/// files, which recovery skips (the newest checkpoint and the records after it count), and the
/// next checkpoint that writes removes them. Returning it would report a published commit as
/// failed.
fn release_covered(wal: &mut crate::durability::wal::Wal, root: &std::path::Path, revision: u64) {
    if let Err(error) = wal.release_through(revision) {
        tracing::warn!(%error, revision, "log segments the checkpoint covers kept for now; the next checkpoint releases them");
    }
    if let Err(error) = checkpoint::remove_older_than(root, revision) {
        tracing::warn!(%error, revision, "older checkpoints kept for now; the next checkpoint removes them");
    }
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
        // The new stack is built from `base` alone (the writer slot keeps it the latest
        // content) without the compaction slot, which `publish` takes to install it: nothing
        // that waits for that slot waits for this parallel work.
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
        let summary = CommitSummary {
            revision: base.revision() + 1,
            inserted: 0,
            deleted: 0,
            inferred_inserted: inserted as u64,
            inferred_deleted: deleted as u64,
            // Rematerialisation keeps the asserted stack.
            asserted: base.asserted_identity(),
        };
        if inserted + deleted == 0 {
            return Ok(CommitSummary {
                revision: base.revision(),
                ..summary
            });
        }
        let inferred = IndexVersion::from_quads(Layout::DefaultGraph, quads);
        // A merge may have replaced the asserted runs since `base` was taken, with the same
        // statements: the latest ones are kept.
        let current = shared.snapshot();
        let current = current.version();
        debug_assert_eq!(current.revision, base.revision(), "single writer");
        let next = Version {
            content: super::Content::fresh(),
            // Rematerialisation: the inferred stack only.
            asserted_content: current.asserted_content,
            asserted: current.asserted.clone(),
            inferred,
            revision: summary.revision,
            // Stamped as it commits ([`publish`]).
            committed_micros: None,
            dictionary_len: shared.dictionary.len(),
            equality: Default::default(),
            canonical: Default::default(),
        };
        // It keeps the asserted stack: of unknown time if that is.
        let stamp = Stamp {
            previous: current.committed_micros,
        };
        publish(engine, Next::Built(next, stamp))?;
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
