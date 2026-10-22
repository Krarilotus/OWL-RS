//! The read contract the SPARQL layer needs from the engine.
//!
//! Queries read a committed [`Snapshot`]; the `WHERE` part of an update reads the open
//! [`Transaction`], so later operations in one request see earlier ones. Both implement
//! [`ReadView`], and everything above this module is written once against the trait.
//!
//! Reads take a [`ReadModel`]: asserted and inferred statements (the default), or either
//! stack alone.

use nrese_engine::{EncodedQuad, QuadPattern, ReadModel, Snapshot, TermId, Transaction};
use nrese_rdf::{Quad, Term, TermRef};
use std::borrow::Cow;

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

    /// Value-equivalent to `decode`, with admission before allocating. Every inline,
    /// stored/compressed and quoted-triple form accepted by `decode` must be supported.
    /// This adds the returned term's heap to `held`; temporary charges are released only
    /// after their buffers are freed. Absence/refusal preserves all pre-existing held
    /// bytes. The caller keeps the added charge until it frees the returned term.
    /// This does not add a graph-read or snapshot-membership boundary to `decode`.
    fn decode_within<B: std::ops::Deref<Target = nrese_exec::Budget>>(
        &self,
        id: TermId,
        held: &mut nrese_exec::Reservation<B>,
    ) -> Result<Option<Term>, nrese_exec::BudgetExceeded>;

    /// Decodes a quad read from this view; `None` only for ids the view doesn't know.
    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad>;

    /// The committed snapshot behind this view, if it is one.
    fn snapshot(&self) -> Option<&Snapshot> {
        None
    }

    /// A snapshot of what this view reads, for the executor: the view itself if it is a
    /// snapshot, else one of its state (a transaction's pending changes over its base).
    fn evaluation_snapshot(&self) -> Cow<'_, Snapshot>;
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

    fn decode_within<B: std::ops::Deref<Target = nrese_exec::Budget>>(
        &self,
        id: TermId,
        held: &mut nrese_exec::Reservation<B>,
    ) -> Result<Option<Term>, nrese_exec::BudgetExceeded> {
        Snapshot::decode_within(self, id, held)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        Snapshot::decode_quad(self, quad)
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        Some(self)
    }

    fn evaluation_snapshot(&self) -> Cow<'_, Snapshot> {
        Cow::Borrowed(self)
    }
}

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

    fn decode_within<B: std::ops::Deref<Target = nrese_exec::Budget>>(
        &self,
        id: TermId,
        held: &mut nrese_exec::Reservation<B>,
    ) -> Result<Option<Term>, nrese_exec::BudgetExceeded> {
        Transaction::decode_within(self, id, held)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        Transaction::decode_quad(self, quad)
    }

    fn evaluation_snapshot(&self) -> Cow<'_, Snapshot> {
        if self.pending() == (0, 0) && self.inferred_pending() == (0, 0) {
            Cow::Borrowed(self.base())
        } else {
            Cow::Owned(self.pending_snapshot())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_engine::{Engine, EngineConfig};
    use nrese_exec::{Budget, Reservation, measure};
    use nrese_rdf::{Literal, NamedNode, Triple};

    fn generic_decode(view: &impl ReadView, id: TermId, expected: &Term) {
        assert_eq!(view.decode(id).as_ref(), Some(expected));
        let budget = Budget::unlimited();
        let ((term, prior, held), excess) = measure::measure_within(&budget, || {
            let mut held = Reservation::new(&budget);
            held.charge(37).unwrap();
            let prior = vec![0_u8; 37];
            let term = view.decode_within(id, &mut held).unwrap().unwrap();
            (term, prior, held)
        });
        assert_eq!(&term, expected);
        assert_eq!(excess, 0);
        assert_eq!(held.bytes(), 37 + term.heap_bytes());
        drop((term, prior, held));
        assert_eq!(budget.used(), 0);
        let budget = Budget::new(37);
        let mut held = Reservation::new(&budget);
        held.charge(37).unwrap();
        assert!(view.decode_within(id, &mut held).is_err());
        assert_eq!(held.bytes(), 37);
    }

    #[test]
    fn complete_decode_generic_pending_and_snapshot_dictionary_domain() {
        if !measure::alone() {
            return;
        }
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let snapshot = engine.snapshot();
        let tx = engine.transaction();
        let atomic: Term = Literal::new_simple_literal("pending ".repeat(4096)).into();
        let triple: Term = Triple::new(
            NamedNode::new_unchecked("urn:s"),
            NamedNode::new_unchecked("urn:p"),
            atomic.clone(),
        )
        .into();
        for term in [atomic, triple, Literal::from(42_i64).into()] {
            let id = tx.intern(term.as_ref());
            generic_decode(&tx, id, &term);
            // Ordinary Snapshot::decode knows these IDs despite its older version fence.
            generic_decode(&snapshot, id, &term);
            assert_eq!(
                snapshot.decode_charged(id, |_| Ok::<_, ()>(())),
                Ok(None),
                "the intentionally partial fenced helper remains unchanged"
            );
        }
    }
}
