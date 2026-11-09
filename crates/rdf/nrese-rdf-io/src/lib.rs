//! NRESE's RDF readers and writers: N-Triples and N-Quads (also in parallel chunks),
//! Turtle, TriG, RDF/XML and JSON-LD.
//!
//! Part of the RDF bundle (`crates/rdf`), replacing `oxrdfio`, `oxttl`, `oxrdfxml` and
//! `oxjsonld`. A [`RdfParser`] reads a document from a slice or any `Read`; its
//! [`QuadParser`] hands out each quad borrowed from its buffers ([`QuadParser::next_ref`],
//! no allocation per term) or, as an iterator, owned. A [`RdfSerializer`] writes to any
//! `Write`, buffered.

#![forbid(unsafe_code)]

mod binary;
mod blank;
mod error;
mod format;
mod input;
pub mod jsonld;
pub mod n3;
mod ntriples;
mod parser;
mod rdfxml;
mod serializer;
mod text;
mod turtle;

// The split tests' documents and comparison, shared with the integration tests (`tests/it`),
// which name this crate `nrese_rdf_io`: the crate's test of each cut alone reads them too.
#[cfg(test)]
extern crate self as nrese_rdf_io;
#[cfg(test)]
#[path = "../tests/it/oracle.rs"]
mod oracle;
#[cfg(test)]
#[path = "../tests/it/recovery_inputs.rs"]
mod recovery_inputs;

pub use error::{RdfParseError, RdfSyntaxError, SkippedStatement, TextPosition};
pub use format::RdfFormat;
pub use ntriples::parse_term;
pub use parser::{QuadParser, RdfParser};
pub use serializer::{QuadSerializer, RdfSerializer};
