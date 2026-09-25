//! SPARQL query execution over a [`ReadView`].

use spareval::{
    CancellationToken, QueryDatasetSpecification, QueryEvaluationError, QueryEvaluator,
    QueryResults,
};
use spargebra::Query;

use crate::dataset::EngineDataset;
use crate::view::ReadView;

/// Per-request query options (the transport layer fills these from the protocol request).
#[derive(Clone, Default)]
pub struct QueryOptions {
    /// Protocol `default-graph-uri` / `named-graph-uri`. When set, it replaces the query's
    /// own `FROM` / `FROM NAMED` clauses, as the SPARQL 1.1 Protocol requires.
    pub dataset: Option<QueryDatasetSpecification>,
    /// Checked by the evaluator while it runs; the transport layer cancels it at the
    /// request deadline.
    pub cancellation: Option<CancellationToken>,
}

/// Evaluates `query` against `view`. Results stream lazily and borrow the view.
pub fn evaluate_query<'a, V: ReadView>(
    view: &'a V,
    query: &Query,
    options: &QueryOptions,
) -> Result<QueryResults<'a>, QueryEvaluationError> {
    let evaluator = evaluator(options.cancellation.as_ref());
    let mut prepared = evaluator.prepare(query);
    if let Some(dataset) = &options.dataset {
        *prepared.dataset_mut() = dataset.clone();
    }
    prepared.execute(EngineDataset::new(view))
}

pub(crate) fn evaluator(cancellation: Option<&CancellationToken>) -> QueryEvaluator {
    let evaluator = QueryEvaluator::new();
    match cancellation {
        Some(token) => evaluator.with_cancellation_token(token.clone()),
        None => evaluator,
    }
}
