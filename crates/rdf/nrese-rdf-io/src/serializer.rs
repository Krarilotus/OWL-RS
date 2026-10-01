//! The serialiser: settings, then statements written to any `Write`.

use std::io::{self, Write};

use nrese_rdf::{GraphNameRef, Iri, QuadRef, TripleRef};

use crate::format::RdfFormat;
use crate::ntriples;
use crate::turtle::TurtleWriter;

/// How to write a document.
#[derive(Debug, Clone)]
pub struct RdfSerializer {
    format: RdfFormat,
    prefixes: Vec<(String, String)>,
}

impl RdfSerializer {
    pub fn from_format(format: RdfFormat) -> Self {
        Self {
            format,
            prefixes: Vec::new(),
        }
    }

    pub fn format(&self) -> RdfFormat {
        self.format
    }

    /// A prefix for IRIs in that namespace (Turtle, TriG): `name` empty or a prefix name
    /// (`PN_PREFIX`), `iri` absolute. Formats without prefixes ignore it.
    pub fn with_prefix(
        mut self,
        name: impl Into<String>,
        iri: impl Into<String>,
    ) -> io::Result<Self> {
        let (name, iri) = (name.into(), iri.into());
        let valid_name = name.is_empty()
            || (name.starts_with(|c: char| c.is_ascii_alphabetic())
                && !name.ends_with('.')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')));
        if !valid_name {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("an invalid prefix name: {name}"),
            ));
        }
        Iri::parse(iri.as_str()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        self.prefixes.retain(|(n, _)| *n != name);
        self.prefixes.push((name, iri));
        Ok(self)
    }

    /// Writes to `writer`, through a buffer of its own (an unbuffered writer is fine).
    pub fn for_writer<W: Write>(self, writer: W) -> QuadSerializer<W> {
        let turtle = match self.format {
            RdfFormat::Turtle | RdfFormat::TriG => Some(TurtleWriter::new(
                self.format == RdfFormat::TriG,
                self.prefixes,
            )),
            _ => None,
        };
        QuadSerializer {
            format: self.format,
            writer,
            buffer: Vec::with_capacity(BUFFER),
            turtle,
        }
    }
}

/// Bytes collected before they go to the writer.
const BUFFER: usize = 64 * 1024;

/// Writes statements in one format. Call [`QuadSerializer::finish`] at the end.
pub struct QuadSerializer<W: Write> {
    format: RdfFormat,
    writer: W,
    buffer: Vec<u8>,
    turtle: Option<TurtleWriter>,
}

impl<W: Write> QuadSerializer<W> {
    /// Writes a quad. A quad in a named graph is an error for a format without graphs.
    pub fn serialize_quad<'a>(&mut self, quad: impl Into<QuadRef<'a>>) -> io::Result<()> {
        let quad = quad.into();
        match self.format {
            RdfFormat::NQuads => ntriples::write_quad(&mut self.buffer, quad)?,
            RdfFormat::NTriples => {
                if !quad.graph_name.is_default_graph() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("N-Triples has no named graphs: {quad}"),
                    ));
                }
                ntriples::write_triple(&mut self.buffer, quad.into())?;
            }
            RdfFormat::Turtle | RdfFormat::TriG => {
                if let Some(turtle) = &mut self.turtle {
                    turtle.write(&mut self.buffer, quad)?;
                }
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("writing {other} is not implemented yet"),
                ));
            }
        }
        self.flush_full()
    }

    /// Writes a triple (in the default graph).
    pub fn serialize_triple<'a>(&mut self, triple: impl Into<TripleRef<'a>>) -> io::Result<()> {
        self.serialize_quad(triple.into().in_graph(GraphNameRef::DefaultGraph))
    }

    /// Ends the document, and returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        if let Some(turtle) = &mut self.turtle {
            turtle.finish(&mut self.buffer);
        }
        self.writer.write_all(&self.buffer)?;
        self.buffer.clear();
        self.writer.flush()?;
        Ok(self.writer)
    }

    fn flush_full(&mut self) -> io::Result<()> {
        if self.buffer.len() >= BUFFER {
            self.writer.write_all(&self.buffer)?;
            self.buffer.clear();
        }
        Ok(())
    }
}
