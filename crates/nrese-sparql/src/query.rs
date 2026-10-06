//! SPARQL query execution over a [`ReadView`].

use nrese_sparql_syntax::Query;

use crate::results::{
    CancellationToken, QueryDatasetSpecification, QueryEvaluationError, QueryResults,
};
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
    /// The data is closed under `owl:sameAs` (materialised by a ruleset with the equality
    /// rules): every fact about one identity holds for the others. Some operators then
    /// work on one representative per identity class where the answer can't tell.
    pub equality_closed: bool,
    /// With equality classes kept over representatives (`reasoner.equality = "compact"`):
    /// answer with one identity per class, the representative, instead of every identity
    /// (`equality.answers = "canonical"`, work package W4 stage C): for analytics, where
    /// the identities are one thing. Constants of the query keep their own names.
    pub equality_canonical: bool,
    /// With equality classes kept over representatives: expand them at every read (stage
    /// B), not after the joins (stage C, the default: the parts of a query whose answer
    /// can't tell the identities apart are joined over representatives and expanded
    /// once). The answers are the same; this is the comparison and an escape hatch.
    pub equality_early_expansion: bool,
    /// Pre-bound variables (SHACL-SPARQL's `$this`, `$value`, parameters): their values
    /// are put in for them throughout the query before it is evaluated, as SHACL §5.6
    /// defines pre-binding. A subquery sees them only where it projects them; the query's
    /// own projection drops them (the caller knows their values).
    pub pre_bound:
        Option<std::sync::Arc<std::collections::HashMap<nrese_rdf::Variable, nrese_rdf::Term>>>,
    /// The rows of a cross product made at once below a GROUP BY (the product is grouped
    /// in chunks of that size); `None`: 4 M. Tests make it small, so that groups are
    /// split across many chunks.
    pub cross_chunk_rows: Option<usize>,
    /// Evaluate every GROUP BY that can stream in morsels of this many rows of its first
    /// pattern (`native/stream.rs`), not only when it runs out of memory. Tests set it.
    pub stream_rows: Option<usize>,
    /// The graphs the query's user may read (graph-level access control): its dataset is
    /// restricted to them. `None`: every graph.
    pub access: Option<std::sync::Arc<crate::GraphAccess>>,
    /// GeoSPARQL relations in triple patterns (`?a geo:sfWithin ?b`) read only the
    /// statements that assert them: GeoSPARQL's topology vocabulary without its
    /// query-rewrite extension, which by default also computes them from the geometries.
    pub geosparql_stated_only: bool,
}

/// Evaluates `query` against `view` over the engine's id tables (`native`); a transaction
/// is read through a snapshot of its pending state. Results are computed, then decoded as
/// they are read.
pub fn evaluate_query<'a, V: ReadView>(
    view: &'a V,
    query: &Query,
    options: &QueryOptions,
) -> Result<QueryResults<'a>, QueryEvaluationError> {
    crate::native::evaluate(view.evaluation_snapshot(), query, options)
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
/// `None` for what isn't written this way (a transaction view, CONSTRUCT, DESCRIBE, ASK in
/// TSV/CSV): then serialise [`evaluate_query`]'s results. On error the output is partial.
/// `version` is announced in JSON's head (`"1.2"`: the results may use RDF 1.2).
pub fn write_results<V: ReadView>(
    view: &V,
    query: &Query,
    options: &QueryOptions,
    format: crate::ResultsFormat,
    version: Option<&'static str>,
    out: &mut dyn std::io::Write,
) -> Option<Result<(), WriteResultsError>> {
    crate::native::write_results(view.snapshot()?, query, options, format, version, out)
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
    /// `native`: the executor (one; the field stays for the report's format).
    pub executor: &'static str,
    /// The rewrites that changed the query before it ran, in the order applied:
    /// `triple-terms`, `join-groups`, `filter-pushdown`, `ask-limit`.
    pub rewrites: Vec<&'static str>,
    pub steps: Vec<PlanStep>,
    /// Solutions (1 or 0 for ASK); CONSTRUCT and DESCRIBE count triples.
    pub rows: u64,
    pub micros: u64,
    /// Whether the answers are complete, where a reasoning path can leave some out (set
    /// by the store; `None` where nothing can).
    pub completeness: Option<crate::completeness::Completeness>,
}

/// Runs `query` like [`evaluate_query`], consuming its results, and reports how it ran:
/// the executor, each operator with its (estimated and) actual rows, and times.
pub fn explain_query<V: ReadView>(
    view: &V,
    query: &Query,
    options: &QueryOptions,
) -> Result<Explanation, QueryEvaluationError> {
    let start = std::time::Instant::now();
    let snapshot = view.evaluation_snapshot();
    let (rewrites, steps, rows) = crate::native::explain(&snapshot, query, options)?;
    Ok(Explanation {
        executor: "native",
        rewrites,
        steps,
        rows,
        micros: start.elapsed().as_micros() as u64,
        completeness: None,
    })
}

/// One node of a query's plan before it runs ([`plan_query`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    /// Nesting depth: a node's inputs follow it one level deeper.
    pub depth: usize,
    /// `scan`, `bgp`, `join`, `optional`, `filter`, `union`, `path`, `group`, ...
    pub operator: String,
    /// The triple pattern, expression or variables the node works on.
    pub detail: String,
    /// Rows estimated from the store's statistics; `None` where unknown (`SERVICE`).
    pub estimated_rows: Option<u64>,
}

/// The plan a query would run as ([`plan_query`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedQuery {
    /// The rewrites that changed the query, in the order applied (as in [`Explanation`]).
    pub rewrites: Vec<&'static str>,
    pub steps: Vec<PlannedStep>,
    /// What the store can say about completeness before running (set by the store).
    pub completeness: Option<crate::completeness::Completeness>,
}

/// The plan `query` would run as on `view`, after the rewrites, each node with an estimate
/// of its rows, without running it (EXPLAIN without ANALYZE).
pub fn plan_query<V: ReadView>(
    view: &V,
    query: &Query,
    options: &QueryOptions,
) -> Result<PlannedQuery, QueryEvaluationError> {
    let snapshot = view.evaluation_snapshot();
    let (rewrites, steps) = crate::native::plan(&snapshot, query, options)?;
    Ok(PlannedQuery {
        rewrites,
        steps,
        completeness: None,
    })
}

/// True if `query` would run on the native executor over a snapshot with default options.
/// For coverage reports and tests; the decision itself is made per call.
pub fn runs_natively(query: &Query) -> bool {
    crate::native::query_supported(query)
}
