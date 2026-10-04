//! OWL 2 for NRESE (docs/design/owl2-dl.md): the structural model of an ontology read
//! from RDF triples ([`read`]), with every axiom's source and a diagnostic for whatever
//! isn't well-formed OWL 2 DL, and the way back to triples ([`write`]).
//!
//! It works over the term ids of whatever holds the triples (the store's `TermId`s) and
//! reads strings only of the literals it needs ([`Terms`]); nothing here depends on the
//! store or an engine.

pub mod clauses;
pub mod diagnostics;
mod functional;
pub mod fuzz;
pub mod mapping;
pub mod model;
pub mod normalise;
pub mod proof;
mod properties;
pub mod vocab;
pub mod write;

pub use clauses::{BodyAtom, Clause, Concept, Facts, Filler, FreshOf, HeadAtom, Normalised, Var};
pub use diagnostics::Diagnostic;
pub use mapping::{BuiltinProperties, Ontology, Source, Statement, TermKind, Terms, read};
pub use model::{
    Axiom, Characteristic, ClassExpr, DataRange, EntityKind, ExprId, Interner, ObjProp, RangeId,
    Term,
};
pub use normalise::{Options, normalise, normalise_with};
pub use proof::{Inference, Justifications, Proof, ProofError, ProofGraph};
pub use vocab::Vocabulary;
pub use write::{Make, write};
