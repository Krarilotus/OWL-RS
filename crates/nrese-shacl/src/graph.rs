//! One graph selection of a [`ReadView`], read the way SHACL reads it: as sets of nodes.
//!
//! A statement asserted in several graphs, or asserted and inferred, is one statement
//! here: every accessor returns a sorted set of ids.

use nrese_engine::{EncodedQuad, GraphSelector, QuadPattern, ReadModel, TermId};
use nrese_sparql::ReadView;
use oxrdf::{NamedNodeRef, Term};

/// Which statements a validation reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub graphs: GraphSelector,
    /// A graph left out of `graphs` (the shapes graph, when every graph is validated).
    pub excluded: Option<TermId>,
    pub model: ReadModel,
}

impl Selection {
    /// `graphs`, asserted and inferred statements.
    pub const fn of(graphs: GraphSelector) -> Self {
        Self {
            graphs,
            excluded: None,
            model: ReadModel::Materialised,
        }
    }

    /// Only what was asserted: shapes graphs are read this way.
    pub const fn asserted(graphs: GraphSelector) -> Self {
        Self {
            graphs,
            excluded: None,
            model: ReadModel::Asserted,
        }
    }

    pub const fn excluding(mut self, graph: TermId) -> Self {
        self.excluded = Some(graph);
        self
    }
}

pub(crate) struct GraphView<'a, V: ReadView> {
    view: &'a V,
    selection: Selection,
}

impl<V: ReadView> Clone for GraphView<'_, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V: ReadView> Copy for GraphView<'_, V> {}

fn sorted_set(mut ids: Vec<TermId>) -> Vec<TermId> {
    ids.sort_unstable();
    ids.dedup();
    ids
}

impl<'a, V: ReadView> GraphView<'a, V> {
    pub(crate) fn new(view: &'a V, selection: Selection) -> Self {
        Self { view, selection }
    }

    fn scan(
        &self,
        subject: Option<TermId>,
        predicate: Option<TermId>,
        object: Option<TermId>,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, V> {
        let pattern = QuadPattern {
            subject,
            predicate,
            object,
            graph: self.selection.graphs,
        };
        let excluded = self.selection.excluded;
        self.view
            .quads_for_pattern_in(self.selection.model, &pattern)
            .filter(move |quad| Some(quad.graph) != excluded)
    }

    /// The objects of `(subject predicate ?)`.
    pub(crate) fn objects(&self, subject: TermId, predicate: TermId) -> Vec<TermId> {
        sorted_set(
            self.scan(Some(subject), Some(predicate), None)
                .map(|quad| quad.object)
                .collect(),
        )
    }

    /// The subjects of `(? predicate object)`.
    pub(crate) fn subjects(&self, predicate: TermId, object: TermId) -> Vec<TermId> {
        sorted_set(
            self.scan(None, Some(predicate), Some(object))
                .map(|quad| quad.subject)
                .collect(),
        )
    }

    /// Every subject of `predicate`.
    pub(crate) fn subjects_of(&self, predicate: TermId) -> Vec<TermId> {
        sorted_set(
            self.scan(None, Some(predicate), None)
                .map(|quad| quad.subject)
                .collect(),
        )
    }

    /// Every object of `predicate`.
    pub(crate) fn objects_of(&self, predicate: TermId) -> Vec<TermId> {
        sorted_set(
            self.scan(None, Some(predicate), None)
                .map(|quad| quad.object)
                .collect(),
        )
    }

    /// The `(predicate, object)` pairs of `subject`.
    pub(crate) fn edges(&self, subject: TermId) -> Vec<(TermId, TermId)> {
        let mut edges: Vec<(TermId, TermId)> = self
            .scan(Some(subject), None, None)
            .map(|quad| (quad.predicate, quad.object))
            .collect();
        edges.sort_unstable();
        edges.dedup();
        edges
    }

    pub(crate) fn contains(&self, subject: TermId, predicate: TermId, object: TermId) -> bool {
        self.scan(Some(subject), Some(predicate), Some(object))
            .next()
            .is_some()
    }

    /// The id of `iri`, if any statement anywhere uses it.
    pub(crate) fn iri(&self, iri: &str) -> Option<TermId> {
        self.view.lookup(NamedNodeRef::new_unchecked(iri).into())
    }

    pub(crate) fn decode(&self, id: TermId) -> Option<Term> {
        self.view.decode(id)
    }
}
