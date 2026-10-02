//! Notation3 (N3, W3C N3 Community Group): reading and writing.
//!
//! N3 is Turtle with formulas (graphs as terms), quick variables and paths, and allows any
//! term in any position. A document reads as [`N3Quad`]s: a formula `{ … }` is a blank
//! node, and the triples inside it are quads in the graph of that blank node (as Oxigraph
//! represents them); the empty formula is `true`. Plain RDF reads through
//! [`crate::RdfParser`] with [`crate::RdfFormat::N3`], which refuses what RDF can't hold.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::{self, Read, Write};

use nrese_rdf::vocab::xsd;
use nrese_rdf::{
    BlankNode, GraphName, Iri, IriParseError, Literal, NamedNode, NamedOrBlankNode, Quad, Term,
    Triple, Variable,
};

use crate::blank::BlankNodes;
use crate::error::RdfParseError;
use crate::turtle::{TurtleParser, TurtleSettings};

/// A term of N3: what RDF has (triple terms too, written as RDF 1.2 writes them), and
/// variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum N3Term {
    NamedNode(NamedNode),
    BlankNode(BlankNode),
    Literal(Literal),
    Triple(Box<Triple>),
    Variable(Variable),
}

/// An N3 statement: any term in any position; in a formula's graph, or the default graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct N3Quad {
    pub subject: N3Term,
    pub predicate: N3Term,
    pub object: N3Term,
    pub graph_name: GraphName,
}

impl fmt::Display for N3Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => n.fmt(f),
            Self::BlankNode(b) => b.fmt(f),
            Self::Literal(l) => l.fmt(f),
            Self::Triple(t) => write!(f, "<<( {t} )>>"),
            Self::Variable(v) => v.fmt(f),
        }
    }
}

impl From<Term> for N3Term {
    fn from(term: Term) -> Self {
        match term {
            Term::NamedNode(n) => Self::NamedNode(n),
            Term::BlankNode(b) => Self::BlankNode(b),
            Term::Literal(l) => Self::Literal(l),
            Term::Triple(t) => Self::Triple(t),
        }
    }
}

impl From<Quad> for N3Quad {
    fn from(quad: Quad) -> Self {
        Self {
            subject: Term::from(quad.subject).into(),
            predicate: N3Term::NamedNode(quad.predicate),
            object: quad.object.into(),
            graph_name: quad.graph_name,
        }
    }
}

impl N3Quad {
    /// The RDF quad, if it is one: a node as subject, an IRI as predicate, no variables.
    pub fn into_quad(self) -> Result<Quad, Box<Self>> {
        let subject = match &self.subject {
            N3Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n.clone()),
            N3Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b.clone()),
            _ => return Err(Box::new(self)),
        };
        let N3Term::NamedNode(predicate) = &self.predicate else {
            return Err(Box::new(self));
        };
        let object = match &self.object {
            N3Term::NamedNode(n) => Term::NamedNode(n.clone()),
            N3Term::BlankNode(b) => Term::BlankNode(b.clone()),
            N3Term::Literal(l) => Term::Literal(l.clone()),
            N3Term::Triple(t) => Term::Triple(t.clone()),
            N3Term::Variable(_) => return Err(Box::new(self)),
        };
        Ok(Quad {
            subject,
            predicate: predicate.clone(),
            object,
            graph_name: self.graph_name,
        })
    }
}

impl fmt::Display for N3Quad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.subject, self.predicate, self.object)?;
        if !self.graph_name.is_default_graph() {
            write!(f, " {}", self.graph_name)?;
        }
        write!(f, " .")
    }
}

/// How to read an N3 document.
#[derive(Debug, Clone)]
pub struct N3Parser {
    base_iri: Option<Iri<String>>,
    blank_nodes: BlankNodes,
    unchecked: bool,
    max_nesting: usize,
}

impl Default for N3Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl N3Parser {
    pub fn new() -> Self {
        Self {
            base_iri: None,
            blank_nodes: BlankNodes::AsWritten,
            unchecked: false,
            max_nesting: 128,
        }
    }

