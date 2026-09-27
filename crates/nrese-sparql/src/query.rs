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
    /// Bytes of intermediate results the native executor may hold; `None` is unlimited.
    pub memory_limit: Option<usize>,
    /// Run on spareval even if the native executor supports the query (differential
    /// testing, and a production escape hatch).
    pub force_spareval: bool,
}

/// Evaluates `query` against `view`. Queries the native executor supports run there
/// (`native`); everything else, and every query over a transaction, runs on spareval.
/// Results stream lazily and borrow the view.
pub fn evaluate_query<'a, V: ReadView>(
    view: &'a V,
    query: &Query,
    options: &QueryOptions,
) -> Result<QueryResults<'a>, QueryEvaluationError> {
    if !options.force_spareval
        && let Some(snapshot) = view.snapshot()
        && let Some(results) = crate::native::evaluate(snapshot, query, options)
    {
        return results;
    }
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

/// True if `query` would run on the native executor over a snapshot with default options.
/// For coverage reports and tests; the decision itself is made per call.
pub fn runs_natively(query: &Query) -> bool {
    crate::native::query_supported(query)
}
