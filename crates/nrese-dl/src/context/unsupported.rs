//! Why the Horn stage gives an ontology up: a typed answer, never a wrong taxonomy.

use std::fmt;

/// Why the Horn stage gives an ontology up. Each names an axiom (an index into the
/// ontology's axioms) where there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// A clause with two head atoms that no renaming of fresh names makes Horn
    /// (`None`: the fresh names' polarities contradict one another).
    NotHorn {
        axiom: Option<usize>,
    },
    /// Equality: functional properties, `≤ n`, keys.
    Equality {
        axiom: usize,
    },
    Nominals {
        axiom: usize,
    },
    /// A clause that requires data values (`∃d.R`, `≥ n d.R`) or data assertions.
    Datatypes {
        axiom: usize,
    },
    /// Assertions the Horn stage can't place (property assertions need nominals).
    Assertions(&'static str),
    /// A clause outside the DL-clause shapes (e.g. a role atom between two neighbours).
    ClauseShape {
        axiom: usize,
    },
    /// What the normalisation couldn't cover itself.
    Normalisation {
        axiom: usize,
        why: &'static str,
    },
    /// More concepts, roles or functions than an atom holds.
    TooLarge,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unsupported::NotHorn { axiom: Some(a) } => write!(out, "not Horn (axiom {a})"),
            Unsupported::NotHorn { axiom: None } => write!(out, "not Horn (no renaming)"),
            Unsupported::Equality { axiom } => write!(out, "equality (axiom {axiom})"),
            Unsupported::Nominals { axiom } => write!(out, "nominals (axiom {axiom})"),
            Unsupported::Datatypes { axiom } => write!(out, "datatypes (axiom {axiom})"),
            Unsupported::Assertions(what) => write!(out, "assertions: {what}"),
            Unsupported::ClauseShape { axiom } => write!(out, "clause shape (axiom {axiom})"),
            Unsupported::Normalisation { axiom, why } => write!(out, "{why} (axiom {axiom})"),
            Unsupported::TooLarge => write!(out, "too many concepts or roles"),
        }
    }
}

impl std::error::Error for Unsupported {}