    pub fn with_base_iri(mut self, base_iri: impl Into<String>) -> Result<Self, IriParseError> {
        self.base_iri = Some(Iri::parse(base_iri.into())?);
        Ok(self)
    }

    /// Fresh blank nodes for this document (see [`crate::RdfParser::rename_blank_nodes`]).
    pub fn rename_blank_nodes(mut self) -> Self {
        self.blank_nodes = BlankNodes::fresh();
        self
    }

    /// IRIs taken as written, unchecked.
    pub fn unchecked(mut self) -> Self {
        self.unchecked = true;
        self
    }

    /// How deep formulas, lists and blank node property lists may nest (128 by default).
    pub fn with_max_nesting(mut self, depth: usize) -> Self {
        self.max_nesting = depth;
        self
    }

    fn settings(&self) -> TurtleSettings {
        TurtleSettings {
            trig: false,
            base: self.base_iri.clone(),
            blank_nodes: self.blank_nodes.clone(),
            unchecked: self.unchecked,
            max_depth: self.max_nesting,
            n3: true,
            recover: false,
        }
    }

    pub fn for_slice(self, bytes: &[u8]) -> N3QuadParser<'_, io::Empty> {
        N3QuadParser {
            inner: TurtleParser::from_slice(bytes, self.settings()),
        }
    }

    /// Reads from `reader` (buffered here).
    pub fn for_reader<R: Read>(self, reader: R) -> N3QuadParser<'static, R> {
        N3QuadParser {
            inner: TurtleParser::from_reader(reader, self.settings()),
        }
    }
}

/// The quads of an N3 document.
pub struct N3QuadParser<'a, R: Read> {
    inner: TurtleParser<'a, R>,
}

impl<R: Read> Iterator for N3QuadParser<'_, R> {
    type Item = Result<N3Quad, RdfParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next_n3()
    }
}

/// Writes N3: the quads of the default graph as statements, a blank node that names a
/// graph as the formula `{ … }` of that graph's quads (nested as deep as they are),
/// variables as `?name`. The statements of a subject are written together
/// (`s p o1, o2; q o3`), so a formula that is the subject of several is written once. It needs every quad before it can tell formulas from blank nodes,
/// so it collects them until [`N3Serializer::finish`].
#[derive(Debug, Default)]
pub struct N3Serializer {
    prefixes: Vec<(String, String)>,
    quads: Vec<N3Quad>,
}

impl N3Serializer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A prefix (`name` empty or a prefix name).
    pub fn with_prefix(mut self, name: impl Into<String>, iri: impl Into<String>) -> Self {
        self.prefixes.push((name.into(), iri.into()));
        self
    }

    pub fn serialize_quad(&mut self, quad: impl Into<N3Quad>) {
        self.quads.push(quad.into());
    }

    /// Writes the document.
    pub fn finish<W: Write>(self, mut writer: W) -> io::Result<W> {
        let mut graphs: BTreeMap<&GraphName, Vec<&N3Quad>> = BTreeMap::new();
        for quad in &self.quads {
            graphs.entry(&quad.graph_name).or_default().push(quad);
        }
        let mut out = String::new();
        for (name, iri) in &self.prefixes {
            out.push_str(&format!("@prefix {name}: <{iri}> .\n"));
        }
        let mut writing = HashSet::new();
        let writer_state = Writer {
            graphs: &graphs,
            prefixes: &self.prefixes,
        };
        if let Some(quads) = graphs.get(&GraphName::DefaultGraph) {
            writer_state.statements(quads, &mut out, &mut writing, 0, " .\n");
        }
        // A graph no term of the default graph names (a formula only nested, or a named
        // graph N3 can't hold) is written as a formula on its own.
        for (name, quads) in &graphs {
            let GraphName::BlankNode(b) = name else {
                continue;
            };
            let referenced = self.quads.iter().any(|q| {
                [&q.subject, &q.predicate, &q.object]
                    .iter()
                    .any(|t| matches!(t, N3Term::BlankNode(x) if x == b))
            });
            if !referenced {
                out.push_str("{ ");
                writer_state.statements(quads, &mut out, &mut writing, 1, " . ");
                out.push_str("} .\n");
            }
        }
        if graphs.keys().any(|g| matches!(g, GraphName::NamedNode(_))) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "N3 has no named graphs, only formulas",
            ));
        }
        writer.write_all(out.as_bytes())?;
        writer.flush()?;
        Ok(writer)
    }
}

