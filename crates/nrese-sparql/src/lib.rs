//! NRESE SPARQL layer (L2, see `docs/ARCHITECTURE.md`).
//!
//! Owns SPARQL semantics over the engine: query evaluation (its own executor over the
//! engine's id tables), protocol dataset parameters, cancellation, and SPARQL Update
//! execution into an engine transaction. Parsing is `nrese_sparql_syntax`'s.
//!
//! Not owned here: storage and visibility (L1 `nrese-engine`), whether a delta may be
//! committed (L3 mutation pipeline: validation, deadlines, commit), HTTP (L4).
//!
//! - [`view`]: the read contract ([`ReadView`]) implemented by snapshots and transactions
//! - [`dataset`]: what a query reads (`FROM`, `FROM NAMED`, the protocol's parameters)
//! - [`results`]: results, errors, cancellation
//! - [`query`] evaluates queries ([`evaluate_query`]) over id tables (`native`, the
//!   execution core of D11)
//! - [`update`] applies SPARQL Update requests ([`apply_update`])
//! - [`cache`]: the results of query plan parts, shared by a store's queries

pub mod cache;
pub mod compat;
pub mod completeness;
pub mod dataset;
pub mod plan;
pub use cache::{
    CachedOutput, OutputSlot, PinRequest, PinnedResult, ResultCache, ResultCacheStats,
};
pub use dataset::GraphAccess;
mod native;
pub mod ql;
pub mod query;
pub mod results;
pub mod service;
pub mod update;
pub mod view;

/// SPARQL's value semantics for terms (§17): what `<`, `=` and friends compare. Public for
/// the layers that are defined through them (SHACL's value-range and property-pair
/// constraints).
pub mod value {
    pub use crate::native::expr::compile_regex;
    pub use crate::native::value::{Value, canonical, compare, equals, lang_matches, order};
    /// The values of `SUM`, `AVG` and `GROUP_CONCAT` over a group's values.
    pub use crate::native::{average, group_concat, sum};
}

/// The evaluator of SPARQL expressions over terms (functions, operators, casts,
/// GeoSPARQL): for the layers and tools that evaluate expressions outside a query (the
/// reference evaluator of the tests, SHACL-SPARQL).
pub mod expression {
    pub use crate::native::expr::Evaluator;
}

pub use completeness::Completeness;
pub use native::{ResultsFormat, geometry_literal};
pub use nrese_exec::{BudgetExceeded, SharedBudget};
pub use query::{
    Explanation, PlanStep, PlannedQuery, PlannedStep, QueryOptions, WriteResultsError,
    cached_output, evaluate_query, explain_query, plan_query, ql_report, runs_natively,
    write_results,
};
pub use results::{
    CancellationToken, QueryDatasetSpecification, QueryEvaluationError, QueryResults,
    QuerySolution, QuerySolutionIter, QueryTripleIter,
};
pub use service::{ServiceClient, ServiceResults, Services};
pub use update::{UpdateError, UpdateOptions, apply_update, graph_label};
pub use view::ReadView;
