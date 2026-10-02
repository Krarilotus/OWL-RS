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
pub use incremental::validate_changes;
pub use model::{Component, SH, Severity, ShapeRef, Shapes};
pub use report::{PropertyPath, ValidationReport, ValidationResult};

use nrese_sparql::ReadView;

/// Validates the statements of `data` against the targeted shapes of `shapes`.
///
/// `shapes` must have been compiled from the same view (or one of the same repository):
/// it holds that dictionary's term ids.
/// Whether validating `data` against `shapes` checks anything: some active targeted shape
/// has a focus node and a constraint. A report that conforms while nothing applies is a
/// vacuous pass, which callers may need to tell apart from a real one.
pub fn applicable<V: ReadView + Sync>(view: &V, shapes: &Shapes, data: Selection) -> bool {
    validate::applicable(view, shapes, data)
}

pub fn validate<V: ReadView + Sync>(
    view: &V,
    shapes: &Shapes,
    data: Selection,
) -> ValidationReport {
    let (raw, failures) = validate::validate_raw(view, shapes, data);
    report::decode(view, shapes, &raw, failures)
}
