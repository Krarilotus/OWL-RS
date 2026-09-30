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
    /// The server's budget for all running queries together, which this query's memory
    /// counts against as well.
    pub shared_memory: Option<std::sync::Arc<nrese_exec::SharedBudget>>,
    /// Run on spareval even if the native executor supports the query (differential
    /// testing, and a production escape hatch).
    pub force_spareval: bool,
    /// Which statements the query reads: asserted and inferred (the default), or one
    /// stack (GraphDB's `infer=false`, `FROM onto:explicit` / `onto:implicit`).
    pub read_model: nrese_engine::ReadModel,
    /// The default graph of a query without a dataset of its own is the merge of all
    /// graphs (as in GraphDB, RDF4J stores and Blazegraph), not only the default graph. A
    /// statement counts once, however many graphs hold it.
    pub union_default_graph: bool,
    /// Evaluate the operators where the query puts them: no filter pushdown, no set
    /// evaluation for DISTINCT and duplicate-insensitive aggregates, paths computed in
    /// full before they are joined. The results are the same; the differential tests
    /// compare the two, and it is an escape hatch if a rewrite is ever suspected.
    pub as_written: bool,
    /// Who answers `SERVICE` calls ([`crate::service`]); `None`: federation is off.
    pub services: Option<crate::Services>,
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
    // The adapter presents the query's dataset as an ordinary store.
    *prepared.dataset_mut() = QueryDatasetSpecification::new();
    prepared.execute(EngineDataset::with_model(view, options.read_model).reading(
        options.union_default_graph,
        options.dataset.as_ref(),
        query.dataset(),
    ))
}

/// A failure while writing results directly ([`write_results`]).
#[derive(Debug, thiserror::Error)]
pub enum WriteResultsError {
    #[error(transparent)]
    Evaluation(#[from] QueryEvaluationError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Writes the results of `query` as SPARQL 1.1 Query Results JSON, TSV or CSV to `out`,
/// directly from the native executor's id table: byte for byte what serialising
/// [`evaluate_query`]'s results gives, without building terms and solutions per row.
/// `None` if the query doesn't run natively here (a transaction view, CONSTRUCT, ASK in
/// TSV/CSV, `force_spareval`, an unsupported operator): then serialise
/// [`evaluate_query`]'s results. On error the output is partial.
pub fn write_results<V: ReadView>(
    view: &V,
    query: &Query,
    options: &QueryOptions,
    format: crate::ResultsFormat,
    out: &mut dyn std::io::Write,
) -> Option<Result<(), WriteResultsError>> {
    if options.force_spareval {
        return None;
    }
    crate::native::write_results(view.snapshot()?, query, options, format, out)
}

/// One operator of a query run, as [`explain_query`] reports it (EXPLAIN ANALYZE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    /// Nesting depth: a step's inputs follow it one level deeper.
    pub depth: usize,
    /// `bgp`, `scan`, `index join`, `join`, `wcoj`, `filter`, `optional`, `group`, ...
    pub operator: String,
    /// The triple pattern, expression or variables the operator works on.
    pub detail: String,
    /// The planner's estimate of the rows after this step (BGP joins only).
    pub estimated_rows: Option<u64>,
    /// Rows the operator produced.
    pub rows: u64,
    /// Wall time including the operator's inputs.
    pub micros: u64,
}

/// How a query ran ([`explain_query`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    /// `native`, or `spareval` for queries the native executor doesn't support (no steps).
    pub executor: &'static str,
    pub steps: Vec<PlanStep>,
    /// Solutions (1 or 0 for ASK); CONSTRUCT and DESCRIBE count triples.
    pub rows: u64,
    pub micros: u64,
}

/// Runs `query` like [`evaluate_query`], consuming its results, and reports how it ran:
/// the executor, each operator with its (estimated and) actual rows, and times.
pub fn explain_query<V: ReadView>(
    view: &V,
    query: &Query,
    options: &QueryOptions,
) -> Result<Explanation, QueryEvaluationError> {
    let start = std::time::Instant::now();
    if !options.force_spareval
        && let Some(snapshot) = view.snapshot()
        && let Some(explained) = crate::native::explain(snapshot, query, options)
    {
        let (steps, rows) = explained?;
        return Ok(Explanation {
            executor: "native",
            steps,
            rows,
            micros: start.elapsed().as_micros() as u64,
        });
    }
    let spareval = QueryOptions {
        force_spareval: true,
        ..options.clone()
    };
    let rows = match evaluate_query(view, query, &spareval)? {
        QueryResults::Solutions(solutions) => {
            let mut rows = 0;
            for solution in solutions {
                solution?;
                rows += 1;
            }
            rows
        }
        QueryResults::Boolean(value) => u64::from(value),
        QueryResults::Graph(triples) => {
            let mut rows = 0;
            for triple in triples {
                triple?;
                rows += 1;
            }
            rows
        }
    };
    Ok(Explanation {
        executor: "spareval",
        steps: Vec::new(),
        rows,
        micros: start.elapsed().as_micros() as u64,
    })
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
