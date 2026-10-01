//! The parser: settings, then a document from a slice, a reader, or in chunks for parsing
//! in parallel.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Take};
use std::path::Path;

use memchr::memchr2;
use nrese_rdf::{GraphName, Iri, IriParseError, Quad, QuadRef};

use crate::blank::BlankNodes;
use crate::error::{RdfParseError, RdfSyntaxError, TextPosition};
use crate::format::RdfFormat;
use crate::input::Lines;
use crate::jsonld::JsonLdOptions;
use crate::jsonld::parser::{JsonLdParser, JsonLdSettings};
use crate::ntriples::LineParser;
use crate::rdfxml::{RdfXmlParser, RdfXmlSettings};
use crate::turtle::{TurtleParser, TurtleSettings};

/// How to read a document: its format, and the settings that apply to every format.
#[derive(Debug, Clone)]
pub struct RdfParser {
    format: RdfFormat,
    base_iri: Option<Iri<String>>,
    default_graph: GraphName,
    without_named_graphs: bool,
    blank_nodes: BlankNodes,
    unchecked: bool,
    max_nesting: usize,
    json_ld: JsonLdOptions,
}

/// How deep `[ … ]` and `( … )` may nest in Turtle and TriG by default: far beyond real
/// data (lists are flat), and safe for the stack of any thread, debug builds included.
const MAX_NESTING: usize = 128;

impl RdfParser {
    pub fn from_format(format: RdfFormat) -> Self {
        Self {
            format,
            base_iri: None,
            default_graph: GraphName::DefaultGraph,
            without_named_graphs: false,
            blank_nodes: BlankNodes::AsWritten,
            unchecked: false,
            max_nesting: MAX_NESTING,
            json_ld: JsonLdOptions::default(),
        }
    }

    pub fn format(&self) -> RdfFormat {
        self.format
    }

    /// The base IRI relative IRIs are resolved against (formats that have them).
    pub fn with_base_iri(mut self, base_iri: impl Into<String>) -> Result<Self, IriParseError> {
        self.base_iri = Some(Iri::parse(base_iri.into())?);
        Ok(self)
    }

    /// The graph for statements the document puts in the default graph.
    pub fn with_default_graph(mut self, graph: impl Into<GraphName>) -> Self {
        self.default_graph = graph.into();
        self
    }

    /// A statement in a named graph is an error (for a payload that is one graph).
    pub fn without_named_graphs(mut self) -> Self {
        self.without_named_graphs = true;
        self
    }

    /// Fresh blank nodes for this document: a label means the same node throughout the
    /// document, and another one than in any other document.
    pub fn rename_blank_nodes(mut self) -> Self {
        self.blank_nodes = BlankNodes::fresh();
        self
    }

    /// Takes IRIs as they are written, without checking they are well formed: faster, for
    /// input known to be valid.
    pub fn unchecked(mut self) -> Self {
        self.unchecked = true;
        self
    }

    /// How deep blank node property lists and collections may nest (Turtle, TriG): deeper
    /// input is an error rather than a risk to the stack. 128 by default; parsing is
    /// recursive, so a higher limit needs a thread with a stack to match.
    pub fn with_max_nesting(mut self, depth: usize) -> Self {
        self.max_nesting = depth;
        self
    }

    /// The JSON-LD settings: processing mode, `rdfDirection`, an expand context, and the
    /// loader of remote contexts (none by default).
    pub fn with_json_ld_options(mut self, options: JsonLdOptions) -> Self {
        self.json_ld = options;
        self
    }

