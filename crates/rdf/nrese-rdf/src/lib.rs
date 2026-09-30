//! NRESE's RDF model: IRIs, terms, triples and quads, in-memory graphs and datasets with
//! canonicalisation, and the common vocabularies.
//!
//! Part of the RDF bundle (`crates/rdf`), which replaces the Oxigraph crates: the API
//! follows `oxrdf` closely so that the switch is mechanical, and nothing here depends on
//! the storage engine. With the `xsd` feature (on by default), literals convert from and to
//! the values of `nrese-xsd`.

#![forbid(unsafe_code)]

pub mod graph;
pub mod iri;
pub mod language;
pub mod term;
pub mod triple;
pub mod vocab;
#[cfg(feature = "xsd")]
mod xsd_literals;

pub use graph::{Dataset, Graph};
pub use iri::{Iri, IriParseError};
pub use term::{
    BlankNode, BlankNodeRef, GraphName, GraphNameRef, Literal, LiteralRef, NamedNode, NamedNodeRef,
    NamedOrBlankNode, NamedOrBlankNodeRef, NotANodeError, Subject, SubjectRef, Term,
    TermParseError, TermRef, Variable,
};
pub use triple::{Quad, QuadRef, Triple, TripleRef};
