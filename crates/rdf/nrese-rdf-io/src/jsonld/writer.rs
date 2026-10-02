//! The streaming JSON-LD writer: a document in the streaming profile (JSON-LD 1.1
//! Streaming), written as the quads come.
//!
//! `{"@context": {prefixes}, "@graph": [node objects]}`, a named graph as
//! `{"@id": name, "@graph": [...]}` among them. The statements of one subject in a row are
//! one node object, buffered until the subject changes so that it is written in the
//! profile's order (`@id`, then `@type`, then the properties, each once). A subject that
//! comes back later gets another node object: JSON-LD merges them. IRIs are compacted with
//! the prefixes; plain strings are JSON strings.
//!
//! An IRI whose scheme is a prefix's name (`ex:thing` with a prefix `ex`) would be read
//! as a compact IRI. The node object (or named graph) that holds one turns that prefix off
//! in a context of its own (`"@context": {"ex": null}`) and writes such IRIs in full.

use std::io;

use nrese_json::escape;
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{
    GraphName, GraphNameRef, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, QuadRef, Term,
    TermRef,
};

/// A prefix: its name, IRI, and whether the context must say it is one (an IRI that
/// doesn't end with a delimiter isn't a prefix by default in JSON-LD 1.1).
struct Prefix {
    name: String,
    iri: String,
    explicit: bool,
}

pub(crate) struct JsonLdWriter {
    prefixes: Vec<Prefix>,
    started: bool,
    /// The named graph whose block is open, and the prefixes it turns off.
    graph: Option<(GraphName, Vec<usize>)>,
    subject: Option<(GraphName, NamedOrBlankNode)>,
    statements: Vec<(NamedNode, Term)>,
    /// Whether the open array (a named graph's, or the top `@graph`) has an item yet.
    items: bool,
    outer_items: bool,
}

impl JsonLdWriter {
    pub(crate) fn new(prefixes: Vec<(String, String)>) -> Self {
        let prefixes = prefixes
            .into_iter()
            // A term can't be empty, and `_` is for blank nodes.
            .filter(|(name, _)| !name.is_empty() && name != "_")
            .map(|(name, iri)| Prefix {
                explicit: !iri.ends_with([':', '/', '?', '#', '[', ']', '@']),
                name,
                iri,
            })
            .collect();
        Self {
            prefixes,
            started: false,
            graph: None,
            subject: None,
            statements: Vec::new(),
            items: false,
            outer_items: false,
        }
    }

