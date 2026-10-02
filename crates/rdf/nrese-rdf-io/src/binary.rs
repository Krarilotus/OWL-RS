//! RDF4J's Binary RDF format (`application/x-binary-rdf`): what RDF4J's HTTP client sends
//! when it uploads statements and asks for when it reads them. A stream of records after a
//! header:
//!
//! ```text
//! "BRDF" | version: i32 (big-endian, 1 or 2) | [version 2: charset name: string]
//! record*: type byte, then
//!   0 namespace  prefix: string, namespace: string
//!   1 statement  subject, predicate, object, context (each a value; context may be null)
//!   2 comment    text: string
//!   3 value decl id, value (later referred to by id)
//!   127 end of data
//! value: type byte, then
//!   0 null | 1 IRI string | 2 blank node label | 3 plain literal label
//!   4 language literal label, tag | 5 typed literal label, datatype IRI
//!   6 reference id | 7 triple term subject, predicate, object (values)
//! version 1: ids are i32, strings an i32 count of UTF-16 code units and UTF-16BE bytes;
//! version 2: ids are unsigned varints (7 bits a byte, low first), strings a varint byte
//! count and bytes in the declared charset.
//! ```
//!
//! The reader takes both versions (UTF-8 and UTF-16 charsets); the writer writes version 2
//! in UTF-8, declaring each IRI and blank node on its first use and referring to it after
//! (a bounded table), as RDF4J's own writer does for repeated values.

use std::collections::HashMap;
use std::io::{self, Read};

use nrese_rdf::{
    BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, QuadRef, Term, TermRef,
    Triple,
};

use crate::blank::BlankNodes;
use crate::error::{RdfParseError, RdfSyntaxError, TextPosition};

const MAGIC: &[u8; 4] = b"BRDF";
const NAMESPACE_DECL: u8 = 0;
const STATEMENT: u8 = 1;
const COMMENT: u8 = 2;
const VALUE_DECL: u8 = 3;
const END_OF_DATA: u8 = 127;
const NULL_VALUE: u8 = 0;
const URI_VALUE: u8 = 1;
const BNODE_VALUE: u8 = 2;
const PLAIN_LITERAL_VALUE: u8 = 3;
const LANG_LITERAL_VALUE: u8 = 4;
const DATATYPE_LITERAL_VALUE: u8 = 5;
const VALUE_REF: u8 = 6;
const TRIPLE_VALUE: u8 = 7;

/// Values a document may declare: more is an error rather than unbounded memory.
const MAX_DECLARED: usize = 1 << 26;
/// The longest string read (a literal of 1 GiB).
const MAX_STRING: usize = 1 << 30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Charset {
    Utf8,
    Utf16Be,
}

/// Reads a Binary RDF document from a reader (buffer it: reads are small).
pub(crate) struct BinaryParser<R: Read> {
    reader: R,
    /// Bytes read so far, for error positions.
    offset: u64,
    version: Option<i32>,
    charset: Charset,
    declared: Vec<Option<Term>>,
    blank_nodes: BlankNodes,
    unchecked: bool,
    pub(crate) current: Option<Quad>,
    done: bool,
}

impl<R: Read> BinaryParser<R> {
    pub(crate) fn new(reader: R, blank_nodes: BlankNodes, unchecked: bool) -> Self {
        Self {
            reader,
            offset: 0,
            version: None,
            charset: Charset::Utf8,
            declared: Vec::new(),
            blank_nodes,
            unchecked,
            current: None,
            done: false,
        }
    }

    fn error(&self, message: impl Into<String>) -> RdfParseError {
        let at = TextPosition {
            line: 0,
            column: 0,
            offset: self.offset,
        };
        RdfSyntaxError::new(message, at..at).into()
    }

