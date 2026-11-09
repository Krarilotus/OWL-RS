//! What both transaction kinds share: a write transaction ([`Transaction`]) and a private one
//! ([`PrivateTransaction`]). Code that applies changes, or reads pending state, over either
//! kind is generic over these traits, so one body serves both and the private kind can't be
//! passed where only the shared kind belongs (its commit, its identity reservations). Dictionary
//! lifecycle, V1 (`notes/2026-11-09-dict-v1-speculative-private-design.md` §1.1).
//!
//! - **Sealed:** only this crate's two transaction kinds implement them.
//! - **Every write can be refused** ([`crate::EngineError::MemoryLimit`]): a private
//!   transaction's terms and changes are charged to its budget. A write transaction answers
//!   what its inherent method answers: `Ok` of it where that method can't be refused, and its
//!   own result where it can (`remove_matching`, its pending growth charged since D16-039).
//! - **A pending snapshot is made only against a budget:** there is no budget-free builder
//!   here.
//! - **Generic bounds, never `dyn`:** the iterator returns are `impl Iterator`, and every call
//!   is resolved at compile time to the kind's own method.

use std::sync::Arc;

use nrese_exec::Budget;
use nrese_rdf::{Quad, QuadRef, Term, TermRef};

use super::{PrivateTransaction, ReadModel, Snapshot, Transaction};
use crate::error::EngineResult;
use crate::quad::{EncodedQuad, EncodedTriple, QuadPattern};
use crate::term::TermId;

mod sealed {
    /// Only the engine's transaction kinds.
    pub trait Sealed {}
    impl Sealed for super::Transaction<'_> {}
    impl Sealed for super::PrivateTransaction<'_> {}
}

/// Reading a transaction: its base, its pending changes, and the state they make, in its own
/// vocabulary. Every id it gives or takes is one of that vocabulary's.
pub trait PendingView: sealed::Sealed {
    /// The snapshot the transaction started from, without its pending changes.
    fn base(&self) -> &Snapshot;

    /// The base with the pending changes, as a snapshot, its making charged to `budget`
    /// ([`Transaction::pending_snapshot`], [`PrivateTransaction::pending_snapshot`]).
    fn pending_snapshot_within(&self, budget: &Arc<Budget>) -> EngineResult<Snapshot>;

    /// [`Self::pending_snapshot_within`] as a reader of `view`, a restricted snapshot of the
    /// base, reads it ([`Transaction::pending_over`]).
    fn pending_over_within(
        &self,
        view: &Snapshot,
        hidden: &[EncodedQuad],
        budget: &Arc<Budget>,
    ) -> EngineResult<Snapshot>;

    /// Number of quads visible in `model`, including pending changes.
    fn len_in(&self, model: ReadModel) -> u64;

    /// Pending asserted (inserted, deleted) counts.
    fn pending(&self) -> (usize, usize);

    /// Pending inferred (inserted, deleted) counts.
    fn inferred_pending(&self) -> (usize, usize);

    /// Pending asserted inserts.
    fn inserted(&self) -> impl Iterator<Item = EncodedQuad> + '_;

    /// Pending asserted deletes.
    fn deleted(&self) -> impl Iterator<Item = EncodedQuad> + '_;

    /// Pending inferred inserts.
    fn inferred_inserted(&self) -> impl Iterator<Item = EncodedTriple> + '_;

    /// Pending inferred deletes.
    fn inferred_deleted(&self) -> impl Iterator<Item = EncodedTriple> + '_;

    fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool;

    /// Quads matching `pattern` in `model`, including pending changes. The iterator borrows
    /// the transaction only, never `pattern`: a scan may outlive the pattern it was made from.
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, Self>;

    /// True if the named graph `graph` has a quad, including pending changes.
    fn contains_named_graph(&self, graph: TermId) -> bool;

    /// Non-empty named graphs, including pending changes, in id order.
    fn named_graphs(&self) -> Vec<TermId>;

