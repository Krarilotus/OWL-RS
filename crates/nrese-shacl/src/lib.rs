//! NRESE SHACL (L2, see `docs/ARCHITECTURE.md` and `docs/design/shacl.md`).
//!
//! Owns SHACL semantics: reading a shapes graph into a program of term ids, validating a
//! data graph against it, and the validation report.
//!
//! - [`compile`] reads the shapes graph ([`Shapes`]); an ill-formed one is an error
//! - [`validate`] checks the data against every targeted shape ([`ValidationReport`])
//! - [`validate_changes`] reports only what a change introduces, validating the focus
//!   nodes it can affect (the commit gate's check)
//!
//! Both read through [`nrese_sparql::ReadView`], so the same code validates a committed
//! snapshot and the state inside an open transaction. Value comparison is SPARQL's
//! ([`nrese_sparql::value`]), as SHACL defines it.
//!
//! Not owned here: when a store validates and what a failed validation means for a
//! commit (L3 mutation pipeline), endpoints and formats (L4).
//!
//! Supported: SHACL Core and SHACL-SPARQL (`sh:sparql` constraints and SPARQL-based
//! constraint components, with pre-binding by substitution: [`sparql`]).

mod compile;
mod datatype;
mod graph;
mod incremental;
mod model;
mod path;
mod report;
mod sparql;
mod validate;

pub use compile::{ShapeError, compile};
pub use graph::Selection;
pub use incremental::{validate_changes, validate_shapes_change};
pub use model::{Component, SH, Severity, ShapeRef, Shapes};
pub use report::{PropertyPath, ValidationReport, ValidationResult};

use nrese_sparql::{QueryEvaluationError, ReadView, Resources};

/// Whether validating `data` against `shapes` checks anything: some active targeted shape
/// has a focus node and a constraint. A report that conforms while nothing applies is a
/// vacuous pass, which callers may need to tell apart from a real one.
pub fn applicable<V: ReadView + Sync>(view: &V, shapes: &Shapes, data: Selection) -> bool {
    validate::applicable(view, shapes, data)
}

/// Validates the statements of `data` against the targeted shapes of `shapes`.
///
/// `shapes` must have been compiled from the same view (or one of the same repository):
/// it holds that dictionary's term ids.
///
/// The validation runs on the caller's `resources`, its nested SPARQL queries included:
/// when their cancellation fires or their envelope refuses a charge, it stops with that
/// error ([`QueryEvaluationError::Cancelled`], [`QueryEvaluationError::MemoryLimit`]) and
/// no report. A stop is never one of the report's failures.
pub fn validate<V: ReadView + Sync>(
    view: &V,
    shapes: &Shapes,
    data: Selection,
    resources: &Resources,
) -> Result<ValidationReport, QueryEvaluationError> {
    let (raw, failures, held) = validate::validate_raw(view, shapes, data, resources, false)?;
    let report = report::decode(view, shapes, &raw, failures, resources);
    // The raw results are freed, then their charge returned.
    drop(raw);
    drop(held);
    report
}
