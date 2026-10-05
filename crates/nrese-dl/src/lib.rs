//! NRESE's OWL 2 DL engines (docs/design/owl2-dl.md), over the DL-clauses `nrese-owl`
//! normalises an ontology into, with their provenance:
//!
//! - [`context`]: the context-saturation core (§5), Horn stage first (package 3.1);
//! - [`tableau`]: the hypertableau, the completeness anchor (§6, package 3.3);
//! - [`bounds`]: the upper bound U1 compiled from the clauses (§8, package 3.2);
//! - [`datatypes`]: the datatype theory the hypertableau calls (package 3.5).
//!
//! Each module is its own package with its own gates (owl2-dl-performance.md §6).

pub mod bounds;
pub mod context;
pub mod datatypes;
pub mod tableau;
