//! The expanded form to RDF: Deserialize JSON-LD to RDF, Object to RDF and List to RDF
//! (JSON-LD 1.1 API §8.1–8.3), walking the typed items directly.
//!
//! The node map of §7.2 merges the node objects of a graph before the triples are read
//! off; the triples are the same set without it, so each node object's triples are
//! written as it is met. What isn't well formed is skipped as the specification says:
//! a subject, predicate, object or graph name that isn't an absolute IRI, a datatype that
//! isn't one, a language tag that isn't BCP 47, and (no generalized RDF) a blank-node
//! predicate. The terms go into an arena that is reused, so handing the quads out
//! allocates nothing per term.

use std::fmt::Write;

use nrese_json::Value;
use nrese_json::canonical::{shortest_digits, write_canonical};
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{
    BlankNodeRef, GraphNameRef, Iri, LiteralRef, NamedNodeRef, NamedOrBlankNodeRef, QuadRef,
    TermRef,
};

use super::RdfDirection;
use super::context::has_iri_form;
use super::items::{Item, ListObject, Node, ValueObject};
use crate::blank::BlankNodes;

const I18N: &str = "https://www.w3.org/ns/i18n#";
const RDF_LANGUAGE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#language";
const RDF_DIRECTION: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#direction";

/// A piece of the arena's text.
#[derive(Debug, Clone, Copy)]
struct Span(u32, u32);

#[derive(Debug, Clone, Copy)]
enum Tag {
    Simple,
    Language(Span),
    Datatype(Span),
    Static(&'static str),
}

/// A term in the arena.
#[derive(Debug, Clone, Copy)]
enum T {
    Named(Span),
    Static(&'static str),
    Blank(Span),
    Literal(Span, Tag),
}

#[derive(Debug, Clone, Copy)]
struct RawQuad {
    subject: T,
    predicate: T,
    object: T,
    graph: Option<T>,
}

/// The quads of one batch, their terms in one string.
#[derive(Debug, Default)]
pub(crate) struct QuadArena {
    text: String,
    quads: Vec<RawQuad>,
}

impl QuadArena {
    pub(crate) fn len(&self) -> usize {
        self.quads.len()
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.quads.clear();
    }

    fn push_str(&mut self, s: &str) -> Span {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        Span(start, self.text.len() as u32)
    }

    fn str(&self, span: Span) -> &str {
        &self.text[span.0 as usize..span.1 as usize]
    }

    fn named(&self, t: T) -> NamedNodeRef<'_> {
        match t {
            T::Named(span) => NamedNodeRef::new_unchecked(self.str(span)),
            T::Static(iri) => NamedNodeRef::new_unchecked(iri),
            _ => unreachable!("a named node where an IRI was written"),
        }
    }

    fn node(&self, t: T) -> NamedOrBlankNodeRef<'_> {
        match t {
            T::Blank(span) => {
                NamedOrBlankNodeRef::BlankNode(BlankNodeRef::new_unchecked(self.str(span)))
            }
            other => NamedOrBlankNodeRef::NamedNode(self.named(other)),
        }
    }

    fn term(&self, t: T) -> TermRef<'_> {
        match t {
            T::Literal(value, tag) => {
                let value = self.str(value);
                TermRef::Literal(match tag {
                    Tag::Simple => LiteralRef::new_simple_literal(value),
                    Tag::Language(language) => {
                        LiteralRef::new_language_tagged_literal_unchecked(value, self.str(language))
                    }
                    Tag::Datatype(datatype) => LiteralRef::new_typed_literal(
                        value,
                        NamedNodeRef::new_unchecked(self.str(datatype)),
                    ),
                    Tag::Static(datatype) => {
                        LiteralRef::new_typed_literal(value, NamedNodeRef::new_unchecked(datatype))
                    }
                })
            }
            other => self.node(other).into(),
        }
    }

    pub(crate) fn quad(&self, i: usize) -> QuadRef<'_> {
        let q = self.quads[i];
        QuadRef {
            subject: self.node(q.subject),
            predicate: self.named(q.predicate),
            object: self.term(q.object),
            graph_name: match q.graph {
                None => GraphNameRef::DefaultGraph,
                Some(T::Blank(span)) => {
                    GraphNameRef::BlankNode(BlankNodeRef::new_unchecked(self.str(span)))
                }
                Some(other) => GraphNameRef::NamedNode(self.named(other)),
            },
        }
    }
}

/// Blank node names for one document, without a table of the labels seen: a label that
/// is a valid N-Triples label not starting with `j` stays, any other becomes `jx` and its
/// bytes in hexadecimal, and a node without a label gets `jg` and a counter (three
/// disjoint sets). Under renaming, each name is then made fresh for the document.
#[derive(Debug)]
pub(crate) struct BlankNames {
    blank_nodes: BlankNodes,
    counter: u64,
    name: String,
    fresh: String,
}

impl BlankNames {
    pub(crate) fn new(blank_nodes: BlankNodes) -> Self {
        Self {
            blank_nodes,
            counter: 0,
            name: String::new(),
            fresh: String::new(),
        }
    }

