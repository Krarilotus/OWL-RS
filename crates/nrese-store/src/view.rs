//! Reading engine views as RDF: the store's one decoding path and graph-target mapping.

use nrese_engine::{EngineError, QuadPattern, ReadModel, TermId};
use nrese_sparql::ReadView;
use oxrdf::{GraphName, NamedNode, Quad};

use crate::error::{StoreError, StoreResult};
use crate::graph_store::GraphTarget;

/// Decoded quads matching `pattern` in `model`. An undecodable id means storage corruption.
pub(crate) fn decoded_quads<'a, V: ReadView>(
    view: &'a V,
    model: ReadModel,
    pattern: &QuadPattern,
) -> impl Iterator<Item = StoreResult<Quad>> + use<'a, V> {
    view.quads_for_pattern_in(model, pattern).map(move |quad| {
        view.decode_quad(quad).ok_or_else(|| {
            StoreError::Engine(EngineError::Corruption(format!(
                "undecodable quad {quad:?}"
            )))
        })
    })
}

impl GraphTarget {
    /// The RDF graph name this target addresses.
    pub(crate) fn graph_name(&self) -> StoreResult<GraphName> {
        match self {
            Self::DefaultGraph => Ok(GraphName::DefaultGraph),
            Self::NamedGraph(iri) => NamedNode::new(iri.as_str())
                .map(GraphName::from)
                .map_err(|_| StoreError::InvalidGraphIri(iri.clone())),
        }
    }

    /// The pattern selecting this graph's quads in `view`, or `None` for a named graph the
    /// view doesn't know (which therefore holds no quads).
    pub(crate) fn pattern_in(&self, view: &impl ReadView) -> StoreResult<Option<QuadPattern>> {
        let graph = match self.graph_name()? {
            GraphName::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
            GraphName::NamedNode(node) => view.lookup(node.as_ref().into()),
            GraphName::BlankNode(node) => view.lookup(node.as_ref().into()),
        };
        Ok(graph.map(QuadPattern::in_graph))
    }
}
