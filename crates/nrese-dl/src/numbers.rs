//! The number module (docs/design/owl2-dl.md#number-reasoning-layers): class sizes the
//! axioms fix, reasoned about by arithmetic before any search. The pigeonhole cases (W3C
//! DL-906, 907, 910: integer multiplication through a nominal and functional properties)
//! are hard for merges, which are resolution, and trivial for counting.
//!
//! The layers, each with the trust its type carries:
//! 1. [`problem`]: the ontology as a [`problem::NumberProblem`], within a fragment; what
//!    lies outside is kept, with the reason.
//! 2. [`closure`]: the sizes and edge counts the problem fixes, or a [`Refutation`]. Sound
//!    for any problem: its axioms are a subset of the ontology's.
//!
//! Nothing here claims a model yet.

pub mod closure;
pub mod problem;

pub use closure::Refutation;
use nrese_owl::Ontology;

/// Whether the counts the axioms fix refute `ontology` (layers 1 and 2).
pub fn refute(ontology: &Ontology) -> Option<Refutation> {
    closure::close(&problem::extract(ontology)).err()
}