    /// The name for the document's `_:label` (`label` without `_:`).
    fn labelled(&mut self, label: &str, arena: &mut QuadArena) -> Span {
        self.name.clear();
        if !label.starts_with('j') && nrese_rdf::BlankNodeRef::new(label).is_ok() {
            self.name.push_str(label);
        } else {
            self.name.push_str("jx");
            for b in label.bytes() {
                let _ = write!(self.name, "{b:02x}");
            }
        }
        self.finish(arena)
    }

    /// A new blank node.
    fn generated(&mut self, arena: &mut QuadArena) -> Span {
        self.name.clear();
        let _ = write!(self.name, "jg{}", self.counter);
        self.counter += 1;
        self.finish(arena)
    }

    fn finish(&mut self, arena: &mut QuadArena) -> Span {
        let name = self.blank_nodes.name(&self.name, &mut self.fresh);
        arena.push_str(name)
    }
}

/// Writes the quads of expanded items.
pub(crate) struct Emitter<'a> {
    pub(crate) arena: &'a mut QuadArena,
    pub(crate) names: &'a mut BlankNames,
    pub(crate) rdf_direction: Option<RdfDirection>,
    pub(crate) unchecked: bool,
    pub(crate) scratch: String,
}

impl Emitter<'_> {
    fn emit(&mut self, subject: T, predicate: T, object: T, graph: Option<T>) {
        self.arena.quads.push(RawQuad {
            subject,
            predicate,
            object,
            graph,
        });
    }

    fn well_formed_iri(&self, iri: &str) -> bool {
        if self.unchecked {
            has_iri_form(iri)
        } else {
            Iri::parse(iri).is_ok()
        }
    }

    /// An IRI or blank node identifier as a term; `None` if not well formed.
    fn resource(&mut self, id: &str) -> Option<T> {
        if let Some(label) = id.strip_prefix("_:") {
            return Some(T::Blank(self.names.labelled(label, self.arena)));
        }
        self.iri(id)
    }

    fn iri(&mut self, iri: &str) -> Option<T> {
        self.well_formed_iri(iri)
            .then(|| T::Named(self.arena.push_str(iri)))
    }

    /// The quads of expanded items in the default graph.
    pub(crate) fn document(&mut self, items: &[Item]) {
        self.items(items, None);
    }

    /// The node objects of a graph (other items there are free-floating: nothing).
    fn items(&mut self, items: &[Item], graph: Option<T>) {
        for item in items {
            if let Item::Node(node) = item {
                self.node(node, graph);
            }
        }
    }

    /// A node object's triples; its subject, if well formed.
    fn node(&mut self, node: &Node, graph: Option<T>) -> Option<T> {
        let subject = match &node.id {
            Some(id) => self.resource(id),
            None if node.id_null => None,
            None => Some(T::Blank(self.names.generated(self.arena))),
        };
        for t in &node.types {
            if let (Some(s), Some(o)) = (subject, self.resource(t)) {
                self.emit(s, T::Static(rdf::TYPE.as_str()), o, graph);
            }
        }
        for (property, values) in &node.properties {
            // Values are still walked when there is no triple: nested nodes have theirs.
            let predicate = if property.starts_with("_:") {
                None
            } else {
                self.iri(property)
            };
            for value in values {
                let object = self.object(value, graph);
                if let (Some(s), Some(p), Some(o)) = (subject, predicate, object) {
                    self.emit(s, p, o, graph);
                }
            }
        }
        for (property, values) in &node.reverse {
            let predicate = if property.starts_with("_:") {
                None
            } else {
                self.iri(property)
            };
            for value in values {
                if let Item::Node(other) = value {
                    let other = self.node(other, graph);
                    if let (Some(s), Some(p), Some(o)) = (other, predicate, subject) {
                        self.emit(s, p, o, graph);
                    }
                }
            }
        }
        if let (Some(items), Some(name)) = (&node.graph, subject) {
            self.items(items, Some(name));
        }
        if let Some(included) = &node.included {
            self.items(included, graph);
        }
        subject
    }

    /// Object to RDF (§8.2).
    fn object(&mut self, item: &Item, graph: Option<T>) -> Option<T> {
        match item {
            Item::Node(node) => self.node(node, graph),
            Item::Value(value) => self.literal(value, graph),
            Item::List(list) => Some(self.list(list, graph)),
        }
    }

    /// List to RDF (§8.3).
    fn list(&mut self, list: &ListObject, graph: Option<T>) -> T {
        let nil = T::Static(rdf::NIL.as_str());
        if list.items.is_empty() {
            return nil;
        }
        let nodes: Vec<T> = (0..list.items.len())
            .map(|_| T::Blank(self.names.generated(self.arena)))
            .collect();
        for (i, item) in list.items.iter().enumerate() {
            if let Some(object) = self.object(item, graph) {
                self.emit(nodes[i], T::Static(rdf::FIRST.as_str()), object, graph);
            }
            let rest = nodes.get(i + 1).copied().unwrap_or(nil);
            self.emit(nodes[i], T::Static(rdf::REST.as_str()), rest, graph);
        }
        nodes[0]
    }

    /// A value object as a literal (or, for `compound-literal`, a node).
    fn literal(&mut self, item: &ValueObject, graph: Option<T>) -> Option<T> {
        let datatype = item.datatype.as_deref();
        if let Some(datatype) = datatype
            && datatype != "@json"
            && !self.well_formed_iri(datatype)
        {
            return None;
        }
        if let Some(language) = &item.language
            && !nrese_rdf::language::is_well_formed(language)
        {
            return None;
        }
        self.scratch.clear();
        let mut default = xsd::STRING.as_str();
        if datatype == Some("@json") {
            write_canonical(&item.value, &mut self.scratch).ok()?;
            default = rdf::JSON.as_str();
        } else {
            match &item.value {
                Value::Boolean(b) => {
                    self.scratch.push_str(if *b { "true" } else { "false" });
                    default = xsd::BOOLEAN.as_str();
                }
                Value::Number(text) => {
                    let x: f64 = text.parse().ok()?;
                    if x.fract() != 0.0 || x.abs() >= 1e21 || datatype == Some(xsd::DOUBLE.as_str())
                    {
                        write_double(x, &mut self.scratch);
                        default = xsd::DOUBLE.as_str();
                    } else {
                        let _ = write!(self.scratch, "{:.0}", x);
                        if self.scratch == "-0" {
                            self.scratch.replace_range(.., "0");
                        }
                        default = xsd::INTEGER.as_str();
                    }
                }
                Value::String(s) => {
                    self.scratch.push_str(s);
                    if item.language.is_some() {
                        default = rdf::LANG_STRING.as_str();
                    }
                }
                _ => return None,
            }
        }
        let value = self.arena.push_str(&self.scratch);
        if let (Some(direction), Some(mode)) = (item.direction, self.rdf_direction) {
            let language = item
                .language
                .as_deref()
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            match mode {
                RdfDirection::I18nDatatype => {
                    let datatype = format!("{I18N}{language}_{}", direction.as_str());
                    let datatype = self.arena.push_str(&datatype);
                    return Some(T::Literal(value, Tag::Datatype(datatype)));
                }
                RdfDirection::CompoundLiteral => {
                    let node = T::Blank(self.names.generated(self.arena));
                    self.emit(
                        node,
                        T::Static(rdf::VALUE.as_str()),
                        T::Literal(value, Tag::Simple),
                        graph,
                    );
                    if item.language.is_some() {
                        let language = self.arena.push_str(&language);
                        self.emit(
                            node,
                            T::Static(RDF_LANGUAGE),
                            T::Literal(language, Tag::Simple),
                            graph,
                        );
                    }
                    let direction = self.arena.push_str(direction.as_str());
                    self.emit(
                        node,
                        T::Static(RDF_DIRECTION),
                        T::Literal(direction, Tag::Simple),
                        graph,
                    );
                    return Some(node);
                }
            }
        }
        if let Some(language) = &item.language {
            let language = self.arena.push_str(&language.to_ascii_lowercase());
            return Some(T::Literal(value, Tag::Language(language)));
        }
        let tag = match datatype.filter(|d| *d != "@json") {
            Some(d) if d == xsd::STRING.as_str() => Tag::Simple,
            Some(d) => Tag::Datatype(self.arena.push_str(d)),
            None if default == xsd::STRING.as_str() => Tag::Simple,
            None => Tag::Static(default),
        };
        Some(T::Literal(value, tag))
    }
}

