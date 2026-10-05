//! The datatype theory (docs/design/owl2-dl.md, "The datatype theory"; package 3.5): a
//! component with its own interface, called by the hypertableau.
//!
//! - [`Ranges`]: the ontology's data ranges as sets of values over `nrese-xsd`'s value
//!   spaces (the OWL 2 datatype map), exact or bounded both ways where a range can't be
//!   decided (patterns, language ranges, datatypes outside the map), and saying so;
//! - [`DatatypeTheory`]: `add_literal`, `add_range` with polarity, `add_not_equal`,
//!   `merge`, `min_cardinality`, and `check(component) -> Sat | Clash(DepSetId)`, solved
//!   per connected component of data variables, clashes carrying dependency sets.

mod ranges;
#[cfg(test)]
mod tests;
mod theory;

pub use crate::tableau::depset::{DepSetId, DepSets};
pub use ranges::{Eval, Ranges};
pub use theory::{DataVar, DatatypeTheory, Verdict};
