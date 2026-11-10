//! The read contract the SPARQL layer needs from the engine.
//!
//! Queries read a committed [`Snapshot`]; the `WHERE` part of an update reads the open
//! transaction, so later operations in one request see earlier ones. A transaction is read
//! through [`PendingRead`]: either kind, the write [`Transaction`] or a
//! [`PrivateTransaction`](nrese_engine::PrivateTransaction) whose new terms are its own and
//! never enter the dictionary, with the budget its pending snapshots are made against. All
//! implement [`ReadView`], and everything above this module is written once against the trait.
//!
//! Reads take a [`ReadModel`]: asserted and inferred statements (the default), or either
//! stack alone.

use std::borrow::Cow;
use std::sync::Arc;

use nrese_engine::{
    EncodedQuad, EngineError, PendingView, QuadPattern, ReadModel, Snapshot, TermId, Transaction,
};
use nrese_exec::Budget;
use nrese_rdf::{Quad, Term, TermRef};

use crate::results::QueryEvaluationError;

pub trait ReadView {
    /// Quads matching `pattern` in `model`. The iterator borrows the view, not the pattern.
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, Self>;

    /// Asserted and inferred quads matching `pattern`.
    fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, Self> {
        self.quads_for_pattern_in(ReadModel::Materialised, pattern)
    }

    /// Non-empty named graphs.
    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, Self>;

    fn contains_named_graph(&self, graph: TermId) -> bool;

    /// Id of `term` if this view knows it. Must return the same id for the same term for
    /// the lifetime of the view; the evaluator's term equality depends on it.
    fn lookup(&self, term: TermRef<'_>) -> Option<TermId>;

    fn decode(&self, id: TermId) -> Option<Term>;

    /// Decodes a quad read from this view; `None` only for ids the view doesn't know.
    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad>;

    /// The committed snapshot behind this view, if it is one.
    fn snapshot(&self) -> Option<&Snapshot> {
        None
    }

    /// A snapshot of what this view reads, for the executor: the view itself if it is a
    /// snapshot, else one of its state (a transaction's pending changes over its base) in
    /// the view's own vocabulary. Made before anything is evaluated. Refused (a private
    /// transaction's frozen vocabulary is charged), the evaluation fails with the refusal:
    /// it never reads another state, whose ids would disagree with the view's lookups.
    fn evaluation_snapshot(&self) -> Result<Cow<'_, Snapshot>, QueryEvaluationError>;
}

/// A transaction of either kind as SPARQL reads it, its pending snapshots made against
/// `budget` ([`PendingView::pending_snapshot_within`]). One per request: every evaluation over
/// it uses the same budget.
pub struct PendingRead<'t, T> {
    tx: &'t T,
    budget: &'t Arc<Budget>,
}

impl<'t, T: PendingView> PendingRead<'t, T> {
    pub fn new(tx: &'t T, budget: &'t Arc<Budget>) -> Self {
        Self { tx, budget }
    }
}

/// Whether `tx` changed anything, asserted or inferred, over its base.
pub(crate) fn changed(tx: &impl PendingView) -> bool {
    tx.pending() != (0, 0) || tx.inferred_pending() != (0, 0)
}

/// A transaction's refusal as the evaluation's: its memory as any other memory refusal (past
/// the request's envelope), anything else unexpected (as the update's refused writes are).
pub(crate) fn engine_refusal(error: EngineError) -> QueryEvaluationError {
    match error {
        EngineError::MemoryLimit(exceeded) => QueryEvaluationError::MemoryLimit(exceeded),
        error => QueryEvaluationError::Unexpected(error.to_string()),
    }
}

impl<'t, T: PendingView> ReadView for PendingRead<'t, T> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 't, T> {
        self.tx.quads_for_pattern_in(model, pattern)
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, 't, T> {
        self.tx.named_graphs().into_iter()
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        self.tx.contains_named_graph(graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.tx.lookup(term)
    }

    fn decode(&self, id: TermId) -> Option<Term> {
        self.tx.decode(id)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.tx.decode_quad(quad)
    }

    fn evaluation_snapshot(&self) -> Result<Cow<'_, Snapshot>, QueryEvaluationError> {
        if changed(self.tx) {
            self.tx
                .pending_snapshot_within(self.budget)
                .map(Cow::Owned)
                .map_err(engine_refusal)
        } else {
            Ok(Cow::Borrowed(self.tx.base()))
        }
    }
}

impl ReadView for Snapshot {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        Snapshot::quads_for_pattern_in(self, model, pattern)
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a> {
        Snapshot::named_graphs(self)
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        Snapshot::contains_named_graph(self, graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        Snapshot::lookup(self, term)
    }

    fn decode(&self, id: TermId) -> Option<Term> {
        Snapshot::decode(self, id)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        Snapshot::decode_quad(self, quad)
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        Some(self)
    }

    fn evaluation_snapshot(&self) -> Result<Cow<'_, Snapshot>, QueryEvaluationError> {
        Ok(Cow::Borrowed(self))
    }
}

/// A write transaction read as it is today: its pending snapshot uncharged (DUR-6 moves its
/// readers onto [`PendingRead`] when it charges them).
impl<'e> ReadView for Transaction<'e> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        Transaction::quads_for_pattern_in(self, model, pattern)
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, 'e> {
        Transaction::named_graphs(self).into_iter()
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        Transaction::contains_named_graph(self, graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        Transaction::lookup(self, term)
    }

    fn decode(&self, id: TermId) -> Option<Term> {
        Transaction::decode(self, id)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        Transaction::decode_quad(self, quad)
    }

    fn evaluation_snapshot(&self) -> Result<Cow<'_, Snapshot>, QueryEvaluationError> {
        Ok(if changed(self) {
            Cow::Owned(self.pending_snapshot())
        } else {
            Cow::Borrowed(self.base())
        })
    }
}
