//! A private transaction: speculative changes over a given snapshot whose new terms are its own
//! and never enter the dictionary ([`Engine::speculative_over`](super::Engine::speculative_over)).
//! It never commits. Dictionary lifecycle, package 2b (`notes/2026-11-08-p2b-snapshot-switch-design.md`
//! §1 point 5 and §2).
//!
//! - **One vocabulary for life:** one overlay over the dictionary's entries below the given
//!   snapshot's length, its `start`, fixed here. A term the dictionary had there keeps its id;
//!   any other gets the overlay's own, charged to the transaction's budget.
//! - **Private by construction:** its pending snapshots hold a frozen view of the overlay, and
//!   every snapshot made from them keeps it. They don't search, checkpoint or save statistics,
//!   and record no corpus lineage.
//! - **The pending changes are the write transaction's type** ([`super::transaction::Pending`]),
//!   with the same exactness and disjointness. No way into a commit record.

use std::sync::Arc;

use nrese_exec::Budget;
use nrese_rdf::{Quad, QuadRef, Term, TermRef};
use parking_lot::Mutex;

use super::transaction::{Pending, SnapshotChanges};
use super::{Inner, ReadModel, Snapshot, Version};
use crate::error::{EngineError, EngineResult};
use crate::quad::{EncodedQuad, EncodedTriple, QuadPattern};
use crate::term::TermId;
use crate::term::overlay::Overlay;

/// Speculative changes over one snapshot, with a vocabulary of their own: no writer slot, no
/// commit, nothing added to the dictionary.
///
/// It has no commit, by type:
///
/// ```compile_fail
/// let engine = nrese_engine::Engine::new(Default::default()).unwrap();
/// let tx = engine.speculative(std::sync::Arc::new(nrese_exec::Budget::unlimited()));
/// tx.commit();
/// ```
pub struct PrivateTransaction<'e> {
    /// Its engine, which it doesn't outlive, as a write transaction doesn't.
    _engine: &'e Inner,
    /// The transaction's vocabulary: the dictionary below the base's length, then its own.
    overlay: Arc<Overlay>,
    /// The base snapshot and the pending changes over it.
    pending: Pending,
    /// Its change generation: one more for every write that changed the pending changes.
    changes: u64,
    /// The last [`Self::pending_snapshot_within`], while its key holds.
    memo: Mutex<Option<Memo>>,
}

/// A pending snapshot and what it was made of: the change generation, the vocabulary's end
/// (an intern after it isn't in its frozen view) and the budget it was asked for, by identity.
struct Memo {
    changes: u64,
    end: u64,
    budget: Arc<Budget>,
    snapshot: Snapshot,
}

impl<'e> PrivateTransaction<'e> {
    /// Over `base`, a published snapshot of `engine`, its own terms charged to `budget`.
    pub(super) fn over(
        engine: &'e Inner,
        base: &Snapshot,
        budget: Arc<Budget>,
    ) -> EngineResult<Self> {
        let Some(dictionary) = base.shared_dictionary() else {
            return Err(EngineError::Configuration(
                "a private transaction opens over a published snapshot, not a private one"
                    .to_owned(),
            ));
        };
        if !Arc::ptr_eq(dictionary, &engine.shared.dictionary) {
            return Err(EngineError::Configuration(
                "a private transaction opens over a snapshot of its own engine".to_owned(),
            ));
        }
        let start = base.dictionary_len();
        Ok(Self::opened(engine, base.clone(), start, budget))
    }

    /// Over `engine`'s latest snapshot, which is its own and published. Its vocabulary starts
    /// at the dictionary's length now, read after the snapshot: past the snapshot's own
    /// length (its last commit's) by the names appended since without the writer slot
    /// ([`super::Engine::intern_system_vocabulary`]). Those keep their shared ids here, as
    /// state that holds them (the DL bounds) has them, and no own id can equal one.
    pub(super) fn over_latest(engine: &'e Inner, budget: Arc<Budget>) -> Self {
        let base = engine.shared.snapshot();
        let start = engine.shared.dictionary.len().max(base.dictionary_len());
        Self::opened(engine, base, start, budget)
    }

    /// Over `base`, a published snapshot of `engine` (checked by the callers), its own ids
    /// from `start`, at least `base`'s length.
    fn opened(engine: &'e Inner, base: Snapshot, start: u64, budget: Arc<Budget>) -> Self {
        let dictionary = Arc::clone(&engine.shared.dictionary);
        // The pending changes are charged to the same budget as the terms, for their life. A
        // new `Pending` has no budget yet, so attaching this one is never refused.
        let mut pending = Pending::over(base);
        if let Err(error) = pending.charge_to(Arc::clone(&budget)) {
            unreachable!("a new Pending has no budget yet: {error}");
        }
        let overlay = Overlay::over(dictionary, start, budget);
        Self {
            _engine: engine,
            overlay,
            pending,
            changes: 0,
            memo: Mutex::new(None),
        }
    }