    /// Id of `term` in the transaction's vocabulary, if it has one. Never adds it.
    fn lookup(&self, term: TermRef<'_>) -> Option<TermId>;

    fn decode(&self, id: TermId) -> Option<Term>;

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad>;
}

/// Changing a transaction. Every write can be refused ([`crate::EngineError::MemoryLimit`]);
/// a refused write changes nothing it reports, though a triple term's components interned
/// before the refusal stay in the vocabulary.
pub trait PendingWrite: PendingView {
    /// The id of `term` in the transaction's vocabulary, added if it has none.
    fn try_intern(&self, term: TermRef<'_>) -> EngineResult<TermId>;

    /// Asserts `quad`, its terms interned as [`Self::try_intern`] does; `false` if it was
    /// already asserted.
    fn try_insert(&mut self, quad: QuadRef<'_>) -> EngineResult<bool>;

    /// Retracts the asserted `quad`; `false` if it was not asserted. Never interns.
    fn remove(&mut self, quad: QuadRef<'_>) -> EngineResult<bool>;

    /// Retracts every asserted quad matching `pattern`; the number removed.
    fn remove_matching(&mut self, pattern: &QuadPattern) -> EngineResult<u64>;

    /// Whether a budget pays for the pending changes' growth: a write transaction's where one
    /// is attached, a private one's always (its budget, from its opening).
    fn charges_pending(&self) -> bool;

    /// Room for `more` asserted inserts, charged before the pending changes grow: the next
    /// `more` inserts allocate nothing. A write transaction without a budget attached refuses
    /// it ([`crate::EngineError::Configuration`]), so a caller asks [`Self::charges_pending`]
    /// first (SPARQL `ADD`/`MOVE`/`COPY`).
    fn try_reserve_inserts(&mut self, more: usize) -> EngineResult<()>;

    /// Asserts an encoded quad, its ids from this transaction's vocabulary, its growth charged;
    /// `false` if it was already asserted.
    fn try_insert_encoded(&mut self, quad: EncodedQuad) -> EngineResult<bool>;

    /// Adds an inferred statement (in the default graph); `false` if it is already inferred,
    /// or asserted. Reasoner only.
    fn insert_inferred(&mut self, triple: EncodedTriple) -> EngineResult<bool>;

    /// Removes an inferred statement; `false` if it was not visible as inferred. Reasoner only.
    fn remove_inferred(&mut self, triple: EncodedTriple) -> EngineResult<bool>;
}

/// A write transaction answers its inherent methods, each the charged one where it has one
/// (DUR-6's pending snapshots, D16-039's pending growth), so a caller through the trait is
/// charged as one calling the transaction itself.
impl<'e> PendingView for Transaction<'e> {
    #[inline]
    fn base(&self) -> &Snapshot {
        Transaction::base(self)
    }

    #[inline]
    fn pending_snapshot_within(&self, budget: &Arc<Budget>) -> EngineResult<Snapshot> {
        Transaction::pending_snapshot_within(self, budget)
    }

    #[inline]
    fn pending_over_within(
        &self,
        view: &Snapshot,
        hidden: &[EncodedQuad],
        budget: &Arc<Budget>,
    ) -> EngineResult<Snapshot> {
        Transaction::pending_over_within(self, view, hidden, budget)
    }

    #[inline]
    fn len_in(&self, model: ReadModel) -> u64 {
        Transaction::len_in(self, model)
    }

    #[inline]
    fn pending(&self) -> (usize, usize) {
        Transaction::pending(self)
    }

    #[inline]
    fn inferred_pending(&self) -> (usize, usize) {
        Transaction::inferred_pending(self)
    }

    #[inline]
    fn inserted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        Transaction::inserted(self)
    }

    #[inline]
    fn deleted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        Transaction::deleted(self)
    }

    #[inline]
    fn inferred_inserted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        Transaction::inferred_inserted(self)
    }

    #[inline]
    fn inferred_deleted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        Transaction::inferred_deleted(self)
    }

    #[inline]
    fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool {
        Transaction::contains_in(self, model, quad)
    }

    #[inline]
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        Transaction::quads_for_pattern_in(self, model, pattern)
    }

    #[inline]
    fn contains_named_graph(&self, graph: TermId) -> bool {
        Transaction::contains_named_graph(self, graph)
    }

    #[inline]
    fn named_graphs(&self) -> Vec<TermId> {
        Transaction::named_graphs(self)
    }

    #[inline]
    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        Transaction::lookup(self, term)
    }

    #[inline]
    fn decode(&self, id: TermId) -> Option<Term> {
        Transaction::decode(self, id)
    }

    #[inline]
    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        Transaction::decode_quad(self, quad)
    }
}

impl PendingWrite for Transaction<'_> {
    #[inline]
    fn try_intern(&self, term: TermRef<'_>) -> EngineResult<TermId> {
        Ok(Transaction::intern(self, term))
    }

    #[inline]
    fn try_insert(&mut self, quad: QuadRef<'_>) -> EngineResult<bool> {
        Transaction::try_insert(self, quad)
    }

    #[inline]
    fn remove(&mut self, quad: QuadRef<'_>) -> EngineResult<bool> {
        Transaction::try_remove(self, quad)
    }

    #[inline]
    fn charges_pending(&self) -> bool {
        Transaction::charges_pending(self)
    }

    #[inline]
    fn try_reserve_inserts(&mut self, more: usize) -> EngineResult<()> {
        Transaction::try_reserve_inserts(self, more)
    }

    #[inline]
    fn try_insert_encoded(&mut self, quad: EncodedQuad) -> EngineResult<bool> {
        Transaction::try_insert_encoded(self, quad)
    }

    #[inline]
    fn remove_matching(&mut self, pattern: &QuadPattern) -> EngineResult<u64> {
        Transaction::remove_matching(self, pattern)
    }

    #[inline]
    fn insert_inferred(&mut self, triple: EncodedTriple) -> EngineResult<bool> {
        Ok(Transaction::insert_inferred(self, triple))
    }

    #[inline]
    fn remove_inferred(&mut self, triple: EncodedTriple) -> EngineResult<bool> {
        Ok(Transaction::remove_inferred(self, triple))
    }
}

