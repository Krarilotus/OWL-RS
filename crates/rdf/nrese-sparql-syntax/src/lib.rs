//! NRESE's SPARQL 1.1 and 1.2 parser, algebra and writer.
//!
//! [`SparqlParser`] reads queries and updates in one pass into the algebra of SPARQL 1.1
//! §18 (with the SPARQL 1.2 additions); [`Query`] and [`Update`] print as SPARQL that
//! parses back to the same algebra. Generated names (aggregate results, anonymous blank
//! nodes, …) are numbered, so the same text always gives the same algebra.

#![forbid(unsafe_code)]

pub mod algebra;
mod parser;
mod query;
pub mod term;
mod writer;

pub use parser::{DEFAULT_MAX_NESTING, SparqlParser, SparqlSyntaxError, TextPosition};
pub use query::{GraphUpdateOperation, Query, Update};