    /// Whether every id `published` names keeps its term in this transaction's vocabulary:
    /// a snapshot of the same engine whose dictionary length is at most this vocabulary's
    /// start. Then its ids and this transaction's own (from that start on) never collide, and
    /// the two can be compared by raw id: the precondition of every API that mixes a published
    /// input with this transaction's ids (dictionary lifecycle V1, the bounded-prefix case).
    pub fn extends(&self, published: &Snapshot) -> bool {
        published.shares_dictionary_with(self.base())
            && published.dictionary_len() <= self.overlay.start()
    }

    /// `changed`, counted into the change generation when it is true.
    fn note(&mut self, changed: bool) -> bool {
        self.changes += u64::from(changed);
        changed
    }

    /// The id of `term` in this transaction's vocabulary, added as its own if the base didn't
    /// have it. Refused ([`EngineError::MemoryLimit`]) where the budget has no room; a triple
    /// term's components added before the refusal stay.
    pub fn try_intern(&self, term: TermRef<'_>) -> EngineResult<TermId> {
        Ok(self.overlay.intern(term)?)
    }

    /// Asserts `quad`, its terms interned as [`Self::try_intern`] does; `false` if it was
    /// already asserted. The pending changes' growth is charged as
    /// [`Self::try_insert_encoded`]'s; refused there, the terms interned for it stay, its own and
    /// charged, as a refused triple term's components do.
    pub fn try_insert(&mut self, quad: QuadRef<'_>) -> EngineResult<bool> {
        let encoded = self.overlay.intern_quad(quad)?;
        self.try_insert_encoded(encoded)
    }

    /// Retracts the asserted `quad`; `false` if it was not asserted. Never interns. Its growth
    /// is charged as [`Self::try_insert_encoded`]'s.
    pub fn remove(&mut self, quad: QuadRef<'_>) -> EngineResult<bool> {
        match self.overlay.lookup_quad(quad) {
            Some(encoded) => self.try_remove_encoded(encoded),
            None => Ok(false),
        }
    }

