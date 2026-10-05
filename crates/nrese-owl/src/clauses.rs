//! DL-clauses (Motik, Shearer and Horrocks, *Hypertableau Reasoning for Description
//! Logics*, JAIR 2009): `U₁ ∧ … ∧ Uₘ → V₁ ∨ … ∨ Vₙ` over a centre variable `x` and
//! successor variables `y₁…`, the form every engine of `nrese-dl` reads
//! (docs/design/owl2-dl.md §3). Concepts in clauses are atomic: named classes or fresh
//! names the normalisation introduced for subexpressions.

use crate::model::{ClassExpr, DataRange, ExprId, Interner, ObjProp, RangeId, Term};

/// A variable of a clause: the centre `x`, a successor `y(i)`, or a data value `v(i)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Var {
    X,
    Y(u16),
    V(u16),
}

/// An atomic concept of the clauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Concept {
    /// A named class of the ontology.
    Named(Term),
    /// A fresh name the normalisation introduced, by number ([`Normalised::fresh`]).
    Fresh(u32),
}

/// What a fresh name stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshOf {
    /// A subexpression: `Q ⊑ E` for a positive occurrence, `E ⊑ Q` for a negative one
    /// (each name occurs with its polarity only, so `Q := E` extends any model).
    Expr { expr: ExprId, positive: bool },
    /// A state of the automaton for `∀ role. filler` over a non-simple role.
    State { role: ObjProp, state: u32 },
}

/// A body atom.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BodyAtom {
    Concept(Concept, Var),
    /// `role(a, b)`; inverses are written with the arguments swapped.
    Role(Term, Var, Var),
    /// The variable is the named individual.
    Nominal(Term, Var),
    /// `property(a, v)` for a data property.
    Data(Term, Var, Var),
}

/// The filler of an at-least atom: an atomic concept, its complement, or anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Filler {
    Top,
    Is(Concept),
    Not(Concept),
}

/// A head atom.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HeadAtom {
    Concept(Concept, Var),
    Role(Term, Var, Var),
    /// At least `n` `role`-successors of `var` that satisfy `filler`.
    AtLeast {
        n: u32,
        role: ObjProp,
        filler: Filler,
        var: Var,
    },
    /// At most `n` `role`-successors of `var` that satisfy `filler`: where `≤ n` is
    /// too large to spell out as `n + 1` successors with equalities (HermiT's
    /// `AtMostConcept`; engines merge successors by it).
    AtMost {
        n: u32,
        role: ObjProp,
        filler: Filler,
        var: Var,
    },
    Equal(Var, Var),
    /// The variable is the named individual.
    Nominal(Term, Var),
    /// At least `n` values of the data `property` of `var` in `range`.
    DataAtLeast {
        n: u32,
        property: Term,
        range: RangeId,
        var: Var,
    },
    /// At most `n` values of the data `property` of `var` in `range`.
    DataAtMost {
        n: u32,
        property: Term,
        range: RangeId,
        var: Var,
    },
    /// The data value `var` is in `range`.
    DataIn(RangeId, Var),
    /// Two data values are the same.
    DataEqual(Var, Var),
    /// Two data values differ (`DisjointDataProperties`: the values two properties give
    /// one individual are never the same value, whichever data nodes hold them).
    DataUnequal(Var, Var),
    /// `property(a, v)` for a data property.
    DataRole(Term, Var, Var),
}

/// What a clause needs and does, for dispatch (docs/design/owl2-dl.md §3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// At most one head atom.
    pub horn: bool,
    pub existential: bool,
    pub equality: bool,
    pub number: bool,
    pub nominal: bool,
    pub datatype: bool,
}

/// A DL-clause with the axioms it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    pub body: Vec<BodyAtom>,
    /// Empty: the body is contradictory (`⊥`).
    pub head: Vec<HeadAtom>,
    /// The axioms the clause comes from, as alternatives: each set's axioms together give
    /// it (indexes into the ontology's axioms). Most clauses have one set of one axiom;
    /// clauses that coincided have a set per origin; a transition of a role's automaton
    /// needs the universal it encodes and the role inclusions of its edge together.
    pub sources: Vec<Vec<usize>>,
    pub flags: Flags,
}

