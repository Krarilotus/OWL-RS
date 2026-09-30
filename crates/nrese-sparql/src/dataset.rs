//! [`spareval::QueryableDataset`] over any [`ReadView`].
//!
//! Term identity: the evaluator compares [`EvalTerm`]s with `==` and hashes them, and treats
//! that as RDF term equality (`sameTerm`). That is sound only if a term that the view knows
//! is *always* represented as [`EvalTerm::Stored`]. [`EngineDataset::internalize_term`]
//! enforces this by looking every incoming term up first; only terms unknown to the view (so
//! matching no stored quad) stay [`EvalTerm::Value`]. Inline values (canonical integers,
//! booleans) have ids without a dictionary entry, so computed values join with stored ones.
//!
//! The dataset of a query (`FROM`, `FROM NAMED`, the protocol's parameters) is resolved
//! here, once for both executors ([`ResolvedDataset`]), and the adapter presents it to
//! spareval as an ordinary store: spareval's own handling repeats a statement that two
//! `FROM` graphs hold, where SPARQL 1.1 §13.2 makes the default graph their RDF merge.

use nrese_engine::{EncodedQuad, EngineError, GraphSelector, QuadPattern, ReadModel, TermId};
use oxrdf::{GraphName, NamedOrBlankNodeRef, Term};
use spareval::{InternalQuad, QueryDatasetSpecification, QueryableDataset};
use spargebra::algebra::QueryDataset;

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
    fn of_store(union_default_graph: bool) -> Self {
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
    fn names(&self, graph: TermId) -> bool {
        self.named
            .as_ref()
            .is_none_or(|named| named.contains(&graph))
    }
}

/// A term during evaluation: a stored id, or a value that isn't in the view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EvalTerm {
    Stored(TermId),
    Value(Term),
}

/// Adapter handed to spareval: a reference to the view, the read model and the dataset.
/// spareval sees an ordinary store whose default graph and named graphs are the
/// dataset's, and must be left with its default dataset specification.
pub struct EngineDataset<'a, V> {
    view: &'a V,
    model: ReadModel,
    dataset: ResolvedDataset,
}

impl<'a, V> EngineDataset<'a, V> {
    /// Reads asserted and inferred statements.
    pub fn new(view: &'a V) -> Self {
        Self::with_model(view, ReadModel::Materialised)
    }

    pub fn with_model(view: &'a V, model: ReadModel) -> Self {
        Self {
            view,
            model,
            dataset: ResolvedDataset::of_store(false),
        }
    }

    /// Whether the default graph is the merge of all graphs (when the query names no
    /// dataset of its own).
    #[must_use]
    pub fn union_default_graph(mut self, union: bool) -> Self {
        self.dataset = ResolvedDataset::of_store(union);
        self
    }
}

impl<V: ReadView> EngineDataset<'_, V> {
    /// Reads the dataset of a query or update: the protocol's if there is one, else its
    /// `own` clauses, else the store's (whose default graph is the merge of all graphs
    /// with `union_default_graph`).
    #[must_use]
    pub fn reading(
        mut self,
        union_default_graph: bool,
        protocol: Option<&QueryDatasetSpecification>,
        own: Option<&QueryDataset>,
    ) -> Self {
        self.dataset = ResolvedDataset::resolve(self.view, union_default_graph, protocol, own);
        self
    }
}

impl<V> Clone for EngineDataset<'_, V> {
    fn clone(&self) -> Self {
        Self {
            view: self.view,
            model: self.model,
            dataset: self.dataset.clone(),
        }
    }
}

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

/// Whether `quad` is the copy of its statement in the first of `graphs` (all graphs if
/// `None`) that holds it (graphs in id order, the default graph first). Keeping only
/// that copy merges the graphs whatever order a scan returns them in.
fn is_first_copy<V: ReadView>(
    view: &V,
    model: ReadModel,
    quad: &EncodedQuad,
    graphs: Option<&[TermId]>,
) -> bool {
    if quad.graph.is_default_graph() {
        return true;
    }
    let copies = QuadPattern {
        subject: Some(quad.subject),
        predicate: Some(quad.predicate),
        object: Some(quad.object),
        graph: GraphSelector::Any,
    };
    view.quads_for_pattern_in(model, &copies)
        .filter(|copy| graphs.is_none_or(|graphs| graphs.contains(&copy.graph)))
        .all(|copy| copy.graph >= quad.graph)
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
        // The dataset's default graph is presented as the default graph, whatever graphs
        // of the store it is made of; a merge keeps each statement once.
        let in_default = matches!(graph_name, Some(None));
        let merge: Option<Option<Vec<TermId>>> = match &self.dataset.default {
            DefaultGraph::Merge(graphs) if in_default => Some(graphs.clone()),
            _ => None,
        };
        let named = if in_default {
            None
        } else {
            self.dataset.named.clone()
        };
        let pattern = (|| {
            let graph = match (&self.dataset.default, graph_name) {
                (DefaultGraph::Empty, Some(None)) => return Err(()),
                (DefaultGraph::Graph(graph), Some(None)) => GraphSelector::Exact(*graph),
                (DefaultGraph::Merge(_), Some(None)) => GraphSelector::Any,
                (_, Some(Some(EvalTerm::Stored(graph)))) if !self.dataset.names(*graph) => {
                    return Err(());
                }
                _ => graph_selector(graph_name)?,
            };
            Ok::<_, ()>(QuadPattern {
                subject: bound(subject)?,
                predicate: bound(predicate)?,
                object: bound(object)?,
                graph,
            })
        })();
        let (view, model): (&'a V, ReadModel) = (self.view, self.model);
        pattern
            .ok()
            .into_iter()
            .flat_map(move |pattern| view.quads_for_pattern_in(model, &pattern))
            .filter(move |quad| match &merge {
                Some(graphs) => {
                    graphs.as_ref().is_none_or(|g| g.contains(&quad.graph))
                        && is_first_copy(view, model, quad, graphs.as_deref())
                }
                None => true,
            })
            .filter(move |quad| {
                named
                    .as_ref()
                    .is_none_or(|named| named.contains(&quad.graph))
            })
            .map(move |mut quad| {
                if in_default {
                    quad.graph = TermId::DEFAULT_GRAPH;
                }
                Ok(to_internal(quad))
            })
    }

    fn internal_named_graphs(
        &self,
    ) -> impl Iterator<Item = Result<EvalTerm, EngineError>> + use<'a, V> {
        let view: &'a V = self.view;
        let named = self.dataset.named.clone();
        view.named_graphs()
            .filter(move |graph| named.as_ref().is_none_or(|named| named.contains(graph)))
            .map(|graph| Ok(EvalTerm::Stored(graph)))
    }

    fn contains_internal_graph_name(&self, graph_name: &EvalTerm) -> Result<bool, EngineError> {
        Ok(match graph_name {
            EvalTerm::Stored(graph) => {
                self.dataset.names(*graph) && self.view.contains_named_graph(*graph)
            }
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