    fn bytes(&mut self, n: usize) -> Result<Vec<u8>, RdfParseError> {
        let mut buffer = vec![0; n];
        self.reader.read_exact(&mut buffer).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                self.error("the document ends in the middle of a record")
            } else {
                error.into()
            }
        })?;
        self.offset += n as u64;
        Ok(buffer)
    }

    fn byte(&mut self) -> Result<u8, RdfParseError> {
        Ok(self.bytes(1)?[0])
    }

    fn int(&mut self) -> Result<i32, RdfParseError> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_be_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn varint(&mut self) -> Result<u32, RdfParseError> {
        let mut value: u32 = 0;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(self.error("a variable-length integer longer than 32 bits"))
    }

    fn id(&mut self) -> Result<usize, RdfParseError> {
        let id = match self.version {
            Some(1) => {
                let id = self.int()?;
                usize::try_from(id).map_err(|_| self.error(format!("a negative value id {id}")))?
            }
            _ => self.varint()? as usize,
        };
        if id >= MAX_DECLARED {
            return Err(self.error(format!("value id {id} beyond {MAX_DECLARED}")));
        }
        Ok(id)
    }

    fn string(&mut self) -> Result<String, RdfParseError> {
        let (length, charset) = match self.version {
            // A count of UTF-16 code units.
            Some(1) => {
                let units = self.int()?;
                let units = usize::try_from(units)
                    .map_err(|_| self.error(format!("a negative string length {units}")))?;
                (units * 2, Charset::Utf16Be)
            }
            _ => (self.varint()? as usize, self.charset),
        };
        if length > MAX_STRING {
            return Err(self.error(format!("a string of {length} bytes")));
        }
        let bytes = self.bytes(length)?;
        match charset {
            Charset::Utf8 => {
                String::from_utf8(bytes).map_err(|_| self.error("a string that isn't UTF-8"))
            }
            Charset::Utf16Be => {
                if bytes.len() % 2 != 0 {
                    return Err(self.error("a UTF-16 string of an odd number of bytes"));
                }
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16(&units).map_err(|_| self.error("a string that isn't UTF-16"))
            }
        }
    }

    fn header(&mut self) -> Result<(), RdfParseError> {
        let magic = self.bytes(4)?;
        if magic != MAGIC {
            return Err(self.error("not a Binary RDF document (no BRDF magic number)"));
        }
        let version = self.int()?;
        self.version = Some(version);
        match version {
            1 => self.charset = Charset::Utf16Be,
            2 => {
                let name = self.string()?;
                self.charset = match name.to_ascii_uppercase().replace('_', "-").as_str() {
                    "UTF-8" | "UTF8" => Charset::Utf8,
                    "UTF-16BE" | "UTF-16" => Charset::Utf16Be,
                    other => return Err(self.error(format!("the charset {other} isn't supported"))),
                };
            }
            other => return Err(self.error(format!("Binary RDF version {other} isn't supported"))),
        }
        Ok(())
    }

    fn iri(&self, text: String) -> Result<NamedNode, RdfParseError> {
        if self.unchecked {
            return Ok(NamedNode::new_unchecked(text));
        }
        NamedNode::new(text).map_err(|error| self.error(error.to_string()))
    }

    fn value(&mut self, depth: usize) -> Result<Option<Term>, RdfParseError> {
        if depth > 64 {
            return Err(self.error("triple terms nested too deep"));
        }
        Ok(Some(match self.byte()? {
            NULL_VALUE => return Ok(None),
            VALUE_REF => {
                let id = self.id()?;
                return match self.declared.get(id) {
                    Some(Some(term)) => Ok(Some(term.clone())),
                    _ => Err(self.error(format!("a reference to value {id}, never declared"))),
                };
            }
            URI_VALUE => {
                let text = self.string()?;
                self.iri(text)?.into()
            }
            BNODE_VALUE => {
                let label = self.string()?;
                let mut fresh = String::new();
                let name = self.blank_nodes.name(&label, &mut fresh);
                BlankNode::new_unchecked(name).into()
            }
            PLAIN_LITERAL_VALUE => Literal::new_simple_literal(self.string()?).into(),
            LANG_LITERAL_VALUE => {
                let label = self.string()?;
                let language = self.string()?;
                Literal::new_language_tagged_literal(label, language)
                    .map_err(|error| self.error(error.to_string()))?
                    .into()
            }
            DATATYPE_LITERAL_VALUE => {
                let label = self.string()?;
                let datatype = self.string()?;
                Literal::new_typed_literal(label, self.iri(datatype)?).into()
            }
            TRIPLE_VALUE => {
                let subject = self.subject(depth + 1)?;
                let predicate = self.predicate(depth + 1)?;
                let object = self
                    .value(depth + 1)?
                    .ok_or_else(|| self.error("a triple term without an object"))?;
                Term::Triple(Box::new(Triple::new(subject, predicate, object)))
            }
            other => return Err(self.error(format!("unknown value type {other}"))),
        }))
    }

    fn subject(&mut self, depth: usize) -> Result<NamedOrBlankNode, RdfParseError> {
        match self.value(depth)? {
            Some(Term::NamedNode(node)) => Ok(node.into()),
            Some(Term::BlankNode(node)) => Ok(node.into()),
            other => Err(self.error(format!("a subject that is no IRI or blank node: {other:?}"))),
        }
    }

    fn predicate(&mut self, depth: usize) -> Result<NamedNode, RdfParseError> {
        match self.value(depth)? {
            Some(Term::NamedNode(node)) => Ok(node),
            other => Err(self.error(format!("a predicate that is no IRI: {other:?}"))),
        }
    }

    /// Reads up to the next statement: `true` with it in `current`, `false` at the end.
    pub(crate) fn advance(&mut self) -> Result<bool, RdfParseError> {
        if self.done {
            return Ok(false);
        }
        if self.version.is_none() {
            if let Err(error) = self.header() {
                self.done = true;
                return Err(error);
            }
        }
        let result = self.record();
        if !matches!(result, Ok(true)) {
            self.done = true;
        }
        result
    }

    fn record(&mut self) -> Result<bool, RdfParseError> {
        loop {
            match self.byte()? {
                END_OF_DATA => return Ok(false),
                STATEMENT => {
                    let subject = self.subject(0)?;
                    let predicate = self.predicate(0)?;
                    let object = self
                        .value(0)?
                        .ok_or_else(|| self.error("a statement without an object"))?;
                    let graph = match self.value(0)? {
                        None => GraphName::DefaultGraph,
                        Some(Term::NamedNode(node)) => GraphName::NamedNode(node),
                        Some(Term::BlankNode(node)) => GraphName::BlankNode(node),
                        Some(other) => {
                            return Err(self.error(format!(
                                "a context that is no IRI or blank node: {other}"
                            )));
                        }
                    };
                    self.current = Some(Quad::new(subject, predicate, object, graph));
                    return Ok(true);
                }
                VALUE_DECL => {
                    let id = self.id()?;
                    let value = self.value(0)?;
                    if self.declared.len() <= id {
                        self.declared.resize(id + 1, None);
                    }
                    self.declared[id] = value;
                }
                NAMESPACE_DECL => {
                    self.string()?;
                    self.string()?;
                }
                COMMENT => {
                    self.string()?;
                }
                other => return Err(self.error(format!("unknown record type {other}"))),
            }
        }
    }
}

