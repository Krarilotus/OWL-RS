//! The dataset of a query (`FROM`, `FROM NAMED`, the protocol's parameters), resolved
//! against a view ([`ResolvedDataset`]). The default graph of several `FROM` graphs is their
//! merge: a statement two of them hold counts once. Blank nodes keep their identity across
//! graphs, as in Jena, GraphDB and Oxigraph: a deliberate deviation from the RDF merge of
//! SPARQL 1.1 §13.2, which keeps them apart (docs/ops/http-api.md, the owner's decision).

use nrese_engine::TermId;
use nrese_rdf::{GraphName, NamedOrBlankNodeRef, Term};
use nrese_sparql_syntax::algebra::QueryDataset;

use crate::results::QueryDatasetSpecification;
use crate::view::ReadView;

/// The graphs a user may read (graph-level access control): named graphs by IRI or IRI
/// prefix, and whether the store's default graph. A query of such a user evaluates over
/// its dataset restricted to them: the others are absent, not forbidden (`GRAPH ?g` never
/// binds them, counts don't see them). Graphs named by blank nodes are never readable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphAccess {
    /// Graph IRIs readable as they are.
    pub graphs: Vec<String>,
    /// IRI prefixes: every graph whose IRI starts with one is readable.
    pub prefixes: Vec<String>,
    /// The store's default graph.
    pub default_graph: bool,
    /// The inferred statements (they live in the default graph; whether a user who may not
    /// read every graph sees them is the configuration's choice).
    pub inferred: bool,
    /// With `inferred`: only the inferred statements one of whose derivations uses
    /// readable graphs alone (their support graph sets); the store hides the others.
    pub inferred_by_support: bool,
    /// Graph IRIs excluded whatever `graphs` and `prefixes` say (an explicit deny).
    pub excluded: Vec<String>,
    /// IRI prefixes excluded the same way.
    pub excluded_prefixes: Vec<String>,
    /// Whether the requester may call other endpoints with `SERVICE` (a privilege of its
    /// own: it makes the server fetch URLs). An unrestricted requester may.
    pub service: bool,
}

impl GraphAccess {
    /// Whether the named graph `iri` is readable.
    pub fn allows(&self, iri: &str) -> bool {
        let matches = |graphs: &[String], prefixes: &[String]| {
            graphs.iter().any(|g| g == iri) || prefixes.iter().any(|p| iri.starts_with(p))
        };
        matches(&self.graphs, &self.prefixes) && !matches(&self.excluded, &self.excluded_prefixes)
    }

    /// Whether `graph` is in the set: the default graph, or a named graph by IRI (graphs
    /// named by blank nodes never).
    pub fn allows_graph(&self, graph: &GraphName) -> bool {
        match graph {
            GraphName::DefaultGraph => self.default_graph,
            GraphName::NamedNode(node) => self.allows(node.as_str()),
            GraphName::BlankNode(_) => false,
        }
    }

    /// Whether the graph `id` of `view` is in the set.
    pub fn allows_id<V: ReadView>(&self, view: &V, id: TermId) -> bool {
        if id == TermId::DEFAULT_GRAPH {
            return self.default_graph;
        }
        matches!(view.decode(id), Some(Term::NamedNode(node)) if self.allows(node.as_str()))
    }
}

/// The default graph merged from `ids` (sorted, each once): nothing, the store's default
/// graph, one graph, or their merge.
fn only(ids: Vec<TermId>) -> DefaultGraph {
    match ids[..] {
        [] => DefaultGraph::Empty,
        [id] if id == TermId::DEFAULT_GRAPH => DefaultGraph::Store,
        [id] => DefaultGraph::Graph(id),
        _ => DefaultGraph::Merge(Some(GraphSet::Only(ids))),
    }
}

