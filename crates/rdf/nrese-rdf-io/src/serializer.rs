//! The serialiser: settings, then statements written to any `Write`.

use std::io::{self, Write};

use nrese_rdf::{QuadRef, TripleRef};

use crate::format::RdfFormat;
use crate::ntriples;

/// How to write a document.
#[derive(Debug, Clone)]
pub struct RdfSerializer {
    format: RdfFormat,
}

impl RdfSerializer {
    pub fn from_format(format: RdfFormat) -> Self {
        Self { format }
    }

    pub fn format(&self) -> RdfFormat {
        self.format
    }

    /// Writes to `writer`, through a buffer of its own (an unbuffered writer is fine).
    pub fn for_writer<W: Write>(self, writer: W) -> QuadSerializer<W> {
        QuadSerializer {
            format: self.format,
            writer,
            buffer: Vec::with_capacity(BUFFER),
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
        self.serialize_quad(
            triple
                .into()
                .in_graph(nrese_rdf::GraphNameRef::DefaultGraph),
        )
    }

    /// Ends the document, and returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
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