/// The canonical lexical form of an `xsd:double` (XSD 1.1 §3.3.5.2): `1.0E0`, `-1.25E-3`.
pub(crate) fn write_double(x: f64, out: &mut String) {
    if x.is_nan() {
        out.push_str("NaN");
    } else if x.is_infinite() {
        out.push_str(if x > 0.0 { "INF" } else { "-INF" });
    } else if x == 0.0 {
        out.push_str(if x.is_sign_negative() {
            "-0.0E0"
        } else {
            "0.0E0"
        });
    } else {
        if x < 0.0 {
            out.push('-');
        }
        let (digits, n) = shortest_digits(x.abs());
        out.push_str(&digits[..1]);
        out.push('.');
        out.push_str(if digits.len() > 1 { &digits[1..] } else { "0" });
        let _ = write!(out, "E{}", n - 1);
    }
}

/// The literal a value object would give, for the writer's tests.
#[cfg(test)]
pub(crate) fn double(x: f64) -> nrese_rdf::Literal {
    let mut out = String::new();
    write_double(x, &mut out);
    nrese_rdf::Literal::new_typed_literal(out, xsd::DOUBLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_doubles() {
        for (x, text) in [
            (5.3, "5.3E0"),
            (1.0, "1.0E0"),
            (123.45, "1.2345E2"),
            (1e21, "1.0E21"),
            (-0.00125, "-1.25E-3"),
            (0.0, "0.0E0"),
        ] {
            assert_eq!(double(x).value(), text);
        }
    }
}