    /// Parses `bytes`.
    pub fn for_slice(self, bytes: &[u8]) -> QuadParser<'_, io::Empty> {
        match self.format {
            RdfFormat::JsonLd => {
                let settings = self.json_ld_settings();
                self.wrap(Inner::JsonLd(Box::new(JsonLdParser::new(
                    Cow::Borrowed(bytes),
                    settings,
                ))))
            }
            RdfFormat::Turtle | RdfFormat::TriG => {
                let settings = self.turtle_settings();
                self.wrap(Inner::Turtle(TurtleParser::from_slice(bytes, settings)))
            }
            RdfFormat::RdfXml => {
                let settings = self.rdf_xml_settings();
                self.wrap(Inner::RdfXmlSlice(RdfXmlParser::new(bytes, settings)))
            }
            _ => self.parser(Lines::from_slice(bytes, 0)),
        }
    }

    /// Parses what `reader` gives (buffered here: an unbuffered reader is fine).
    pub fn for_reader<R: Read>(self, reader: R) -> QuadParser<'static, R> {
        match self.format {
            RdfFormat::JsonLd => {
                let settings = self.json_ld_settings();
                self.wrap(Inner::JsonLdReader(Some((reader, settings)), None))
            }
            RdfFormat::Turtle | RdfFormat::TriG => {
                let settings = self.turtle_settings();
                self.wrap(Inner::Turtle(TurtleParser::from_reader(reader, settings)))
            }
            RdfFormat::RdfXml => {
                let settings = self.rdf_xml_settings();
                self.wrap(Inner::RdfXmlReader(RdfXmlParser::new(
                    BufReader::new(reader),
                    settings,
                )))
            }
            _ => self.parser(Lines::from_reader(reader, 0)),
        }
    }

    fn turtle_settings(&self) -> TurtleSettings {
        TurtleSettings {
            trig: self.format == RdfFormat::TriG,
            base: self.base_iri.clone(),
            blank_nodes: self.blank_nodes.clone(),
            unchecked: self.unchecked,
            max_depth: self.max_nesting,
        }
    }

    fn json_ld_settings(&self) -> JsonLdSettings {
        JsonLdSettings {
            base: self.base_iri.clone(),
            blank_nodes: self.blank_nodes.clone(),
            unchecked: self.unchecked,
            max_depth: self.max_nesting,
            options: self.json_ld.clone(),
        }
    }

    fn rdf_xml_settings(&self) -> RdfXmlSettings {
        RdfXmlSettings {
            base: self.base_iri.clone(),
            blank_nodes: self.blank_nodes.clone(),
            unchecked: self.unchecked,
        }
    }

    fn wrap<'a, R: Read>(self, inner: Inner<'a, R>) -> QuadParser<'a, R> {
        QuadParser {
            inner,
            default_graph: self.default_graph,
            without_named_graphs: self.without_named_graphs,
        }
    }

    /// `bytes` in up to `parts` chunks that parse independently (cut at line ends), for
    /// N-Triples and N-Quads; with renamed blank nodes, every chunk names a label alike.
    pub fn split_slice_for_parallel_parsing(
        self,
        bytes: &[u8],
        parts: usize,
    ) -> Result<Vec<QuadParser<'_, io::Empty>>, RdfParseError> {
        self.line_based()?;
        let bounds = boundaries(bytes.len() as u64, parts, |at| {
            Ok(memchr2(b'\n', b'\r', &bytes[at as usize..])
                .map_or(bytes.len() as u64, |i| at + i as u64 + 1))
        })?;
        Ok(bounds
            .windows(2)
            .map(|w| {
                let (start, end) = (w[0] as usize, w[1] as usize);
                self.clone()
                    .parser(Lines::from_slice(&bytes[start..end], w[0]))
            })
            .collect())
    }

    /// The file at `path` in up to `parts` chunks that parse independently, each read
    /// through its own handle (see [`RdfParser::split_slice_for_parallel_parsing`]).
    pub fn split_file_for_parallel_parsing(
        self,
        path: &Path,
        parts: usize,
    ) -> Result<Vec<QuadParser<'static, Take<BufReader<File>>>>, RdfParseError> {
        self.line_based()?;
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut probe = [0_u8; 4096];
        let bounds = boundaries(length, parts, |at| {
            // The first line end at or after `at`.
            file.seek(SeekFrom::Start(at))?;
            let mut position = at;
            loop {
                let read = file.read(&mut probe)?;
                if read == 0 {
                    return Ok(length);
                }
                if let Some(i) = memchr2(b'\n', b'\r', &probe[..read]) {
                    return Ok(position + i as u64 + 1);
                }
                position += read as u64;
            }
        })?;
        bounds
            .windows(2)
            .map(|w| {
                let mut file = File::open(path)?;
                file.seek(SeekFrom::Start(w[0]))?;
                let reader = BufReader::with_capacity(256 * 1024, file).take(w[1] - w[0]);
                Ok(self.clone().parser(Lines::from_reader(reader, w[0])))
            })
            .collect()
    }

    fn line_based(&self) -> Result<(), RdfParseError> {
        match self.format {
            RdfFormat::NTriples | RdfFormat::NQuads => Ok(()),
            other => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{other} can't be split for parallel parsing"),
            )
            .into()),
        }
    }

    fn parser<'a, R: Read>(self, lines: Lines<'a, R>) -> QuadParser<'a, R> {
        let inner = match self.format {
            RdfFormat::NTriples | RdfFormat::NQuads => Inner::Lines {
                lines,
                parser: LineParser::new(
                    self.format == RdfFormat::NQuads,
                    self.unchecked,
                    self.blank_nodes.clone(),
                ),
            },
            other => Inner::Unsupported(Some(other)),
        };
        self.wrap(inner)
    }
}

/// Chunk bounds: `0`, a line end at or after each `length × k / parts`, then `length`.
fn boundaries(
    length: u64,
    parts: usize,
    mut line_end_from: impl FnMut(u64) -> io::Result<u64>,
) -> io::Result<Vec<u64>> {
    let parts = parts.max(1) as u64;
    let mut bounds = vec![0];
    for k in 1..parts {
        let nominal = length * k / parts;
        let last = *bounds.last().unwrap_or(&0);
        if nominal <= last {
            continue;
        }
        let at = line_end_from(nominal)?;
        if at > last && at < length {
            bounds.push(at);
        }
    }
    bounds.push(length);
    Ok(bounds)
}

