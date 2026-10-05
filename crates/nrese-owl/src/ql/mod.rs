//! OWL 2 QL answers through existentials (docs/design/ql-rewriting.md): the QL part of an
//! ontology compiled for rewriting ([`Tbox`]), and the tree-witness rewriting of a
//! conjunctive query over data the materialisation has closed ([`rewrite`]).
//!
//! Everything here is over the source's term ids; the SPARQL layer turns basic graph
//! patterns into [`Cq`]s and [`Rewriting`]s back into patterns.

mod rewrite;
mod tbox;
mod witness;

#[cfg(test)]
mod tests;

pub use rewrite::{Branch, Limits, Outcome, Part, Rewriting, rewrite};
pub use tbox::{Basic, Closure, Role, Tbox};

use crate::model::Term;

/// A term of a query: a variable (by number) or a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum QTerm {
    Var(u32),
    Const(Term),
}

/// An atom of a query.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Atom {
    /// `t rdf:type class`.
    Class(QTerm, Term),
    /// `s property o`.
    Role(QTerm, Term, QTerm),
    /// An atom the rewriting doesn't read (a variable predicate or class, a term the store
    /// doesn't know), with its variables: kept as it is, and its variables are never
    /// mapped to anonymous elements. The number is the caller's.
    Other(usize, Vec<u32>),
}

impl Atom {
    pub fn terms(&self) -> Vec<QTerm> {
        match self {
            Self::Class(t, _) => vec![*t],
            Self::Role(s, _, o) => vec![*s, *o],
            Self::Other(_, vars) => vars.iter().map(|&v| QTerm::Var(v)).collect(),
        }
    }

    pub fn vars(&self) -> Vec<u32> {
        self.terms()
            .into_iter()
            .filter_map(|t| match t {
                QTerm::Var(v) => Some(v),
                QTerm::Const(_) => None,
            })
            .collect()
    }

    pub fn has_var(&self, var: u32) -> bool {
        self.terms().contains(&QTerm::Var(var))
    }

    /// The atom with each variable replaced as `map` says.
    pub fn map(&self, map: &impl Fn(QTerm) -> QTerm) -> Self {
        match self {
            Self::Class(t, c) => Self::Class(map(*t), *c),
            Self::Role(s, p, o) => Self::Role(map(*s), *p, map(*o)),
            Self::Other(n, vars) => Self::Other(*n, vars.clone()),
        }
    }
}

/// A conjunctive query: a basic graph pattern's atoms; variables are numbered `0..vars`,
/// and `existential[v]` says whether nothing outside the pattern reads `v` (a blank
/// node, or a variable not projected and not used elsewhere).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cq {
    pub atoms: Vec<Atom>,
    pub vars: u32,
    pub existential: Vec<bool>,
}
