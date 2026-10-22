//! One graph selection of a [`ReadView`], read the way SHACL reads it: as sets of nodes.
//!
//! A statement asserted in several graphs, or asserted and inferred, is one statement
//! here: every accessor returns a sorted set of ids.

use nrese_exec::{Budget, ChargedVec, Reservation, Stop, Ticker};
use nrese_sparql::QueryEvaluationError;

use nrese_engine::{EncodedQuad, GraphSelector, QuadPattern, ReadModel, TermId};
use nrese_rdf::{GraphName, NamedNodeRef, Term};
use nrese_sparql::{QueryDatasetSpecification, QueryOptions, ReadView};

/// Which statements a validation reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub graphs: GraphSelector,
    /// Graphs left out of `graphs`: shapes graphs, when every graph is validated.
    pub excluded: [Option<TermId>; 2],
    pub model: ReadModel,
}

impl Selection {
    /// `graphs`, asserted and inferred statements.
    pub const fn of(graphs: GraphSelector) -> Self {
        Self {
            graphs,
            excluded: [None; 2],
            model: ReadModel::Materialised,
        }
    }

    /// Only what was asserted: shapes graphs are read this way.
    pub const fn asserted(graphs: GraphSelector) -> Self {
        Self {
            graphs,
            excluded: [None; 2],
            model: ReadModel::Asserted,
        }
    }

    /// Leaves `graph` out. Two graphs can be left out (the stored shapes graph and the
    /// shapes of the request); a third replaces the second.
    pub const fn excluding(mut self, graph: TermId) -> Self {
        let slot = if self.excluded[0].is_none() { 0 } else { 1 };
        self.excluded[slot] = Some(graph);
        self
    }

    /// Sets `options` to read, as the query's default graph, the statements Core reads
    /// ([`GraphView`]): the selected graphs without the excluded ones, in the selection's read
    /// model. The named graphs stay the store's, so `GRAPH $shapesGraph` still reads the
    /// shapes; only the default graph, which SHACL-SPARQL's data graph is, leaves them out.
    /// Every selection sets the dataset, as a protocol request does: a constraint's own `FROM`
    /// and `FROM NAMED` are replaced whatever form the selection takes, so the answer depends
    /// only on the statements selected.
    pub(crate) fn query_dataset<V: ReadView>(
        &self,
        view: &V,
        options: &mut QueryOptions,
        held: &mut Reservation<&Budget>,
        ticker: &mut Ticker,
    ) -> Result<(), QueryEvaluationError> {
        ticker.check().map_err(stopped)?;
        options.read_model = self.model;
        let (ids, excluded) = match self.graphs {
            GraphSelector::Exact(graph) if self.excluded.contains(&Some(graph)) => {
                ([None; 3], false)
            }
            GraphSelector::Exact(graph) => ([Some(graph), None, None], false),
            GraphSelector::Any => ([self.excluded[0], self.excluded[1], None], true),
            GraphSelector::AnyNamed => (
                [
                    self.excluded[0],
                    self.excluded[1],
                    Some(TermId::DEFAULT_GRAPH),
                ],
                true,
            ),
        };
        let count = ids.iter().flatten().count();
        held.charge(count * size_of::<GraphName>())?;
        let mut names = Vec::with_capacity(count);
        for id in ids.into_iter().flatten() {
            ticker.tick(1).map_err(stopped)?;
            let name = if id.is_default_graph() {
                GraphName::DefaultGraph
            } else {
                match view.decode_within(id, held)? {
                    Some(Term::NamedNode(n)) => GraphName::NamedNode(n),
                    Some(Term::BlankNode(n)) => GraphName::BlankNode(n),
                    _ => {
                        return Err(QueryEvaluationError::Unexpected(format!(
                            "graph #{} isn't a graph name",
                            id.raw()
                        )));
                    }
                }
            };
            names.push(name);
        }
        let mut dataset = QueryDatasetSpecification::new();
        if excluded {
            dataset.set_default_graph_excluding(names);
        } else {
            dataset.set_default_graph(names);
        }
        options.dataset = Some(dataset);
        ticker.check().map_err(stopped)?;
        Ok(())
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

    fn scan_unfiltered(
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
        self.view
            .quads_for_pattern_in(self.selection.model, &pattern)
    }

    pub(crate) fn scan(
        &self,
        subject: Option<TermId>,
        predicate: Option<TermId>,
        object: Option<TermId>,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, V> {
        let excluded = self.selection.excluded;
        self.scan_unfiltered(subject, predicate, object)
            .filter(move |quad| !excluded.contains(&Some(quad.graph)))
    }

    // Compilation's existing accessors below keep their contract. Validation collects
    // into admitted storage and counts every candidate before SHACL's exclusion filter.
    fn collect_within<'b, T: Ord + Copy>(
        &self,
        subject: Option<TermId>,
        predicate: Option<TermId>,
        object: Option<TermId>,
        value: impl Fn(EncodedQuad) -> T,
        budget: &'b Budget,
        ticker: &mut Ticker,
    ) -> Result<ChargedVec<T, &'b Budget>, QueryEvaluationError> {
        ticker.check().map_err(stopped)?;
        let mut out = ChargedVec::new(budget);
        for quad in self.scan_unfiltered(subject, predicate, object) {
            ticker.tick(1).map_err(stopped)?;
            if !self.selection.excluded.contains(&Some(quad.graph)) {
                out.push(value(quad))?;
            }
        }
        let kept = sort_set(&mut out, ticker)?;
        out.truncate(kept);
        Ok(out)
    }