// One per parser, never in a collection: boxing the large variant would only add an
// indirection on the hot path.
#[allow(clippy::large_enum_variant)]
enum Inner<'a, R: Read> {
    Lines {
        lines: Lines<'a, R>,
        parser: LineParser,
    },
    Turtle(TurtleParser<'a, R>),
    RdfXmlSlice(RdfXmlParser<&'a [u8]>),
    RdfXmlReader(RdfXmlParser<BufReader<R>>),
    JsonLd(Box<JsonLdParser<'a>>),
    /// JSON-LD from a reader: read whole at the first call (see [`crate::jsonld`]).
    JsonLdReader(
        Option<(R, JsonLdSettings)>,
        Option<Box<JsonLdParser<'static>>>,
    ),
    /// A format this crate doesn't read yet: one error, then the end.
    Unsupported(Option<RdfFormat>),
}

/// The quads of one document. [`QuadParser::next_ref`] borrows each quad from the parser's
/// buffers (no allocation per term); as an `Iterator` it gives owned quads.
pub struct QuadParser<'a, R: Read> {
    inner: Inner<'a, R>,
    default_graph: GraphName,
    without_named_graphs: bool,
}

impl<R: Read> QuadParser<'_, R> {
    /// The next quad, borrowed until the next call; `None` at the end.
    pub fn next_ref(&mut self) -> Option<Result<QuadRef<'_>, RdfParseError>> {
        let Self {
            inner,
            default_graph,
            without_named_graphs,
        } = self;
        let (quad, at) = match inner {
            Inner::Unsupported(format) => {
                let format = format.take()?;
                return Some(Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("reading {format} is not implemented yet"),
                )
                .into()));
            }
            Inner::RdfXmlSlice(parser) => match parser.advance() {
                Ok(true) => (parser.current.as_ref()?.as_ref(), TextPosition::default()),
                Ok(false) => return None,
                Err(error) => return Some(Err(error)),
            },
            Inner::RdfXmlReader(parser) => match parser.advance() {
                Ok(true) => (parser.current.as_ref()?.as_ref(), TextPosition::default()),
                Ok(false) => return None,
                Err(error) => return Some(Err(error)),
            },
            Inner::JsonLd(parser) => match parser.next_ref()? {
                Ok(quad) => (quad, TextPosition::default()),
                Err(error) => return Some(Err(error)),
            },
            Inner::JsonLdReader(pending, parser) => {
                if let Some((mut reader, settings)) = pending.take() {
                    let mut bytes = Vec::new();
                    if let Err(error) = reader.read_to_end(&mut bytes) {
                        return Some(Err(error.into()));
                    }
                    *parser = Some(Box::new(JsonLdParser::new(Cow::Owned(bytes), settings)));
                }
                match parser.as_mut()?.next_ref()? {
                    Ok(quad) => (quad, TextPosition::default()),
                    Err(error) => return Some(Err(error)),
                }
            }
            Inner::Turtle(parser) => match parser.next_ref()? {
                Ok(quad) => (quad, TextPosition::default()),
                Err(error) => return Some(Err(error)),
            },
            Inner::Lines { lines, parser } => {
                // Find the next line with a statement without holding a borrow.
                let range = loop {
                    match lines.next() {
                        Ok(Some(range)) => {
                            if has_statement(lines.slice(range.clone())) {
                                break range;
                            }
                        }
                        Ok(None) => return None,
                        Err(error) => return Some(Err(error.into())),
                    }
                };
                let (line, offset) = lines.position();
                match parser.parse(lines.slice(range), line, offset) {
                    Ok(Some(quad)) => (
                        quad,
                        TextPosition {
                            line,
                            column: 0,
                            offset,
                        },
                    ),
                    Ok(None) => unreachable!("a line with a statement"),
                    Err(error) => return Some(Err(error.into())),
                }
            }
        };
        Some(place(quad, default_graph, *without_named_graphs, at))
    }
}

/// The quad in the graph the settings say.
fn place<'b>(
    mut quad: QuadRef<'b>,
    default_graph: &'b GraphName,
    without_named_graphs: bool,
    at: TextPosition,
) -> Result<QuadRef<'b>, RdfParseError> {
    if quad.graph_name.is_default_graph() {
        quad.graph_name = default_graph.as_ref();
    } else if without_named_graphs {
        return Err(RdfSyntaxError::new(
            format!("a statement in the named graph {}", quad.graph_name),
            at..at,
        )
        .into());
    }
    Ok(quad)
}

/// Whether a line holds a statement (not only whitespace or a comment).
fn has_statement(line: &[u8]) -> bool {
    match line.iter().find(|&&b| b != b' ' && b != b'\t') {
        None | Some(b'#') => false,
        Some(_) => true,
    }
}

impl<R: Read> Iterator for QuadParser<'_, R> {
    type Item = Result<Quad, RdfParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.next_ref()?.map(QuadRef::into_owned))
    }
}