impl Clause {
    /// A clause from its atoms (sorted, deduplicated) with its flags; `sources` is one
    /// set of axioms that give it.
    pub fn new(body: Vec<BodyAtom>, head: Vec<HeadAtom>, sources: Vec<usize>) -> Self {
        let mut body = body;
        body.sort();
        body.dedup();
        let mut head = head;
        head.sort();
        head.dedup();
        let flags = Flags {
            horn: head.len() <= 1,
            existential: head
                .iter()
                .any(|h| matches!(h, HeadAtom::AtLeast { .. } | HeadAtom::DataAtLeast { .. })),
            equality: head.iter().any(|h| {
                matches!(
                    h,
                    HeadAtom::Equal(..)
                        | HeadAtom::DataEqual(..)
                        | HeadAtom::AtMost { .. }
                        | HeadAtom::DataAtMost { .. }
                )
            }),
            number: head.iter().any(|h| {
                matches!(h, HeadAtom::AtLeast { n, .. } | HeadAtom::DataAtLeast { n, .. } if *n > 1)
                    || matches!(
                        h,
                        HeadAtom::Equal(..) | HeadAtom::AtMost { .. } | HeadAtom::DataAtMost { .. }
                    )
            }),
            nominal: body.iter().any(|b| matches!(b, BodyAtom::Nominal(..)))
                || head.iter().any(|h| matches!(h, HeadAtom::Nominal(..))),
            datatype: body.iter().any(|b| matches!(b, BodyAtom::Data(..)))
                || head.iter().any(|h| {
                    matches!(
                        h,
                        HeadAtom::DataAtLeast { .. }
                            | HeadAtom::DataAtMost { .. }
                            | HeadAtom::DataIn(..)
                            | HeadAtom::DataEqual(..)
                            | HeadAtom::DataUnequal(..)
                            | HeadAtom::DataRole(..)
                    )
                }),
        };
        Self {
            body,
            head,
            sources: vec![sources],
            flags,
        }
    }
}

/// The facts of the ABox, over named (and anonymous) individuals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `(concept, individual)`: class assertions, complex ones through a fresh name.
    pub concepts: Vec<(Concept, Term, usize)>,
    /// `(role, subject, object)`.
    pub roles: Vec<(Term, Term, Term, usize)>,
    /// `(property, subject, literal)`.
    pub data: Vec<(Term, Term, Term, usize)>,
    pub same: Vec<(Term, Term, usize)>,
    pub different: Vec<(Term, Term, usize)>,
    /// Negative assertions: `(role, subject, object)`.
    pub not_roles: Vec<(Term, Term, Term, usize)>,
    pub not_data: Vec<(Term, Term, Term, usize)>,
}

/// A DL-safe rule (Motik, Sattler and Studer, *Query Answering for OWL-DL with Rules*, JWS
/// 2005): its object variables bind to named individuals only, its data variables to data
/// values of theirs. What a key becomes: `HasKey(C (P…) (D…))` is
/// `C(x) ∧ C(y₀) ∧ ⋀ Pᵢ(x, yᵢ) ∧ Pᵢ(y₀, yᵢ) ∧ ⋀ Dⱼ(x, vⱼ) ∧ Dⱼ(y₀, vⱼ) → x ≈ y₀`
/// (`C` a fresh name `Q` with `C ⊑ Q` where it isn't one; no class atom for `owl:Thing`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeRule {
    pub body: Vec<BodyAtom>,
    pub head: Vec<HeadAtom>,
    /// The axiom it comes from.
    pub source: usize,
}

/// An ontology in DL-clauses.
#[derive(Debug, Clone, Default)]
pub struct Normalised {
    pub clauses: Vec<Clause>,
    pub facts: Facts,
    /// What each fresh name stands for.
    pub fresh: Vec<FreshOf>,
    /// Axioms (by index) the clauses don't cover, and why: their tasks are answered
    /// `unsupported`.
    pub unsupported: Vec<(usize, &'static str)>,
    /// The ontology's expressions with those the normalisation made (fresh names refer
    /// to them).
    pub classes: Interner<ClassExpr>,
    pub ranges: Interner<DataRange>,
    /// Keys as DL-safe rules (their axioms are also in `unsupported`, under
    /// [`crate::UNSUPPORTED_KEYS`], for engines that don't apply them).
    pub rules: Vec<SafeRule>,
    /// Datatype definitions: the datatype and its range (also in `unsupported`, under
    /// [`crate::UNSUPPORTED_DATATYPE_DEFINITIONS`]).
    pub definitions: Vec<(Term, RangeId)>,
}
