//! The dataset of a query (`FROM`, `FROM NAMED`, the protocol's parameters), resolved
//! against a view ([`ResolvedDataset`]). The default graph of several `FROM` graphs is their
//! RDF merge (SPARQL 1.1 §13.2): a statement two of them hold counts once.

use nrese_engine::TermId;
use nrese_rdf::{GraphName, NamedOrBlankNodeRef};
use nrese_sparql_syntax::algebra::QueryDataset;

use crate::results::QueryDatasetSpecification;
use crate::view::ReadView;

/// The default graph of a query's dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DefaultGraph {
    /// The store's default graph.
    Store,
    /// One named graph.
    Graph(TermId),
    /// The RDF merge of every graph (`None`) or of the listed ones: a statement counts
    /// once, however many of them hold it.
    Merge(Option<Vec<TermId>>),
    /// No graph the store holds.
    Empty,
}

/// What a query reads (SPARQL 1.1 §13.2): its default graph, and the graphs `GRAPH` may
/// name (`None`: every named graph of the store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedDataset {
    pub(crate) default: DefaultGraph,
    pub(crate) named: Option<Vec<TermId>>,
}

impl ResolvedDataset {
    /// The store's own dataset: its default graph, or the merge of all graphs if the
    /// store is configured so, and every named graph.
    pub(crate) fn of_store(union_default_graph: bool) -> Self {
        Self {
            default: if union_default_graph {
                DefaultGraph::Merge(None)
            } else {
                DefaultGraph::Store
            },
            named: None,
        }
    }

    /// The protocol's dataset if there is one, else the query's own (`FROM`, `FROM
    /// NAMED`; for an update `USING`, `WITH`), else the store's.
    pub(crate) fn resolve<V: ReadView>(
        view: &V,
        union_default_graph: bool,
        protocol: Option<&QueryDatasetSpecification>,
        own: Option<&QueryDataset>,
    ) -> Self {
        let specification = protocol
            .cloned()
            .or_else(|| own.cloned().map(Into::into))
            .filter(|specification| !specification.is_default_dataset());
        let Some(specification) = specification else {
            return Self::of_store(union_default_graph);
        };
        // A graph the store doesn't hold contributes nothing.
        let graph = |name: NamedOrBlankNodeRef<'_>| {
            view.lookup(name.into())
                .filter(|&id| view.contains_named_graph(id))
        };
        let default = match specification.default_graph_graphs() {
            None => DefaultGraph::Merge(None),
            Some(graphs) => {
                let mut ids: Vec<TermId> = graphs
                    .iter()
                    .filter_map(|name| match name {
                        GraphName::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
                        GraphName::NamedNode(n) => graph(n.as_ref().into()),
                        GraphName::BlankNode(b) => graph(b.as_ref().into()),
                    })
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                match ids[..] {
                    [] => DefaultGraph::Empty,
                    [id] if id == TermId::DEFAULT_GRAPH => DefaultGraph::Store,
                    [id] => DefaultGraph::Graph(id),
                    _ => DefaultGraph::Merge(Some(ids)),
                }
            }
        };
        let named = specification.available_named_graphs().map(|graphs| {
            graphs
                .iter()
                .filter_map(|name| graph(name.as_ref()))
                .collect()
        });
        Self { default, named }
    }

    /// Whether the dataset has the named graph `graph`, which the store holds.
    #[allow(dead_code)]
    pub(crate) fn names(&self, graph: TermId) -> bool {
        self.named
            .as_ref()
            .is_none_or(|named| named.contains(&graph))
    }
}