/// The default graph of a query's dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DefaultGraph {
    /// The store's default graph.
    Store,
    /// One named graph.
    Graph(TermId),
    /// The merge of every graph (`None`) or of some ([`GraphSet`]): a statement counts once,
    /// however many of them hold it.
    Merge(Option<GraphSet>),
    /// No graph the store holds.
    Empty,
}

/// The graphs of a merged default graph that isn't every graph: the listed ones (several
/// `FROM`), or every graph but the listed ones (SHACL's data graph, without the shapes).
/// Each list is sorted and holds a graph once.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum GraphSet {
    Only(Vec<TermId>),
    AllBut(Vec<TermId>),
}

/// No graph at all: the merge of `GRAPH <g>` for a graph the store doesn't know.
pub(crate) static NO_GRAPHS: GraphSet = GraphSet::Only(Vec::new());

impl GraphSet {
    pub(crate) fn contains(&self, graph: &TermId) -> bool {
        match self {
            Self::Only(graphs) => graphs.contains(graph),
            Self::AllBut(graphs) => !graphs.contains(graph),
        }
    }
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
        let ids_of = |graphs: &[GraphName]| -> Vec<TermId> {
            let mut ids: Vec<TermId> = graphs
                .iter()
                .filter_map(|name| match name {
                    GraphName::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
                    // Jena's name for the default graph (`compat`).
                    GraphName::NamedNode(n) if crate::compat::names_default_graph(n.as_str()) => {
                        Some(TermId::DEFAULT_GRAPH)
                    }
                    GraphName::NamedNode(n) => graph(n.as_ref().into()),
                    GraphName::BlankNode(b) => graph(b.as_ref().into()),
                })
                .collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let default = match specification.default_graph_graphs() {
            None => match ids_of(specification.excluded_default_graphs().unwrap_or_default()) {
                left_out if left_out.is_empty() => DefaultGraph::Merge(None),
                left_out => DefaultGraph::Merge(Some(GraphSet::AllBut(left_out))),
            },
            Some(graphs) => only(ids_of(graphs)),
        };
        let named = specification.available_named_graphs().map(|graphs| {
            graphs
                .iter()
                .filter_map(|name| graph(name.as_ref()))
                .collect()
        });
        Self { default, named }
    }

    /// This dataset restricted to what `access` lets its user read: the default graph
    /// merged from the readable graphs among its own, the readable named graphs.
    pub(crate) fn restricted<V: ReadView>(self, view: &V, access: &GraphAccess) -> Self {
        let readable_named = || -> Vec<TermId> {
            view.named_graphs()
                .filter(|&g| access.allows_id(view, g))
                .collect()
        };
        let merge = |mut ids: Vec<TermId>| {
            ids.sort_unstable();
            ids.dedup();
            only(ids)
        };
        let default = match self.default {
            DefaultGraph::Store if access.default_graph => DefaultGraph::Store,
            DefaultGraph::Store | DefaultGraph::Empty => DefaultGraph::Empty,
            DefaultGraph::Graph(g) if access.allows_id(view, g) => DefaultGraph::Graph(g),
            DefaultGraph::Graph(_) => DefaultGraph::Empty,
            DefaultGraph::Merge(None) => {
                let mut ids = readable_named();
                if access.default_graph {
                    ids.push(TermId::DEFAULT_GRAPH);
                }
                merge(ids)
            }
            DefaultGraph::Merge(Some(GraphSet::Only(ids))) => merge(
                ids.into_iter()
                    .filter(|&g| access.allows_id(view, g))
                    .collect(),
            ),
            DefaultGraph::Merge(Some(GraphSet::AllBut(left_out))) => {
                let mut ids = readable_named();
                if access.default_graph {
                    ids.push(TermId::DEFAULT_GRAPH);
                }
                ids.retain(|g| left_out.binary_search(g).is_err());
                merge(ids)
            }
        };
        let named = Some(match self.named {
            None => readable_named(),
            Some(graphs) => graphs
                .into_iter()
                .filter(|&g| access.allows_id(view, g))
                .collect(),
        });
        Self { default, named }
    }
}
