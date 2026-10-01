//! NRESE's SPARQL query results formats (SPARQL 1.2 Query Results JSON, XML, CSV and TSV):
//! streaming readers and writers.
//!
//! Part of the RDF bundle (`crates/rdf`), which replaces the Oxigraph crates: the API
//! follows `sparesults` (`QueryResultsParser`, `QueryResultsSerializer`, `QuerySolution`)
//! so that switching is a change of paths.
//!
//! - **Reading** JSON, XML and TSV, from memory or any `Read`, one solution at a time.
//!   CSV can't be read back: it doesn't say which values are IRIs, blank nodes or
//!   literals (the specification calls it lossy).
//! - **Writing** all four, a buffer of bytes at a time, with the same bytes as
//!   `sparesults` (and NRESE's native result writer) for RDF 1.1 terms.
//! - **SPARQL 1.2**: triple terms (`"type": "triple"`, `<triple>`, `<<( s p o )>>`) and
//!   literals with a base direction (`its:dir`, `@ar--rtl`), read and written. Where
//!   `sparesults` 0.3 writes a triple term in CSV as `s p o`, this writes what the
//!   specification gives, `<<( s p o )>>` (quoted as CSV needs).

#![forbid(unsafe_code)]

mod csv;
mod error;
mod format;
mod json;
mod parser;
mod serializer;
mod solution;
mod tsv;
mod xml;

pub use error::{QueryResultsParseError, QueryResultsSyntaxError, TextPosition};
pub use format::QueryResultsFormat;
pub use parser::{
    QueryResultsParser, ReaderQueryResultsParserOutput, ReaderSolutionsParser,
    SliceQueryResultsParserOutput, SliceSolutionsParser,
};
pub use serializer::{QueryResultsSerializer, WriterSolutionsSerializer};
pub use solution::QuerySolution;
