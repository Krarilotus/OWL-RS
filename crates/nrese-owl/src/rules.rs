//! DL-safe rules (SWRL in the OWL API's functional syntax, `DLSafeRule(Body(…) Head(…))`):
//! not OWL 2 (*OWL 2 Structural Specification* §9 has no rule axiom), read so that what
//! they say can be decided where it can (`nrese_dl::beyond`) and named where it can't.
//!
//! **Semantics** (Motik, Sattler and Studer, *Query Answering for OWL-DL with Rules*, JWS
//! 2005; the OWL API's reading, HermiT's and Pellet's): a rule holds in an interpretation if
//! for every binding of its individual variables to **named** individuals and of its data
//! variables to data values under which every body atom holds, every head atom holds. A
//! built-in atom (`swrlb:…`) holds where its relation holds of its arguments' values.
//!
//! Each rule is kept with a fatal [`crate::Diagnostic::Rule`], so that whatever decides
//! OWL 2 DL alone answers for the rest and says it left the rule out.

use crate::model::{ExprId, ObjProp, RangeId, Term};

/// An individual argument: a variable (by its index in [`Rule::variables`]) or a named
/// individual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IArg {
    Var(u32),
    Individual(Term),
}

/// A data argument: a variable or a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DArg {
    Var(u32),
    Literal(Term),
}

/// An atom of a rule's body or head.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Atom {
    Class(ExprId, IArg),
    Range(RangeId, DArg),
    Object(ObjProp, IArg, IArg),
    Data(Term, IArg, DArg),
    Same(IArg, IArg),
    Different(IArg, IArg),
    /// A built-in's IRI and its arguments.
    BuiltIn(Term, Vec<DArg>),
}

/// A rule: where every body atom holds, every head atom does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rule {
    pub body: Vec<Atom>,
    pub head: Vec<Atom>,
    /// The variables' IRIs, by index; `data[i]`: whether variable `i` is a data variable.
    pub variables: Vec<Term>,
    pub data: Vec<bool>,
    /// Where it was read from (from 1; 0 where it wasn't read from text).
    pub line: u32,
}

impl Atom {
    /// The variables it mentions, as individual (`false`) or data (`true`) variables.
    pub fn variables(&self) -> Vec<(u32, bool)> {
        let i = |a: &IArg| match *a {
            IArg::Var(v) => Some((v, false)),
            IArg::Individual(_) => None,
        };
        let d = |a: &DArg| match *a {
            DArg::Var(v) => Some((v, true)),
            DArg::Literal(_) => None,
        };
        match self {
            Atom::Class(_, x) => i(x).into_iter().collect(),
            Atom::Range(_, x) => d(x).into_iter().collect(),
            Atom::Object(_, x, y) | Atom::Same(x, y) | Atom::Different(x, y) => {
                i(x).into_iter().chain(i(y)).collect()
            }
            Atom::Data(_, x, v) => i(x).into_iter().chain(d(v)).collect(),
            Atom::BuiltIn(_, args) => args.iter().filter_map(d).collect(),
        }
    }
}

impl Rule {
    /// Why it isn't a rule NRESE can evaluate: a variable used both for individuals and
    /// for data values, or a head variable no body atom binds (the rule would quantify it
    /// over every element, which DL-safety forbids). A data variable a built-in computes is
    /// bound by it ([`crate::rules`]' semantics; `nrese_dl::beyond` evaluates it).
    pub fn malformed(&self) -> Option<&'static str> {
        let n = self.variables.len();
        let mut kind: Vec<Option<bool>> = vec![None; n];
        let mut bound = vec![false; n];
        for (atoms, body) in [(&self.body, true), (&self.head, false)] {
            for atom in atoms {
                for (v, data) in atom.variables() {
                    match kind[v as usize] {
                        Some(k) if k != data => {
                            return Some("a variable used for individuals and for data values");
                        }
                        _ => kind[v as usize] = Some(data),
                    }
                    if body {
                        bound[v as usize] = true;
                    }
                }
            }
        }
        if (0..n).any(|v| kind[v].is_some() && !bound[v]) {
            return Some("a head variable no body atom mentions");
        }
        None
    }
}