struct Writer<'a> {
    graphs: &'a BTreeMap<&'a GraphName, Vec<&'a N3Quad>>,
    prefixes: &'a [(String, String)],
}

impl Writer<'_> {
    /// The statements of `quads`, by subject (in the order subjects first appear), each
    /// ended with `end`.
    fn statements(
        &self,
        quads: &[&N3Quad],
        out: &mut String,
        writing: &mut HashSet<BlankNode>,
        depth: usize,
        end: &str,
    ) {
        let mut subjects: Vec<&N3Term> = Vec::new();
        let mut by_subject: BTreeMap<&N3Term, Vec<&N3Quad>> = BTreeMap::new();
        for quad in quads {
            let entry = by_subject.entry(&quad.subject).or_default();
            if entry.is_empty() {
                subjects.push(&quad.subject);
            }
            entry.push(quad);
        }
        for subject in subjects {
            self.term(subject, out, writing, depth);
            let mut previous: Option<&N3Term> = None;
            for quad in &by_subject[subject] {
                if previous == Some(&quad.predicate) {
                    out.push_str(" ,");
                } else {
                    if previous.is_some() {
                        out.push_str(" ;");
                    }
                    out.push(' ');
                    self.term(&quad.predicate, out, writing, depth);
                    previous = Some(&quad.predicate);
                }
                out.push(' ');
                self.term(&quad.object, out, writing, depth);
            }
            out.push_str(end);
        }
    }

    fn term(
        &self,
        term: &N3Term,
        out: &mut String,
        writing: &mut HashSet<BlankNode>,
        depth: usize,
    ) {
        match term {
            N3Term::NamedNode(n) => self.iri(n.as_str(), out),
            N3Term::BlankNode(b) => {
                let name = GraphName::BlankNode(b.clone());
                // A blank node that names a graph is that formula (unless it is being
                // written: a formula can't contain itself).
                match self.graphs.get(&name) {
                    Some(quads) if depth < 4096 && writing.insert(b.clone()) => {
                        out.push_str("{ ");
                        self.statements(quads, out, writing, depth + 1, " . ");
                        out.push('}');
                        writing.remove(b);
                    }
                    _ => out.push_str(&b.to_string()),
                }
            }
            N3Term::Literal(l) => {
                if l.datatype() == xsd::BOOLEAN && matches!(l.value(), "true" | "false") {
                    out.push_str(l.value());
                } else {
                    out.push_str(&l.to_string());
                }
            }
            N3Term::Triple(triple) => self.triple_term(triple, out),
            N3Term::Variable(v) => {
                out.push('?');
                out.push_str(v.as_str());
            }
        }
    }

    /// `<<( s p o )>>`: its blank nodes are nodes, never formulas.
    fn triple_term(&self, triple: &Triple, out: &mut String) {
        out.push_str("<<( ");
        match &triple.subject {
            NamedOrBlankNode::NamedNode(n) => self.iri(n.as_str(), out),
            NamedOrBlankNode::BlankNode(b) => out.push_str(&b.to_string()),
        }
        out.push(' ');
        self.iri(triple.predicate.as_str(), out);
        out.push(' ');
        match &triple.object {
            Term::NamedNode(n) => self.iri(n.as_str(), out),
            Term::Triple(inner) => self.triple_term(inner, out),
            other => out.push_str(&other.to_string()),
        }
        out.push_str(" )>>");
    }

    fn iri(&self, iri: &str, out: &mut String) {
        for (name, namespace) in self.prefixes {
            if let Some(local) = iri.strip_prefix(namespace.as_str())
                && !local.is_empty()
                && local
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && local
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            {
                out.push_str(name);
                out.push(':');
                out.push_str(local);
                return;
            }
        }
        out.push('<');
        out.push_str(iri);
        out.push('>');
    }
}