/// Values the writer refers to by id: the most it keeps at once.
const WRITER_TABLE: usize = 1 << 16;

/// Writes Binary RDF, version 2 in UTF-8.
pub(crate) struct BinaryWriter {
    started: bool,
    ids: HashMap<Term, u32>,
    /// Where the record being written starts in the buffer: declarations go before it.
    record_start: usize,
}

fn varint(out: &mut Vec<u8>, mut value: u32) {
    while value > 0x7f {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn string(out: &mut Vec<u8>, text: &str) {
    varint(out, text.len() as u32);
    out.extend_from_slice(text.as_bytes());
}

impl BinaryWriter {
    pub(crate) fn new() -> Self {
        Self {
            started: false,
            ids: HashMap::new(),
            record_start: 0,
        }
    }

    fn start(&mut self, out: &mut Vec<u8>) {
        if !self.started {
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&2_i32.to_be_bytes());
            string(out, "UTF-8");
            self.started = true;
        }
    }

    /// Writes `term`: by reference where declared before, declaring an IRI or blank node
    /// on its first use while the table has room.
    fn term(&mut self, out: &mut Vec<u8>, term: TermRef<'_>) -> io::Result<()> {
        let repeatable = matches!(term, TermRef::NamedNode(_) | TermRef::BlankNode(_));
        if repeatable {
            let owned = term.into_owned();
            if let Some(&id) = self.ids.get(&owned) {
                out.push(VALUE_REF);
                varint(out, id);
                return Ok(());
            }
            if self.ids.len() < WRITER_TABLE {
                let id = self.ids.len() as u32;
                // The declaration goes before the record that uses it.
                let mut declaration = vec![VALUE_DECL];
                varint(&mut declaration, id);
                write_value(&mut declaration, term)?;
                out.splice(
                    self.record_start..self.record_start,
                    declaration.iter().copied(),
                );
                self.record_start += declaration.len();
                self.ids.insert(owned, id);
                out.push(VALUE_REF);
                varint(out, id);
                return Ok(());
            }
        }
        write_value(out, term)
    }

    pub(crate) fn write(&mut self, out: &mut Vec<u8>, quad: QuadRef<'_>) -> io::Result<()> {
        self.start(out);
        self.record_start = out.len();
        out.push(STATEMENT);
        self.term(out, quad.subject.into())?;
        self.term(out, quad.predicate.into())?;
        self.term(out, quad.object)?;
        match quad.graph_name {
            nrese_rdf::GraphNameRef::DefaultGraph => out.push(NULL_VALUE),
            nrese_rdf::GraphNameRef::NamedNode(node) => self.term(out, node.into())?,
            nrese_rdf::GraphNameRef::BlankNode(node) => self.term(out, node.into())?,
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self, out: &mut Vec<u8>) {
        self.start(out);
        out.push(END_OF_DATA);
    }
}

/// A value written in full.
fn write_value(out: &mut Vec<u8>, term: TermRef<'_>) -> io::Result<()> {
    match term {
        TermRef::NamedNode(node) => {
            out.push(URI_VALUE);
            string(out, node.as_str());
        }
        TermRef::BlankNode(node) => {
            out.push(BNODE_VALUE);
            string(out, node.as_str());
        }
        TermRef::Literal(literal) => {
            if literal.direction().is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Binary RDF has no base directions: {literal}"),
                ));
            }
            if let Some(language) = literal.language() {
                out.push(LANG_LITERAL_VALUE);
                string(out, literal.value());
                string(out, language);
            } else if literal.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#string" {
                out.push(PLAIN_LITERAL_VALUE);
                string(out, literal.value());
            } else {
                out.push(DATATYPE_LITERAL_VALUE);
                string(out, literal.value());
                string(out, literal.datatype().as_str());
            }
        }
        TermRef::Triple(triple) => {
            out.push(TRIPLE_VALUE);
            write_value(out, triple.subject.as_ref().into())?;
            write_value(out, triple.predicate.as_ref().into())?;
            write_value(out, triple.object.as_ref())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RdfFormat, RdfParser, RdfSerializer};

    fn quads() -> Vec<Quad> {
        let ex = |l: &str| NamedNode::new_unchecked(format!("http://example.com/{l}"));
        let triple = Triple::new(ex("a"), ex("p"), Literal::new_simple_literal("x"));
        vec![
            Quad::new(ex("a"), ex("p"), ex("b"), GraphName::DefaultGraph),
            Quad::new(
                ex("a"),
                ex("p"),
                Literal::new_simple_literal("ünïcödé 𝄞"),
                ex("g"),
            ),
            Quad::new(
                BlankNode::new_unchecked("n1"),
                ex("q"),
                Literal::new_language_tagged_literal("hallo", "de-AT").unwrap(),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                ex("a"),
                ex("q"),
                Literal::new_typed_literal(
                    "7",
                    NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer"),
                ),
                GraphName::BlankNode(BlankNode::new_unchecked("g2")),
            ),
            Quad::new(
                ex("r"),
                ex("reifies"),
                Term::Triple(Box::new(triple)),
                GraphName::DefaultGraph,
            ),
        ]
    }

    #[test]
    fn written_documents_read_back() {
        let mut serializer =
            RdfSerializer::from_format(RdfFormat::BinaryRdf).for_writer(Vec::new());
        for quad in quads() {
            serializer.serialize_quad(&quad).unwrap();
        }
        let bytes = serializer.finish().unwrap();
        assert_eq!(&bytes[..4], b"BRDF");
        let read: Vec<Quad> = RdfParser::from_format(RdfFormat::BinaryRdf)
            .for_slice(&bytes)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(read, quads());
        let streamed: Vec<Quad> = RdfParser::from_format(RdfFormat::BinaryRdf)
            .for_reader(bytes.as_slice())
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(streamed, quads());
        // Repeated IRIs are declared once and referred to.
        let iri = b"http://example.com/a";
        let count = bytes.windows(iri.len()).filter(|w| w == iri).count();
        assert_eq!(count, 2, "once declared, once inside the triple term");
        // Cut short: an error, not a panic.
        let cut = RdfParser::from_format(RdfFormat::BinaryRdf)
            .for_slice(&bytes[..bytes.len() - 5])
            .collect::<Result<Vec<_>, _>>();
        assert!(cut.is_err());
    }

    /// A version 1 document as RDF4J 2 wrote it: i32 ids, UTF-16 strings.
    #[test]
    fn version_1_documents_are_read() {
        let mut bytes = b"BRDF".to_vec();
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        let utf16 = |out: &mut Vec<u8>, text: &str| {
            let units: Vec<u16> = text.encode_utf16().collect();
            out.extend_from_slice(&(units.len() as i32).to_be_bytes());
            for unit in units {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        };
        bytes.push(VALUE_DECL);
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        bytes.push(URI_VALUE);
        utf16(&mut bytes, "http://example.com/p");
        bytes.push(STATEMENT);
        bytes.push(URI_VALUE);
        utf16(&mut bytes, "http://example.com/s");
        bytes.push(VALUE_REF);
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        bytes.push(PLAIN_LITERAL_VALUE);
        utf16(&mut bytes, "grüße");
        bytes.push(NULL_VALUE);
        bytes.push(END_OF_DATA);
        let read: Vec<Quad> = RdfParser::from_format(RdfFormat::BinaryRdf)
            .for_slice(&bytes)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(
            read[0].to_string(),
            "<http://example.com/s> <http://example.com/p> \"grüße\""
        );
    }
}
