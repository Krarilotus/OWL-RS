//! NRESE's JSON, shared by the JSON-LD reader and writer and the SPARQL JSON results.
//!
//! Part of the RDF bundle (`crates/rdf`). A pull parser over text in memory
//! ([`SliceJsonParser`], strings without escapes borrowed for the text's lifetime) or any
//! `Read` ([`ReaderJsonParser`]), strict RFC 8259 with a depth limit; a tree
//! ([`Value`]) built from either, iteratively; a writer of events ([`JsonWriter`]); and
//! the JSON Canonicalization Scheme of RFC 8785 ([`canonical`]).

#![forbid(unsafe_code)]

pub mod canonical;
mod error;
mod parser;
mod value;
mod writer;

pub use error::{JsonParseError, JsonSyntaxError};
pub use parser::{JsonEvent, JsonParserState, MAX_DEPTH, ReaderJsonParser, SliceJsonParser};
pub use value::{Object, Value};
pub use writer::{JsonWriter, escape, write_string, write_value};
