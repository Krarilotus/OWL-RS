//! The read contract the SPARQL layer needs from the engine.
//!
//! Queries read a committed [`Snapshot`]; the `WHERE` part of an update reads the open
//! [`Transaction`], so later operations in one request see earlier ones. Both implement
//! [`ReadView`], and everything above this module is written once against the trait.

use nrese_engine::{EncodedQuad, QuadPattern, Snapshot, TermId, Transaction};
use oxrdf::{Quad, Term, TermRef};

pub trait ReadView {
    /// Quads matching `pattern`. The iterator borrows the view, not the pattern.
    fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, Self>;

    /// Non-empty named graphs.
    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, Self>;

    fn contains_named_graph(&self, graph: TermId) -> bool;

    /// Id of `term` if this view knows it. Must return the same id for the same term for
    /// the lifetime of the view; the evaluator's term equality depends on it.
    fn lookup(&self, term: TermRef<'_>) -> Option<TermId>;

    fn decode(&self, id: TermId) -> Option<Term>;

    /// Decodes a quad read from this view; `None` only for ids the view doesn't know.
    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad>;
}

impl ReadView for Snapshot {
    fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        Snapshot::quads_for_pattern(self, pattern)
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
}

impl<'e> ReadView for Transaction<'e> {
    fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        Transaction::quads_for_pattern(self, pattern)
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
}