/// A private transaction answers its inherent methods, the infallible ones as `Ok`. Its
/// pending snapshots' freeze is charged to the budget it was opened with; `budget` keys its
/// memo ([`PrivateTransaction::pending_snapshot_within`]).
impl<'e> PendingView for PrivateTransaction<'e> {
    #[inline]
    fn base(&self) -> &Snapshot {
        PrivateTransaction::base(self)
    }

    #[inline]
    fn pending_snapshot_within(&self, budget: &Arc<Budget>) -> EngineResult<Snapshot> {
        PrivateTransaction::pending_snapshot_within(self, budget)
    }

    #[inline]
    fn pending_over_within(
        &self,
        view: &Snapshot,
        hidden: &[EncodedQuad],
        _budget: &Arc<Budget>,
    ) -> EngineResult<Snapshot> {
        PrivateTransaction::pending_over(self, view, hidden)
    }

    #[inline]
    fn len_in(&self, model: ReadModel) -> u64 {
        PrivateTransaction::len_in(self, model)
    }

    #[inline]
    fn pending(&self) -> (usize, usize) {
        PrivateTransaction::pending(self)
    }

    #[inline]
    fn inferred_pending(&self) -> (usize, usize) {
        PrivateTransaction::inferred_pending(self)
    }

    #[inline]
    fn inserted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        PrivateTransaction::inserted(self)
    }

    #[inline]
    fn deleted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        PrivateTransaction::deleted(self)
    }

    #[inline]
    fn inferred_inserted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        PrivateTransaction::inferred_inserted(self)
    }

    #[inline]
    fn inferred_deleted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        PrivateTransaction::inferred_deleted(self)
    }

    #[inline]
    fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool {
        PrivateTransaction::contains_in(self, model, quad)
    }

    #[inline]
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        PrivateTransaction::quads_for_pattern_in(self, model, pattern)
    }

    #[inline]
    fn contains_named_graph(&self, graph: TermId) -> bool {
        PrivateTransaction::contains_named_graph(self, graph)
    }

    #[inline]
    fn named_graphs(&self) -> Vec<TermId> {
        PrivateTransaction::named_graphs(self)
    }

    #[inline]
    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        PrivateTransaction::lookup(self, term)
    }

    #[inline]
    fn decode(&self, id: TermId) -> Option<Term> {
        PrivateTransaction::decode(self, id)
    }

    #[inline]
    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        PrivateTransaction::decode_quad(self, quad)
    }
}

impl PendingWrite for PrivateTransaction<'_> {
    #[inline]
    fn try_intern(&self, term: TermRef<'_>) -> EngineResult<TermId> {
        PrivateTransaction::try_intern(self, term)
    }

    #[inline]
    fn try_insert(&mut self, quad: QuadRef<'_>) -> EngineResult<bool> {
        PrivateTransaction::try_insert(self, quad)
    }

    #[inline]
    fn remove(&mut self, quad: QuadRef<'_>) -> EngineResult<bool> {
        PrivateTransaction::remove(self, quad)
    }

    #[inline]
    fn remove_matching(&mut self, pattern: &QuadPattern) -> EngineResult<u64> {
        PrivateTransaction::remove_matching(self, pattern)
    }

    #[inline]
    fn charges_pending(&self) -> bool {
        true
    }

    #[inline]
    fn try_reserve_inserts(&mut self, more: usize) -> EngineResult<()> {
        PrivateTransaction::try_reserve_inserts(self, more)
    }

    #[inline]
    fn try_insert_encoded(&mut self, quad: EncodedQuad) -> EngineResult<bool> {
        PrivateTransaction::try_insert_encoded(self, quad)
    }

    #[inline]
    fn insert_inferred(&mut self, triple: EncodedTriple) -> EngineResult<bool> {
        Ok(PrivateTransaction::insert_inferred(self, triple))
    }

    #[inline]
    fn remove_inferred(&mut self, triple: EncodedTriple) -> EngineResult<bool> {
        Ok(PrivateTransaction::remove_inferred(self, triple))
    }
}
