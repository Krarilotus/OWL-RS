//! OWL 2 for NRESE (docs/design/owl2-dl.md): the structural model of an ontology read
//! from RDF triples ([`read`]) or from the functional-style syntax ([`read_functional`]),
//! with a diagnostic for whatever isn't well-formed OWL 2 DL, and the ways back: to
//! triples ([`write`]) and to the functional-style syntax ([`Ontology::functional`]).
//!
//! It works over the term ids of whatever holds the triples (the store's `TermId`s) and
//! reads strings only of the literals it needs ([`Terms`]); nothing here depends on the
//! store or an engine.

pub mod clauses;
pub mod diagnostics;
pub mod functional;
pub mod fuzz;
pub mod mapping;
pub mod model;
pub mod normalise;
pub mod ofn;
pub mod proof;
mod properties;
mod universal;
pub mod vocab;
pub mod write;

pub use clauses::{
    BodyAtom, Clause, Concept, Facts, Filler, FreshOf, HeadAtom, Normalised, SafeRule, Var,
};
pub use diagnostics::Diagnostic;
pub use functional::{iri_text, literal_text};
pub use mapping::{BuiltinProperties, Ontology, Source, Statement, TermKind, Terms, read};
pub use model::{
    Axiom, Characteristic, ClassExpr, DataRange, DataTerms, EntityKind, ExprId, Interner, Literal,
    ObjProp, RangeId, Term,
};
pub use normalise::{
    Options, UNSUPPORTED_DATATYPE_DEFINITIONS, UNSUPPORTED_KEYS, normalise, normalise_with,
};
pub use ofn::{Document, FunctionalReader, Intern, read_functional};
pub use proof::{Inference, Justifications, Proof, ProofError, ProofGraph};
pub use vocab::Vocabulary;
pub use write::{Make, write};
