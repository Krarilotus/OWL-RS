//! [`spareval::QueryableDataset`] over any [`ReadView`].
//!
//! Term identity: the evaluator compares [`EvalTerm`]s with `==` and hashes them, and treats
//! that as RDF term equality (`sameTerm`). That is sound only if a term that the view knows
//! is *always* represented as [`EvalTerm::Stored`]. [`EngineDataset::internalize_term`]
//! enforces this by looking every incoming term up first; only terms unknown to the view (so
//! matching no stored quad) stay [`EvalTerm::Value`]. Inline values (canonical integers,
//! booleans) have ids without a dictionary entry, so computed values join with stored ones.

use nrese_engine::{EncodedQuad, EngineError, GraphSelector, QuadPattern, TermId};
use oxrdf::Term;
use spareval::{InternalQuad, QueryableDataset};

use crate::view::ReadView;

/// A term during evaluation: a stored id, or a value that isn't in the view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EvalTerm {
    Stored(TermId),
    Value(Term),
}

/// Adapter handed to spareval. Cheap to copy: it is just a reference to the view.
pub struct EngineDataset<'a, V> {
    view: &'a V,
}

impl<'a, V> EngineDataset<'a, V> {
    pub fn new(view: &'a V) -> Self {
        Self { view }
    }
}

impl<V> Clone for EngineDataset<'_, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> Copy for EngineDataset<'_, V> {}

/// `Ok(None)` for an unbound position, `Ok(Some(id))` for a stored term, and `Err(())` for a
/// term the view doesn't know, which can't match anything.
fn bound(term: Option<&EvalTerm>) -> Result<Option<TermId>, ()> {
    match term {
        None => Ok(None),
        Some(EvalTerm::Stored(id)) => Ok(Some(*id)),
        Some(EvalTerm::Value(_)) => Err(()),
    }
}

/// spareval's graph argument: `None` = any named graph, `Some(None)` = default graph.
fn graph_selector(graph: Option<Option<&EvalTerm>>) -> Result<GraphSelector, ()> {
    Ok(match graph {
        None => GraphSelector::AnyNamed,
        Some(None) => GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        Some(Some(EvalTerm::Stored(graph))) => GraphSelector::Exact(*graph),
        Some(Some(EvalTerm::Value(_))) => return Err(()),
    })
}

fn to_internal(quad: EncodedQuad) -> InternalQuad<EvalTerm> {
    InternalQuad {
        subject: EvalTerm::Stored(quad.subject),
        predicate: EvalTerm::Stored(quad.predicate),
        object: EvalTerm::Stored(quad.object),
        graph_name: (!quad.graph.is_default_graph()).then_some(EvalTerm::Stored(quad.graph)),
    }
}

impl<'a, V: ReadView> QueryableDataset<'a> for EngineDataset<'a, V> {
    type InternalTerm = EvalTerm;
    type Error = EngineError;

    fn internal_quads_for_pattern(
        &self,
        subject: Option<&EvalTerm>,
        predicate: Option<&EvalTerm>,
        object: Option<&EvalTerm>,
        graph_name: Option<Option<&EvalTerm>>,
    ) -> impl Iterator<Item = Result<InternalQuad<EvalTerm>, EngineError>> + use<'a, V> {
        let pattern = (|| {
            Ok::<_, ()>(QuadPattern {
                subject: bound(subject)?,
                predicate: bound(predicate)?,
                object: bound(object)?,
                graph: graph_selector(graph_name)?,
            })
        })();
        let view: &'a V = self.view;
        pattern
            .ok()
            .into_iter()
            .flat_map(move |pattern| view.quads_for_pattern(&pattern))
            .map(|quad| Ok(to_internal(quad)))
    }

    fn internal_named_graphs(
        &self,
    ) -> impl Iterator<Item = Result<EvalTerm, EngineError>> + use<'a, V> {
        let view: &'a V = self.view;
        view.named_graphs().map(|graph| Ok(EvalTerm::Stored(graph)))
    }

    fn contains_internal_graph_name(&self, graph_name: &EvalTerm) -> Result<bool, EngineError> {
        Ok(match graph_name {
            EvalTerm::Stored(graph) => self.view.contains_named_graph(*graph),
            EvalTerm::Value(_) => false,
        })
    }

    fn internalize_term(&self, term: Term) -> Result<EvalTerm, EngineError> {
        Ok(match self.view.lookup(term.as_ref()) {
            Some(id) => EvalTerm::Stored(id),
            None => EvalTerm::Value(term),
        })
    }

    fn externalize_term(&self, term: EvalTerm) -> Result<Term, EngineError> {
        match term {
            EvalTerm::Stored(id) => self
                .view
                .decode(id)
                .ok_or(EngineError::UnknownTerm(id.raw())),
            EvalTerm::Value(term) => Ok(term),
        }
    }
}