    fn start(&mut self, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        out.extend_from_slice(b"{\"@context\":{");
        for (i, prefix) in self.prefixes.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            string(&prefix.name, out);
            out.push(b':');
            if prefix.explicit {
                out.extend_from_slice(b"{\"@id\":");
                string(&prefix.iri, out);
                out.extend_from_slice(b",\"@prefix\":true}");
            } else {
                string(&prefix.iri, out);
            }
        }
        out.extend_from_slice(b"},\"@graph\":[");
    }

    /// The prefix an IRI's scheme would be mistaken for, if any.
    fn clash(&self, iri: &str) -> Option<usize> {
        let (scheme, rest) = iri.split_once(':')?;
        if rest.starts_with("//") {
            return None;
        }
        self.prefixes.iter().position(|p| p.name == scheme)
    }

    /// `iri` as written where the prefixes in `off` are turned off: compact where a prefix
    /// fits, otherwise in full.
    fn iri(&self, iri: &str, off: &[usize], out: &mut Vec<u8>) {
        let best = self
            .prefixes
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                !off.contains(i)
                    && iri.len() >= p.iri.len()
                    && iri.starts_with(p.iri.as_str())
                    && !iri[p.iri.len()..].starts_with("//")
            })
            .max_by_key(|(_, p)| p.iri.len());
        match best {
            Some((_, prefix)) => string(
                &format!("{}:{}", prefix.name, &iri[prefix.iri.len()..]),
                out,
            ),
            None => string(iri, out),
        }
    }

    fn node(&self, node: NamedOrBlankNodeRef<'_>, off: &[usize], out: &mut Vec<u8>) {
        match node {
            NamedOrBlankNodeRef::NamedNode(n) => self.iri(n.as_str(), off, out),
            NamedOrBlankNodeRef::BlankNode(b) => string(&format!("_:{}", b.as_str()), out),
        }
    }

    /// Opens a context that turns off the prefixes `off`, as the first entry of an object.
    fn turn_off(&self, off: &[usize], out: &mut Vec<u8>) {
        out.extend_from_slice(b"\"@context\":{");
        for (i, &p) in off.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            string(&self.prefixes[p].name, out);
            out.extend_from_slice(b":null");
        }
        out.extend_from_slice(b"},");
    }

    pub(crate) fn write(&mut self, out: &mut Vec<u8>, quad: QuadRef<'_>) -> io::Result<()> {
        if let TermRef::Triple(triple) = quad.object {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("JSON-LD 1.1 can't express the triple term <<( {triple} )>>"),
            ));
        }
        self.start(out);
        let same = self
            .subject
            .as_ref()
            .is_some_and(|(g, s)| g.as_ref() == quad.graph_name && s.as_ref() == quad.subject);
        if !same {
            self.flush_node(out);
            let open = self.graph.as_ref().map(|(g, _)| g.as_ref());
            let wanted = (!quad.graph_name.is_default_graph()).then_some(quad.graph_name);
            if open != wanted {
                self.close_graph(out);
                if let Some(name) = wanted {
                    self.open_graph(name, out);
                }
            }
            self.subject = Some((quad.graph_name.into_owned(), quad.subject.into_owned()));
        }
        self.statements
            .push((quad.predicate.into_owned(), quad.object.into_owned()));
        Ok(())
    }

    fn open_graph(&mut self, name: GraphNameRef<'_>, out: &mut Vec<u8>) {
        if self.outer_items {
            out.push(b',');
        }
        self.outer_items = true;
        out.extend_from_slice(b"\n{");
        let mut off = Vec::new();
        if let GraphNameRef::NamedNode(n) = name
            && let Some(p) = self.clash(n.as_str())
        {
            off.push(p);
            self.turn_off(&off, out);
        }
        out.extend_from_slice(b"\"@id\":");
        match name {
            GraphNameRef::NamedNode(n) => self.iri(n.as_str(), &off, out),
            GraphNameRef::BlankNode(b) => string(&format!("_:{}", b.as_str()), out),
            GraphNameRef::DefaultGraph => {}
        }
        out.extend_from_slice(b",\"@graph\":[");
        self.graph = Some((name.into_owned(), off));
        self.items = false;
    }

    /// Writes the buffered node object.
    fn flush_node(&mut self, out: &mut Vec<u8>) {
        let Some((_, subject)) = self.subject.take() else {
            return;
        };
        let statements = std::mem::take(&mut self.statements);
        // The prefixes the graph turned off stay off; this node turns off those its IRIs
        // would be mistaken for.
        let inherited: &[usize] = self.graph.as_ref().map_or(&[], |(_, off)| off);
        let mut own = Vec::new();
        let mut note = |iri: &str| {
            if let Some(p) = self.clash(iri)
                && !inherited.contains(&p)
                && !own.contains(&p)
            {
                own.push(p);
            }
        };
        if let NamedOrBlankNode::NamedNode(n) = &subject {
            note(n.as_str());
        }
        for (predicate, object) in &statements {
            note(predicate.as_str());
            match object {
                Term::NamedNode(n) => note(n.as_str()),
                Term::Literal(l) => note(l.datatype().as_str()),
                Term::BlankNode(_) | Term::Triple(_) => {}
            }
        }
        let mut off = inherited.to_vec();
        off.extend_from_slice(&own);

        let in_graph = self.graph.is_some();
        let started = if in_graph {
            &mut self.items
        } else {
            &mut self.outer_items
        };
        if *started {
            out.push(b',');
        }
        *started = true;
        out.extend_from_slice(if in_graph { b"\n {" } else { b"\n{" });
        if !own.is_empty() {
            self.turn_off(&own, out);
        }
        out.extend_from_slice(b"\"@id\":");
        self.node(subject.as_ref(), &off, out);
        // `rdf:type` with a node object is `@type`; the rest grouped by predicate, in the
        // order first seen.
        let is_type = |(p, o): &&(NamedNode, Term)| *p == rdf::TYPE && !o.is_literal();
        let types: Vec<&(NamedNode, Term)> = statements.iter().filter(is_type).collect();
        if !types.is_empty() {
            out.extend_from_slice(b",\"@type\":[");
            for (i, (_, object)) in types.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                if let Ok(node) = NamedOrBlankNodeRef::try_from(object.as_ref()) {
                    self.node(node, &off, out);
                }
            }
            out.push(b']');
        }
        let mut predicates: Vec<&NamedNode> = Vec::new();
        for (predicate, object) in &statements {
            if !(predicate == &rdf::TYPE && !object.is_literal())
                && !predicates.contains(&predicate)
            {
                predicates.push(predicate);
            }
        }
        for predicate in predicates {
            out.push(b',');
            self.iri(predicate.as_str(), &off, out);
            out.extend_from_slice(b":[");
            let objects = statements
                .iter()
                .filter(|(p, o)| p == predicate && !(p == &rdf::TYPE && !o.is_literal()));
            for (i, (_, object)) in objects.enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                self.value(object.as_ref(), &off, out);
            }
            out.push(b']');
        }
        out.push(b'}');
    }

    fn value(&self, object: TermRef<'_>, off: &[usize], out: &mut Vec<u8>) {
        match object {
            TermRef::NamedNode(n) => {
                out.extend_from_slice(b"{\"@id\":");
                self.iri(n.as_str(), off, out);
                out.push(b'}');
            }
            TermRef::BlankNode(b) => {
                out.extend_from_slice(b"{\"@id\":");
                string(&format!("_:{}", b.as_str()), out);
                out.push(b'}');
            }
            TermRef::Literal(literal) => {
                if let Some(language) = literal.language() {
                    out.extend_from_slice(b"{\"@value\":");
                    string(literal.value(), out);
                    out.extend_from_slice(b",\"@language\":");
                    string(language, out);
                    if let Some(direction) = literal.direction() {
                        out.extend_from_slice(b",\"@direction\":");
                        string(direction.as_str(), out);
                    }
                    out.push(b'}');
                } else if literal.datatype() == xsd::STRING {
                    string(literal.value(), out);
                } else {
                    out.extend_from_slice(b"{\"@value\":");
                    string(literal.value(), out);
                    out.extend_from_slice(b",\"@type\":");
                    self.iri(literal.datatype().as_str(), off, out);
                    out.push(b'}');
                }
            }
            TermRef::Triple(_) => unreachable!("refused in `write`"),
        }
    }

    fn close_graph(&mut self, out: &mut Vec<u8>) {
        if self.graph.take().is_some() {
            out.extend_from_slice(b"]}");
        }
    }

    pub(crate) fn finish(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        self.start(out);
        self.flush_node(out);
        self.close_graph(out);
        out.extend_from_slice(b"\n]}\n");
        Ok(())
    }
}

fn string(text: &str, out: &mut Vec<u8>) {
    escape(text, |piece| out.extend_from_slice(piece.as_bytes()));
}
