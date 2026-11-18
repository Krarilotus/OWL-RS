//! Turtle and TriG (RDF 1.1): one lexer and parser for both (TriG is Turtle with graphs).

// Its `opener!` and `closer!` patterns are the parser's and the skimmer's too.
#[macro_use]
mod lexer;
mod parser;
pub(crate) mod split;
mod writer;

pub(crate) use parser::{TurtleParser, TurtleSettings};
pub(crate) use writer::TurtleWriter;
