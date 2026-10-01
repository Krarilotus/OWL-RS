//! The serialiser: settings, then statements written to any `Write`.

use std::io::{self, Write};

use nrese_rdf::{GraphNameRef, Iri, Quad, QuadRef, TripleRef};

use crate::format::RdfFormat;
use crate::jsonld::writer::JsonLdWriter;
use crate::jsonld::{FromRdfOptions, from_rdf};
use crate::ntriples;
use crate::rdfxml::RdfXmlWriter;
use crate::turtle::TurtleWriter;

/// How to write a document.
#[derive(Debug, Clone)]
pub struct RdfSerializer {
    format: RdfFormat,
    prefixes: Vec<(String, String)>,
    json_ld_expanded: Option<FromRdfOptions>,
}

impl RdfSerializer {
    pub fn from_format(format: RdfFormat) -> Self {
        Self {
            format,
            prefixes: Vec::new(),
            json_ld_expanded: None,
        }
    }

    /// JSON-LD in expanded form, by the specification's algorithm (Serialize RDF as
    /// JSON-LD): collections as `@list`, and the options' native types. It needs the whole
    /// dataset, so the quads are kept until [`QuadSerializer::finish`]. Without this, JSON-LD
    /// is written as it comes, compacted with the prefixes (the streaming profile).
    pub fn with_json_ld_expanded(mut self, options: FromRdfOptions) -> Self {
        self.json_ld_expanded = Some(options);
        self
    }

    pub fn format(&self) -> RdfFormat {
        self.format
    }

    /// A prefix for IRIs in that namespace (Turtle, TriG, RDF/XML, JSON-LD): `name` empty or a prefix name
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
        let (turtle, rdf_xml) = match self.format {
            RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::N3 => (
                Some(TurtleWriter::new(
                    self.format == RdfFormat::TriG,
                    self.prefixes.clone(),
                )),
                None,
            ),
            RdfFormat::RdfXml => (None, Some(RdfXmlWriter::new(self.prefixes.clone()))),
            _ => (None, None),
        };
        let json_ld = match (self.format, self.json_ld_expanded) {
            (RdfFormat::JsonLd, Some(options)) => Some(JsonLd::Expanded(options, Vec::new())),
            (RdfFormat::JsonLd, None) => Some(JsonLd::Streaming(JsonLdWriter::new(self.prefixes))),
            _ => None,
        };
        QuadSerializer {
            format: self.format,
            writer,
            buffer: Vec::with_capacity(BUFFER),
            turtle,
            rdf_xml,
            json_ld,
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
    rdf_xml: Option<RdfXmlWriter>,
    json_ld: Option<JsonLd>,
}

enum JsonLd {
    Streaming(JsonLdWriter),
    Expanded(FromRdfOptions, Vec<Quad>),
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
            RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::N3 => {
                if let Some(turtle) = &mut self.turtle {
                    turtle.write(&mut self.buffer, quad)?;
                }
            }
            RdfFormat::RdfXml => {
                if let Some(rdf_xml) = &mut self.rdf_xml {
                    rdf_xml.write(&mut self.buffer, quad)?;
                }
            }
            RdfFormat::JsonLd => match &mut self.json_ld {
                Some(JsonLd::Streaming(writer)) => writer.write(&mut self.buffer, quad)?,
                Some(JsonLd::Expanded(_, quads)) => quads.push(quad.into_owned()),
                None => {}
            },
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
        if let Some(rdf_xml) = &mut self.rdf_xml {
            rdf_xml.finish(&mut self.buffer);
        }
        match self.json_ld.take() {
            Some(JsonLd::Streaming(mut writer)) => writer.finish(&mut self.buffer)?,
            Some(JsonLd::Expanded(options, quads)) => {
                let document = from_rdf(quads.iter().map(Quad::as_ref), &options)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                let mut text = String::new();
                nrese_json::write_value(&document, &mut text);
                self.buffer.extend_from_slice(text.as_bytes());
                self.buffer.push(b'\n');
            }
            None => {}
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
