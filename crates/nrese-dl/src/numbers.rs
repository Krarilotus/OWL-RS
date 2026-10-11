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
//! 3. [`candidate`]: a compressed model built from the counts, untrusted.
//! 4. [`validate`]: an independent check of a candidate against the ontology's axioms,
//!    the only way to [`Validated`] and so to "consistent".
//!
//! Anything else (a problem outside the fragment, no candidate, a candidate the validator
//! declines) answers nothing: the search decides.

pub mod candidate;
pub mod closure;
pub mod problem;
pub mod validate;

pub use closure::Refutation;
use nrese_owl::Ontology;
pub use validate::Validated;

/// Whether the counts the axioms fix refute `ontology` (layers 1 and 2).
pub fn refute(ontology: &Ontology) -> Option<Refutation> {
    closure::close(&problem::extract(ontology)).err()
}

/// A model of `ontology` from its counts, checked (layers 1 to 4); `None` where the module
/// can't show one.
pub fn model(ontology: &Ontology) -> Option<Validated> {
    let p = problem::extract(ontology);
    if !p.complete() {
        return None;
    }
    let counts = closure::close(&p).ok()?;
    let candidate = candidate::construct(&p, &counts)?;
    validate::validate(ontology, &candidate).ok()
}
