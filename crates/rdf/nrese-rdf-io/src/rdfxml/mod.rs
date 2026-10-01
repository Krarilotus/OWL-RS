//! RDF/XML (RDF 1.1 XML Syntax).

mod parser;
mod writer;

pub(crate) use parser::{RdfXmlParser, RdfXmlSettings};
pub(crate) use writer::RdfXmlWriter;
