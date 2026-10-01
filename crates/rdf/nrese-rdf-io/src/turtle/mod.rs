//! Turtle and TriG (RDF 1.1): one lexer and parser for both (TriG is Turtle with graphs).

mod lexer;
mod parser;
mod writer;

pub(crate) use parser::{TurtleParser, TurtleSettings};
pub(crate) use writer::TurtleWriter;