    pub(crate) fn objects_within<'b>(
        &self,
        subject: TermId,
        predicate: TermId,
        budget: &'b Budget,
        ticker: &mut Ticker,
    ) -> Result<ChargedVec<TermId, &'b Budget>, QueryEvaluationError> {
        self.collect_within(
            Some(subject),
            Some(predicate),
            None,
            |q| q.object,
            budget,
            ticker,
        )
    }

    pub(crate) fn subjects_within<'b>(
        &self,
        predicate: TermId,
        object: TermId,
        budget: &'b Budget,
        ticker: &mut Ticker,
    ) -> Result<ChargedVec<TermId, &'b Budget>, QueryEvaluationError> {
        self.collect_within(
            None,
            Some(predicate),
            Some(object),
            |q| q.subject,
            budget,
            ticker,
        )
    }

    pub(crate) fn subjects_of_within<'b>(
        &self,
        predicate: TermId,
        budget: &'b Budget,
        ticker: &mut Ticker,
    ) -> Result<ChargedVec<TermId, &'b Budget>, QueryEvaluationError> {
        self.collect_within(None, Some(predicate), None, |q| q.subject, budget, ticker)
    }

    pub(crate) fn objects_of_within<'b>(
        &self,
        predicate: TermId,
        budget: &'b Budget,
        ticker: &mut Ticker,
    ) -> Result<ChargedVec<TermId, &'b Budget>, QueryEvaluationError> {
        self.collect_within(None, Some(predicate), None, |q| q.object, budget, ticker)
    }

    pub(crate) fn edges_within<'b>(
        &self,
        subject: TermId,
        budget: &'b Budget,
        ticker: &mut Ticker,
    ) -> Result<ChargedVec<(TermId, TermId), &'b Budget>, QueryEvaluationError> {
        self.collect_within(
            Some(subject),
            None,
            None,
            |q| (q.predicate, q.object),
            budget,
            ticker,
        )
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

    pub(crate) fn lookup(&self, term: &Term) -> Option<TermId> {
        self.view.lookup(term.as_ref())
    }

    /// The view and the statements it reads: what SHACL-SPARQL queries run over.
    pub(crate) fn parts(&self) -> (&'a V, Selection) {
        (self.view, self.selection)
    }
}

pub(crate) fn stopped(stop: Stop) -> QueryEvaluationError {
    match stop {
        Stop::Cancelled => QueryEvaluationError::Cancelled,
        Stop::Memory(error) => QueryEvaluationError::MemoryLimit(error),
        error => QueryEvaluationError::Unexpected(error.to_string()),
    }
}

/// Sort/dedup in the admitted allocation. No replacement Vec or heap during the sort.
pub(crate) fn sort_set<T: Ord + Copy>(
    items: &mut [T],
    ticker: &mut Ticker,
) -> Result<usize, QueryEvaluationError> {
    ticker.check().map_err(stopped)?;
    items.sort_unstable();
    ticker.check().map_err(stopped)?;
    let mut kept = 0;
    for read in 0..items.len() {
        ticker.tick(1).map_err(stopped)?;
        if kept == 0 || items[read] != items[kept - 1] {
            items[kept] = items[read];
            kept += 1;
        }
    }
    Ok(kept)
}

pub(crate) fn contains_within<T: PartialEq>(
    items: &[T],
    item: &T,
    ticker: &mut Ticker,
) -> Result<bool, QueryEvaluationError> {
    for candidate in items {
        ticker.tick(1).map_err(stopped)?;
        if candidate == item {
            return Ok(true);
        }
    }
    Ok(false)
}
