//! NRESE SPARQL layer (L2, see `docs/ARCHITECTURE.md`).
//!
//! Owns SPARQL semantics over the engine: query evaluation through `spareval` (ADR-0001),
//! protocol dataset parameters, cancellation, and SPARQL Update execution into an engine
//! transaction. Parsing is `spargebra`'s.
//!
//! Not owned here: storage and visibility (L1 `nrese-engine`), whether a delta may be
//! committed (L3 mutation pipeline: validation, deadlines, commit), HTTP (L4).
//!
//! - [`view`]: the read contract ([`ReadView`]) implemented by snapshots and transactions
//! - [`dataset`]: the `spareval::QueryableDataset` adapter and its term-identity invariant
//! - [`query`] evaluates queries ([`evaluate_query`]): natively over id tables when every
//!   operator is supported (`native`, the execution core of D11), else with spareval
//! - [`update`] applies SPARQL Update requests ([`apply_update`])

pub mod dataset;
mod native;
pub mod query;
pub mod update;
pub mod view;

pub use dataset::{EngineDataset, EvalTerm};
pub use native::ResultsFormat;
pub use nrese_exec::BudgetExceeded;
pub use query::{
    Explanation, PlanStep, QueryOptions, WriteResultsError, evaluate_query, explain_query,
    runs_natively, write_results,
};
pub use spareval::{
    CancellationToken, QueryDatasetSpecification, QueryEvaluationError, QueryResults,
};
pub use update::{UpdateError, UpdateOptions, apply_update};
pub use view::ReadView;