    /// Id of `term` in this transaction's vocabulary, if it has one.
    pub fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.overlay.lookup(term)
    }

    pub fn decode(&self, id: TermId) -> Option<Term> {
        self.overlay.decode(id)
    }

    pub fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.overlay.decode_quad(quad)
    }

    /// The snapshot this transaction started from, without its pending changes.
    pub fn base(&self) -> &Snapshot {
        self.pending.base()
    }

    /// The base with the pending changes, as [`super::Transaction::pending_snapshot`] makes it,
    /// reading this transaction's terms: private ([`Snapshot::is_private`]). Charged to the
    /// transaction's budget, as [`Self::build`] charges, so this can be refused.
    pub fn pending_snapshot(&self) -> EngineResult<Snapshot> {
        self.build(self.overlay.budget())
    }

    /// The pending snapshot, what it makes charged to `budget` before it is made and held by
    /// the snapshot, as [`super::Transaction::pending_snapshot_within`] charges (DUR-6);
    /// freezing the vocabulary is charged to the transaction's budget.
    fn build(&self, budget: &Arc<Budget>) -> EngineResult<Snapshot> {
        let view = self.overlay.freeze()?;
        let mut scratch = nrese_exec::Reservation::new(Arc::clone(budget));
        scratch.charge(self.pending.snapshot_changes_bound())?;
        let SnapshotChanges {
            asserted_inserts,
            asserted_deletes,
            inferred_inserts,
            inferred_deletes,
        } = self.pending.snapshot_changes();
        let base = self.pending.base();
        let seen = base.version();
        let end = view.end();
        let snapshot = base.pending_within(
            [&asserted_inserts[..], &asserted_deletes[..]],
            [&inferred_inserts[..], &inferred_deletes[..]],
            &self._engine.shared.forms,
            budget,
            |asserted, inferred| own_version(seen, asserted, inferred, end),
        )?;
        // The lists go before their charge.
        drop((
            asserted_inserts,
            asserted_deletes,
            inferred_inserts,
            inferred_deletes,
            scratch,
        ));
        Ok(snapshot.private(view))
    }

    /// [`Self::pending_snapshot`], the same snapshot (one identity, so one own-state slot) for
    /// as long as nothing changed: no write that changed the pending changes, no new term, and
    /// the same `budget` (by identity) as the call that made it. The memo holds the snapshot,
    /// and its vocabulary's charge, until the next such call after a change, or the drop.
    ///
    /// The memo's lock is held only to look an entry up, take one out or put one in: never
    /// while a snapshot is built (that runs parallel work on Rayon's pool, from
    /// `Run::from_delta`'s threshold on, which a reader waiting on the lock from that pool
    /// would block) or dropped. Two calls that miss at once both build; the first to install
    /// wins, and the other's snapshot, with its charge, is dropped.
    ///
    /// An entry is keyed by the vocabulary end its snapshot froze, not one read before the
    /// build: [`Self::try_intern`] takes `&self`, so another thread can add a term meanwhile.
    pub fn pending_snapshot_within(&self, budget: &Arc<Budget>) -> EngineResult<Snapshot> {
        let fits = |kept: &Memo, end: u64| {
            kept.changes == self.changes && kept.end == end && Arc::ptr_eq(&kept.budget, budget)
        };
        let stale = {
            let mut memo = self.memo.lock();
            let end = self.overlay.end();
            if let Some(kept) = memo.as_ref().filter(|kept| fits(kept, end)) {
                return Ok(kept.snapshot.clone());
            }
            // Taken out first: a refusal below leaves no stale entry, and its charge is freed
            // (below, unlocked) before the new freeze is charged.
            memo.take()
        };
        drop(stale);
        let snapshot = self.build(budget)?;
        let frozen = snapshot.dictionary_len();
        // The answer, and what goes once the lock is let go: our snapshot if another call
        // installed this state's meanwhile, else the entry ours replaced.
        let (answer, ours, replaced) = {
            let mut memo = self.memo.lock();
            if let Some(kept) = memo.as_ref().filter(|kept| fits(kept, frozen)) {
                // Its identity is the one every reader gets.
                (kept.snapshot.clone(), Some(snapshot), None)
            } else {
                // Any other entry is another key's; replaced.
                let replaced = memo.replace(Memo {
                    changes: self.changes,
                    end: frozen,
                    budget: Arc::clone(budget),
                    snapshot: snapshot.clone(),
                });
                (snapshot, None, replaced)
            }
        };
        drop((ours, replaced));
        Ok(answer)
    }

    /// Test support: the memo's key end and the vocabulary end its snapshot froze, if it holds
    /// one.
    #[cfg(test)]
    pub(super) fn memo_ends(&self) -> Option<(u64, u64)> {
        let memo = self.memo.lock();
        memo.as_ref()
            .map(|kept| (kept.end, kept.snapshot.dictionary_len()))
    }

    /// [`Self::pending_snapshot`] as a reader of `view` reads it, as
    /// [`super::Transaction::pending_over`] makes it: `view` a restricted snapshot of the base.
    /// Charged to the transaction's budget, as [`Self::pending_over_within`] charges.
    pub fn pending_over(&self, view: &Snapshot, hidden: &[EncodedQuad]) -> EngineResult<Snapshot> {
        self.pending_over_within(view, hidden, self.overlay.budget())
    }

    /// [`Self::pending_over`], what it makes charged to `budget` before it is made and held by
    /// the snapshot, as [`super::Transaction::pending_over_within`] charges (DUR-6); freezing
    /// the vocabulary is charged to the transaction's budget.
    pub fn pending_over_within(
        &self,
        view: &Snapshot,
        hidden: &[EncodedQuad],
        budget: &Arc<Budget>,
    ) -> EngineResult<Snapshot> {
        let vocabulary = self.overlay.freeze()?;
        let mut scratch = nrese_exec::Reservation::new(Arc::clone(budget));
        scratch.charge(self.pending.over_bound(hidden))?;
        let seen = view.version();
        let gone = self.pending.over_gone(&seen.inferred, hidden);
        let asserted_inserts: Vec<EncodedQuad> = self.pending.asserted().inserted().collect();
        let asserted_deletes: Vec<EncodedQuad> = self.pending.asserted().deleted().collect();
        let end = vocabulary.end();
        let snapshot = view.pending_within(
            [&asserted_inserts[..], &asserted_deletes[..]],
            [&[][..], &gone[..]],
            &self._engine.shared.forms,
            budget,
            |asserted, inferred| own_version(seen, asserted, inferred, end),
        )?;
        drop((gone, asserted_inserts, asserted_deletes, scratch));
        Ok(snapshot.private(vocabulary))
    }

    /// Number of asserted and inferred quads, including pending changes.
    pub fn len(&self) -> u64 {
        self.len_in(ReadModel::Materialised)
    }

    pub fn len_in(&self, model: ReadModel) -> u64 {
        self.pending.len_in(model)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pending asserted (inserted, deleted) counts.
    pub fn pending(&self) -> (usize, usize) {
        self.pending.asserted().counts()
    }

    pub fn inserted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.pending.asserted().inserted()
    }

    pub fn deleted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.pending.asserted().deleted()
    }

    /// Pending inferred (inserted, deleted) counts.
    pub fn inferred_pending(&self) -> (usize, usize) {
        self.pending.inferred().counts()
    }

    pub fn contains(&self, quad: &EncodedQuad) -> bool {
        self.contains_in(ReadModel::Materialised, quad)
    }

    pub fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool {
        self.pending.contains_in(model, quad)
    }

    pub fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        self.quads_for_pattern_in(ReadModel::Materialised, pattern)
    }

    pub fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        self.pending.quads_for_pattern_in(model, pattern)
    }

    pub fn contains_named_graph(&self, graph: TermId) -> bool {
        self.pending.contains_named_graph(graph)
    }

    pub fn named_graphs(&self) -> Vec<TermId> {
        self.pending.named_graphs()
    }

    /// Asserts an encoded quad, its ids from this transaction's vocabulary ([`Self::try_intern`]
    /// or the base's). `false` if it was already asserted. The pending changes' growth is
    /// charged to the transaction's budget before it happens (D16-039's `Pending` charge, which
    /// this transaction's opening attached): refused ([`EngineError::MemoryLimit`]), nothing
    /// changes. Every private write is fallible and charged (§1.1).
    pub fn try_insert_encoded(&mut self, quad: EncodedQuad) -> EngineResult<bool> {
        let changed = self.pending.try_insert_asserted(quad)?;
        Ok(self.note(changed))
    }

    /// Retracts an asserted encoded quad; `false` if it was not asserted. Its growth is charged
    /// as [`Self::try_insert_encoded`]'s.
    pub fn try_remove_encoded(&mut self, quad: EncodedQuad) -> EngineResult<bool> {
        let changed = self.pending.try_remove_asserted(quad)?;
        Ok(self.note(changed))
    }

    /// Retracts every asserted quad matching `pattern`; the number removed. What the
    /// retractions grow is charged as [`Self::try_insert_encoded`]'s; refused part way, the
    /// retractions made before the refusal stay, so the refusal counts as a change.
    pub fn remove_matching(&mut self, pattern: &QuadPattern) -> EngineResult<u64> {
        let removed = self.pending.try_remove_matching(pattern).inspect_err(|_| {
            self.note(true);
        })?;
        self.note(removed > 0);
        Ok(removed)
    }

    /// Room for `more` asserted inserts, charged to the transaction's budget before the pending
    /// changes grow: the next `more` inserts allocate nothing (SPARQL `ADD`/`MOVE`/`COPY`).
    pub fn try_reserve_inserts(&mut self, more: usize) -> EngineResult<()> {
        self.pending.try_reserve_inserts(more)
    }

    /// Adds an inferred statement (in the default graph); `false` if it is already inferred,
    /// or asserted.
    pub fn insert_inferred(&mut self, triple: EncodedTriple) -> bool {
        let changed = self.pending.insert_inferred(triple.in_default_graph());
        self.note(changed)
    }

    pub fn remove_inferred(&mut self, triple: EncodedTriple) -> bool {
        let changed = self.pending.remove_inferred(triple.in_default_graph());
        self.note(changed)
    }

    pub fn inferred_inserted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        self.pending.inferred().inserted().map(Into::into)
    }

    pub fn inferred_deleted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        self.pending.inferred().deleted().map(Into::into)
    }
}

/// A private pending version of `seen` with these stacks, every id it knows below `end`. Its
/// derived state is its own: classes built from its statements name its own ids, and the
/// base's cell is the published version's.
fn own_version(
    seen: &Version,
    asserted: crate::index::IndexVersion,
    inferred: crate::index::IndexVersion,
    end: u64,
) -> Version {
    Version {
        content: super::Content::fresh(),
        asserted_content: super::AssertedIdentity::fresh(),
        asserted,
        inferred,
        revision: seen.revision,
        committed_micros: seen.committed_micros,
        dictionary_len: end,
        equality: Default::default(),
        canonical: Default::default(),
        own: Default::default(),
    }
}

#[cfg(test)]
#[path = "private_tests.rs"]
mod tests;
