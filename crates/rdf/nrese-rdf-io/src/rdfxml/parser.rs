//! The RDF/XML parser (RDF 1.2 XML Syntax, §6 grammar), on `quick-xml`'s events.
//!
//! A stack of frames, one per open element, says what an element means where it stands: a
//! node element (`rdf:Description` or typed) gives a subject; a property element below it
//! gives a predicate, and an object from its attributes, its text, a nested node element,
//! or `rdf:parseType` (`Resource`, `Collection`, `Literal`, `Triple`). Each frame keeps the
//! `xml:base`, `xml:lang`, `rdf:version` and `its:dir` in scope. Statements go to a queue
//! as they are complete.
//!
//! - **RDF 1.2.** `rdf:annotation` and `rdf:annotationNodeID` reify a property element's
//!   statement (`reifier rdf:reifies <<( s p o )>>`). `rdf:parseType="Triple"` makes the
//!   one statement its content describes a triple term: that content's statements are
//!   collected in its frame instead of the queue. `its:dir` gives literals a base direction.
//!   Both need `rdf:version` 1.2 or later in scope; without it a `Triple` element is
//!   ignored, and `its:dir` is an ordinary property attribute.
//!
//! - **Entities** declared in the document type (`<!ENTITY xsd "…">`, common in OWL files)
//!   are expanded in attribute values and text, as are XML's own and character references.
//! - **XML literals** are written in exclusive canonical form (Exclusive XML Canonicalization
//!   1.0, without comments): namespace declarations only where a name uses them and an
//!   ancestor in the literal hasn't declared them, attributes sorted, text and attribute
//!   values escaped as canonicalisation says.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufRead;
use std::sync::Arc;

use nrese_rdf::vocab::rdf;
use nrese_rdf::{
    BaseDirection, BlankNode, GraphName, Iri, Literal, NamedNode, NamedOrBlankNode, Quad, Term,
    Triple,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::blank::BlankNodes;
use crate::error::{RdfParseError, RdfSyntaxError, TextPosition};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const ITS: &str = "http://www.w3.org/2005/11/its";

/// The settings an RDF/XML parser takes from [`crate::RdfParser`].
#[derive(Debug, Clone)]
pub(crate) struct RdfXmlSettings {
    pub(crate) base: Option<Iri<String>>,
    pub(crate) blank_nodes: BlankNodes,
    pub(crate) unchecked: bool,
    /// After a syntax error, go on after the outermost node element it is in
    /// ([`crate::RdfParser::recovering`]).
    pub(crate) recover: bool,
}

/// What an open element is.
enum Frame {
    /// `rdf:RDF`: node elements follow.
    Rdf,
    /// A node element: property elements follow.
    Node {
        subject: NamedOrBlankNode,
        /// The last `rdf:li` number.
        li: u64,
    },
    /// A property element, its object not known yet.
    Property {
        subject: NamedOrBlankNode,
        predicate: NamedNode,
        /// `rdf:ID`: the statement is reified with this IRI.
        reified: Option<NamedNode>,
        /// `rdf:annotation` or `rdf:annotationNodeID`: the statement's reifier.
        annotation: Option<NamedOrBlankNode>,
        datatype: Option<NamedNode>,
        /// The object its attributes give (`rdf:resource`, `rdf:nodeID`, or a blank node
        /// carrying property attributes); then the element must be empty.
        object: Option<NamedOrBlankNode>,
        text: String,
        /// A node element stood in it (it is then the object).
        nested: bool,
    },
    /// `rdf:parseType="Collection"`: node elements, the list's members.
    Collection {
        subject: NamedOrBlankNode,
        predicate: NamedNode,
        reified: Option<NamedNode>,
        annotation: Option<NamedOrBlankNode>,
        members: Vec<NamedOrBlankNode>,
    },
    /// `rdf:parseType="Literal"`: the content, as canonical XML.
    Literal {
        subject: NamedOrBlankNode,
        predicate: NamedNode,
        reified: Option<NamedNode>,
        annotation: Option<NamedOrBlankNode>,
        xml: String,
        /// Elements open inside the literal: the name as written, and the namespaces it
        /// declared (prefix, IRI).
        open: Vec<(String, Vec<(String, String)>)>,
    },
    /// `rdf:parseType="Triple"`: the statements its content makes, of which there must be
    /// exactly one, the triple term.
    Triple {
        subject: NamedOrBlankNode,
        predicate: NamedNode,
        captured: Vec<Quad>,
    },
    /// An element RDF ignores, with everything in it (`depth`: elements open inside).
    Ignored { depth: usize },
}

/// What an element inherits from the elements around it. Shared with the scopes inside:
/// an element's scope costs no copy of the strings.
#[derive(Clone, Default)]
struct Inherited {
    base: Option<Arc<Iri<String>>>,
    language: Option<Arc<str>>,
    /// `rdf:version`.
    version: Option<Arc<str>>,
    /// `its:dir`, where the version allows it.
    direction: Option<BaseDirection>,
}

impl Inherited {
    /// Whether the version in scope is RDF 1.2 or later (`1.2`, `1.2-basic`, `1.3`, `2.0`…).
    fn rdf_12(&self) -> bool {
        self.version.as_deref().is_some_and(|version| {
            let mut parts = version.split(['.', '-']);
            let major = parts.next().and_then(|p| p.parse::<u32>().ok());
            let minor = parts
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .unwrap_or(0);
            major.is_some_and(|major| (major, minor) >= (1, 2))
        })
    }

    /// A literal of `value` in this scope's language (and direction).
    fn literal(&self, value: impl Into<String>) -> Literal {
        match (&self.language, self.direction) {
            (Some(tag), Some(direction)) => {
                Literal::new_directional_language_tagged_literal_unchecked(value, &**tag, direction)
            }
            (Some(tag), None) => Literal::new_language_tagged_literal_unchecked(value, &**tag),
            (None, _) => Literal::new_simple_literal(value),
        }
    }
}

/// A frame with the scope it opened in.
struct Scope {
    frame: Frame,
    inherited: Inherited,
}

pub(crate) struct RdfXmlParser<R: BufRead> {
    reader: Reader<R>,
    /// The namespaces each open element declares (prefix, IRI; "" for the default), with
    /// entities expanded: `xmlns:rdf="&rdf;"` is common in OWL files.
    namespaces: Vec<Vec<(String, Arc<str>)>>,
    /// The IRIs of element and attribute names, checked once per document.
    iri_cache: RefCell<HashMap<String, NamedNode>>,
    key: RefCell<String>,
    no_namespace: Arc<str>,
    xml_namespace: Arc<str>,
    buffer: Vec<u8>,
    settings: RdfXmlSettings,
    stack: Vec<Scope>,
    entities: HashMap<String, String>,
    /// IRIs `rdf:ID` has made (each only once in a document).
    ids: HashSet<String>,
    generated_key: u64,
    generated: u64,
    queue: VecDeque<Quad>,
    /// `rdf:parseType="Triple"` elements open: their statements go to their frames.
    capturing: usize,
    pub(crate) current: Option<Quad>,
    done: bool,
    /// The XML reader failed (not well-formed) or the input ended: nothing to resume.
    broken: bool,
}

type Step<T> = Result<T, RdfParseError>;

/// The parts of an element: its IRI and its attributes (resolved, values unescaped).
struct Element {
    /// The namespace and local name.
    namespace: Arc<str>,
    local: String,
    attributes: Vec<Attribute>,
}

struct Attribute {
    namespace: Arc<str>,
    local: String,
    value: String,
}

impl Element {
    fn is_rdf(&self, local: &str) -> bool {
        &*self.namespace == RDF && self.local == local
    }
}

impl<R: BufRead> RdfXmlParser<R> {
    pub(crate) fn new(reader: R, settings: RdfXmlSettings) -> Self {
        let mut reader = Reader::from_reader(reader);
        let config = reader.config_mut();
        config.expand_empty_elements = true;
        config.check_end_names = true;
        Self {
            reader,
            buffer: Vec::new(),
            settings,
            stack: Vec::new(),
            namespaces: Vec::new(),
            iri_cache: RefCell::new(HashMap::new()),
            key: RefCell::new(String::new()),
            no_namespace: Arc::from(""),
            xml_namespace: Arc::from(XML),
            entities: HashMap::new(),
            ids: HashSet::new(),
            generated_key: {
                use std::hash::BuildHasher;
                std::collections::hash_map::RandomState::new().hash_one(0_u8)
            },
            generated: 0,
            queue: VecDeque::new(),
            capturing: 0,
            current: None,
            done: false,
            broken: false,
        }
    }

    /// Moves to the next quad (in `self.current`); `false` at the end.
    pub(crate) fn advance(&mut self) -> Step<bool> {
        loop {
            if let Some(quad) = self.queue.pop_front() {
                self.current = Some(quad);
                return Ok(true);
            }
            if self.done {
                self.current = None;
                return Ok(false);
            }
            if let Err(error) = self.event() {
                self.queue.clear();
                let syntax = matches!(error, RdfParseError::Syntax(_));
                self.done = !(syntax && self.settings.recover && self.resync());
                return Err(error);
            }
        }
    }

    /// After a syntax error: the rest of the outermost node element it is in (below
    /// `rdf:RDF`, or the document's root) is skipped, with the statements of the failing
    /// event not handed out yet, and parsing goes on after that element. `false` where
    /// there is nothing to resume: the XML isn't well-formed, the input ended, or the error
    /// is in `rdf:RDF` itself.
    fn resync(&mut self) -> bool {
        if self.broken {
            return false;
        }
        // The stack's index of the outermost node element.
        let top = match self.stack.first().map(|scope| &scope.frame) {
            Some(Frame::Rdf) => 1,
            Some(_) => 0,
            None => return false,
        };
        // Elements open in the XML: one namespace entry each, pushed at a start tag and
        // popped at its end tag even when the element's own handling failed.
        let open = self.namespaces.len();
        if open <= top {
            // The outermost node element has ended (its end tag failed): go on after it.
            self.stack.truncate(top);
            self.capturing = 0;
            return top == 1 && open == 1;
        }
        let depth = open - top - 1;
        if self.stack.len() > top {
            self.stack.truncate(top + 1);
            self.stack[top].frame = Frame::Ignored { depth };
        } else {
            // Its start tag failed before its frame was pushed.
            let inherited = self
                .stack
                .last()
                .map(|scope| scope.inherited.clone())
                .unwrap_or_default();
            self.stack.push(Scope {
                frame: Frame::Ignored { depth },
                inherited,
            });
        }
        self.capturing = 0;
        true
    }

    fn error(&self, message: impl Into<String>) -> RdfParseError {
        let offset = self.reader.buffer_position();
        let at = TextPosition {
            line: 0,
            column: 0,
            offset,
        };
        RdfSyntaxError::new(message, at..at).into()
    }

    fn base(&self) -> Option<&Iri<String>> {
        self.stack
            .last()
            .map_or(self.settings.base.as_ref(), |s| s.inherited.base.as_deref())
    }

    /// The IRI of a qualified name, checked the first time it is met in the document.
    fn name_iri(&self, namespace: &str, local: &str) -> Step<NamedNode> {
        let mut key = self.key.borrow_mut();
        key.clear();
        key.push_str(namespace);
        key.push_str(local);
        if let Some(iri) = self.iri_cache.borrow().get(key.as_str()) {
            return Ok(iri.clone());
        }
        let iri = self.iri(key.clone())?;
        let mut cache = self.iri_cache.borrow_mut();
        // Bounded: a document with ever new names can't grow it without end.
        if cache.len() >= 65_536 {
            cache.clear();
        }
        cache.insert(key.clone(), iri.clone());
        Ok(iri)
    }

    /// An attribute's value: entities expanded and whitespace normalised; one allocation
    /// when there is neither to do.
    fn attribute_value(&self, raw: &str) -> String {
        if raw.contains(['&', '\t', '\n', '\r']) {
            normalize_attribute(&self.expand(raw))
        } else {
            raw.to_owned()
        }
    }

    /// An IRI reference resolved against the base in scope.
    fn resolve(&self, reference: &str) -> Step<NamedNode> {
        match self.base() {
            Some(base) if self.settings.unchecked => Ok(NamedNode::new_unchecked(
                base.resolve_unchecked(reference).into_inner(),
            )),
            Some(base) => base
                .resolve(reference)
                .map(NamedNode::new_from_iri)
                .map_err(|e| self.error(format!("an invalid IRI {reference:?}: {e}"))),
            None if self.settings.unchecked => Ok(NamedNode::new_unchecked(reference)),
            None => NamedNode::new(reference)
                .map_err(|e| self.error(format!("an invalid or relative IRI {reference:?}: {e}"))),
        }
    }

    fn iri(&self, iri: String) -> Step<NamedNode> {
        if self.settings.unchecked {
            Ok(NamedNode::new_unchecked(iri))
        } else {
            NamedNode::new(iri.clone())
                .map_err(|e| self.error(format!("an invalid IRI {iri:?}: {e}")))
        }
    }

    fn fresh(&mut self) -> BlankNode {
        self.generated += 1;
        BlankNode::new_unchecked(format!("r{:016x}x{}", self.generated_key, self.generated))
    }

    fn labelled(&self, label: &str) -> Step<BlankNode> {
        if !is_nc_name(label) {
            return Err(self.error(format!("rdf:nodeID {label:?} is not an XML name")));
        }
        let mut out = String::new();
        let name = self.settings.blank_nodes.name(label, &mut out);
        if BlankNode::new(name).is_ok() {
            return Ok(BlankNode::new_unchecked(name));
        }
        // An XML name that isn't a blank node label (`object.`: N-Triples labels can't end
        // with a dot): named from the document's key and the name, in hex, so that every
        // writer can write it and the same name stays the same node (found by fuzzing,
        // `nrese-fuzz`).
        let hex: String = label.bytes().map(|b| format!("{b:02x}")).collect();
        Ok(BlankNode::new_unchecked(format!(
            "r{:016x}n{hex}",
            self.generated_key
        )))
    }

    /// The IRI `rdf:ID` makes: the base with the fragment `id`, once per document.
    fn id(&mut self, id: &str) -> Step<NamedNode> {
        if !is_nc_name(id) {
            return Err(self.error(format!("rdf:ID {id:?} is not an XML name")));
        }
        let iri = self.resolve(&format!("#{id}"))?;
        if !self.ids.insert(iri.as_str().to_owned()) {
            return Err(self.error(format!("rdf:ID {id:?} is used twice")));
        }
        Ok(iri)
    }

    fn emit(&mut self, subject: NamedOrBlankNode, predicate: NamedNode, object: Term) {
        let quad = Quad::new(subject, predicate, object, GraphName::DefaultGraph);
        if self.capturing > 0 {
            // Inside `rdf:parseType="Triple"`: the innermost one's content.
            let captured = self
                .stack
                .iter_mut()
                .rev()
                .find_map(|scope| match &mut scope.frame {
                    Frame::Triple { captured, .. } => Some(captured),
                    _ => None,
                });
            if let Some(captured) = captured {
                captured.push(quad);
                return;
            }
        }
        self.queue.push_back(quad);
    }

    /// A statement, its reification if a property element had `rdf:ID`, and its
    /// annotation (`reifier rdf:reifies <<( s p o )>>`) if it had `rdf:annotation` or
    /// `rdf:annotationNodeID`.
    fn statement(
        &mut self,
        subject: NamedOrBlankNode,
        predicate: NamedNode,
        object: Term,
        reified: Option<NamedNode>,
        annotation: Option<NamedOrBlankNode>,
    ) {
        if let Some(reifier) = annotation {
            let term = Triple::new(subject.clone(), predicate.clone(), object.clone());
            self.emit(reifier, rdf::REIFIES.into_owned(), term.into());
        }
        if let Some(statement) = reified {
            let r: NamedOrBlankNode = statement.into();
            self.emit(
                r.clone(),
                rdf::TYPE.into_owned(),
                rdf::STATEMENT.into_owned().into(),
            );
            self.emit(r.clone(), rdf::SUBJECT.into_owned(), subject.clone().into());
            self.emit(
                r.clone(),
                rdf::PREDICATE.into_owned(),
                predicate.clone().into(),
            );
            self.emit(r, rdf::OBJECT.into_owned(), object.clone());
        }
        self.emit(subject, predicate, object);
    }

    // --- Events --------------------------------------------------------------------------

    fn event(&mut self) -> Step<()> {
        let mut buffer = std::mem::take(&mut self.buffer);
        buffer.clear();
        let result = self.dispatch(&mut buffer);
        self.buffer = buffer;
        result
    }

    fn dispatch(&mut self, buffer: &mut Vec<u8>) -> Step<()> {
        let event = match self.reader.read_event_into(buffer) {
            Ok(event) => event,
            Err(e) => {
                self.broken = true;
                return Err(self.error(format!("not well-formed XML: {e}")));
            }
        };
        match event {
            Event::Start(start) => {
                self.open_namespaces(&start)?;
                if let Some(Scope {
                    frame: Frame::Ignored { depth },
                    ..
                }) = self.stack.last_mut()
                {
                    *depth += 1;
                    return Ok(());
                }
                // Inside an XML literal, an element is content.
                if let Some(Scope {
                    frame: Frame::Literal { .. },
                    ..
                }) = self.stack.last()
                {
                    return self.literal_start(&start);
                }
                let element = self.element(&start)?;
                self.start(element)
            }
            Event::End(_) => {
                let ended = match self.stack.last_mut() {
                    Some(Scope {
                        frame: Frame::Ignored { depth },
                        ..
                    }) if *depth > 0 => {
                        *depth -= 1;
                        Ok(())
                    }
                    _ => self.end(),
                };
                self.namespaces.pop();
                ended
            }
            Event::Text(text) => {
                let text = text.xml10_content();
                self.text(&text, false)
            }
            Event::CData(data) => {
                let text = data.xml10_content();
                self.text(&text, true)
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    Ok(None) => {
                        let name = reference.xml10_content();
                        match predefined(&name)
                            .map(str::to_owned)
                            .or_else(|| self.entities.get(name.as_ref()).cloned())
                        {
                            Some(value) => value,
                            None => {
                                return Err(self.error(format!("an undeclared entity &{name};")));
                            }
                        }
                    }
                    Err(e) => {
                        return Err(self.error(format!("an invalid character reference: {e}")));
                    }
                };
                self.text(&resolved, true)
            }
            Event::DocType(doctype) => {
                let text = doctype.xml10_content().into_owned();
                self.declare_entities(&text);
                Ok(())
            }
            Event::Eof => {
                self.broken = true;
                if !self.stack.is_empty() {
                    return Err(self.error("the document ends inside an element"));
                }
                self.done = true;
                Ok(())
            }
            Event::Empty(_) => unreachable!("empty elements are expanded"),
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) => {
                if let Some(Scope {
                    frame: Frame::Literal { xml, .. },
                    ..
                }) = self.stack.last_mut()
                    && let Event::PI(pi) = &event
                {
                    xml.push_str("<?");
                    xml.push_str(pi);
                    xml.push_str("?>");
                }
                Ok(())
            }
        }
    }

    /// `<!ENTITY name "value">` declarations of the internal subset.
    fn declare_entities(&mut self, doctype: &str) {
        let mut rest = doctype;
        while let Some(at) = rest.find("<!ENTITY") {
            rest = &rest[at + 8..];
            let trimmed = rest.trim_start();
            let name_end = trimmed
                .find(|c: char| c.is_whitespace())
                .unwrap_or(trimmed.len());
            let name = &trimmed[..name_end];
            let after = trimmed[name_end..].trim_start();
            let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            let Some(close) = after[1..].find(quote) else {
                break;
            };
            let raw = &after[1..1 + close];
            // Earlier entities and character references in the value.
            let value = self.expand(raw);
            self.entities.entry(name.to_owned()).or_insert(value);
            rest = &after[1 + close..];
        }
    }

    /// Expands `&name;` (declared or predefined) and `&#…;` in `text`.
    fn expand(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find('&') {
            out.push_str(&rest[..at]);
            let Some(end) = rest[at..].find(';') else {
                out.push_str(&rest[at..]);
                return out;
            };
            let name = &rest[at + 1..at + end];
            if let Some(code) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                out.extend(u32::from_str_radix(code, 16).ok().and_then(char::from_u32));
            } else if let Some(code) = name.strip_prefix('#') {
                out.extend(code.parse().ok().and_then(char::from_u32));
            } else if let Some(value) = predefined(name)
                .map(str::to_owned)
                .or_else(|| self.entities.get(name).cloned())
            {
                out.push_str(&value);
            } else {
                out.push_str(&rest[at..at + end + 1]);
            }
            rest = &rest[at + end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// The namespace declarations of a start tag, entities expanded.
    fn open_namespaces(&mut self, start: &BytesStart<'_>) -> Step<()> {
        let mut declared = Vec::new();
        for attribute in start.attributes().with_checks(true) {
            let attribute =
                attribute.map_err(|e| self.error(format!("a malformed attribute: {e}")))?;
            let key: &str = attribute.key.as_ref();
            let prefix = match key.strip_prefix("xmlns") {
                Some("") => "",
                Some(rest) => match rest.strip_prefix(':') {
                    Some(prefix) => prefix,
                    None => continue,
                },
                None => continue,
            };
            declared.push((prefix.to_owned(), Arc::from(self.expand(&attribute.value))));
        }
        self.namespaces.push(declared);
        Ok(())
    }

    /// The IRI bound to `prefix` ("" for the default namespace), if any.
    fn namespace_of(&self, prefix: &str) -> Option<&Arc<str>> {
        if prefix == "xml" {
            return Some(&self.xml_namespace);
        }
        self.namespaces
            .iter()
            .rev()
            .flat_map(|declared| declared.iter().rev())
            .find(|(p, _)| p == prefix)
            .map(|(_, ns)| ns)
            .filter(|ns| !ns.is_empty())
    }

    /// A qualified name: its namespace and local name. An element without a prefix is in
    /// the default namespace; an attribute without one is in none.
    fn resolve_name(&self, name: &str, attribute: bool) -> Step<(Arc<str>, String)> {
        match name.split_once(':') {
            Some((prefix, local)) => match self.namespace_of(prefix) {
                Some(ns) => Ok((ns.clone(), local.to_owned())),
                None => Err(self.error(format!("the namespace prefix {prefix:?} isn't declared"))),
            },
            None if attribute => Ok((self.no_namespace.clone(), name.to_owned())),
            None => Ok((
                self.namespace_of("").unwrap_or(&self.no_namespace).clone(),
                name.to_owned(),
            )),
        }
    }

    /// The element's name and attributes, resolved against the namespaces in scope.
    fn element(&self, start: &BytesStart<'_>) -> Step<Element> {
        let (namespace, local) = self.resolve_name(start.name().as_ref(), false)?;
        let mut attributes = Vec::new();
        for attribute in start.attributes().with_checks(true) {
            let attribute =
                attribute.map_err(|e| self.error(format!("a malformed attribute: {e}")))?;
            let key = attribute.key;
            let key_text: &str = key.as_ref();
            if key_text == "xmlns" || key_text.starts_with("xmlns:") {
                continue;
            }
            let (namespace, local) = self.resolve_name(key_text, true)?;
            attributes.push(Attribute {
                namespace,
                local,
                value: self.attribute_value(&attribute.value),
            });
        }
        Ok(Element {
            namespace,
            local,
            attributes,
        })
    }

    /// The scope a new element opens: its `xml:base`, `xml:lang`, `rdf:version` and (with
    /// RDF 1.2 in scope) `its:dir`, or those around it.
    fn scope(&self, element: &Element) -> Step<Inherited> {
        let mut inherited = match self.stack.last() {
            Some(scope) => scope.inherited.clone(),
            None => Inherited {
                base: self.settings.base.clone().map(Arc::new),
                ..Inherited::default()
            },
        };
        for attribute in &element.attributes {
            match (&*attribute.namespace, attribute.local.as_str()) {
                (XML, "base") => {
                    // The base without its fragment.
                    let reference = attribute.value.split('#').next().unwrap_or("");
                    let resolved = self.resolve(reference)?;
                    inherited.base = Some(Arc::new(
                        Iri::parse(resolved.into_string())
                            .map_err(|e| self.error(format!("an invalid xml:base: {e}")))?,
                    ));
                }
                (XML, "lang") => {
                    // XML allows any text here; RDF only BCP 47 tags (as the other readers).
                    let tag = attribute.value.to_ascii_lowercase();
                    if !tag.is_empty()
                        && !self.settings.unchecked
                        && !nrese_rdf::language::is_well_formed(&tag)
                    {
                        return Err(self.error(format!(
                            "xml:lang {:?} is not a BCP 47 language tag",
                            attribute.value
                        )));
                    }
                    inherited.language = (!tag.is_empty()).then(|| Arc::from(tag));
                }
                (RDF, "version") => inherited.version = Some(Arc::from(attribute.value.as_str())),
                _ => {}
            }
        }
        // `its:dir` counts only under RDF 1.2, which this element's own `rdf:version` may
        // announce.
        if inherited.rdf_12() {
            for attribute in &element.attributes {
                if &*attribute.namespace == ITS && attribute.local == "dir" {
                    inherited.direction = match attribute.value.as_str() {
                        "" => None,
                        value => Some(value.parse().map_err(|_| {
                            self.error(format!("its:dir {value:?} is not ltr, rtl or empty"))
                        })?),
                    };
                }
            }
        }
        Ok(inherited)
    }

    /// Whether `attribute` is consumed by the scope rather than a property attribute:
    /// `rdf:version`, and `its:dir` and `its:version` under RDF 1.2.
    fn scoped(attribute: &Attribute, inherited: &Inherited) -> bool {
        (&*attribute.namespace == RDF && attribute.local == "version")
            || (&*attribute.namespace == ITS
                && matches!(attribute.local.as_str(), "dir" | "version")
                && inherited.rdf_12())
    }

    fn start(&mut self, element: Element) -> Step<()> {
        let inherited = self.scope(&element)?;
        let expects_node = match self.stack.last().map(|s| &s.frame) {
            None | Some(Frame::Rdf) | Some(Frame::Collection { .. }) => true,
            Some(Frame::Property { .. }) | Some(Frame::Triple { .. }) => true,
            Some(Frame::Node { .. }) => false,
            Some(Frame::Literal { .. }) | Some(Frame::Ignored { .. }) => {
                unreachable!("handled before")
            }
        };
        if self.stack.is_empty() && element.is_rdf("RDF") {
            self.stack.push(Scope {
                frame: Frame::Rdf,
                inherited,
            });
            return Ok(());
        }
        if expects_node {
            self.node_element(element, inherited)
        } else {
            self.property_element(element, inherited)
        }
    }

    /// A node element (§7.2.11).
    fn node_element(&mut self, element: Element, inherited: Inherited) -> Step<()> {
        if &*element.namespace == RDF
            && matches!(
                element.local.as_str(),
                "RDF"
                    | "ID"
                    | "about"
                    | "bagID"
                    | "parseType"
                    | "resource"
                    | "nodeID"
                    | "datatype"
                    | "li"
                    | "aboutEach"
                    | "aboutEachPrefix"
            )
        {
            return Err(self.error(format!("rdf:{} can't be a node element", element.local)));
        }
        // The scope's base applies to this element's own attributes.
        self.stack.push(Scope {
            frame: Frame::Rdf,
            inherited,
        });
        let result = self.node_subject(&element);
        let scope = self.stack.pop().expect("pushed");
        let (subject, properties) = result?;
        // The parent: a property element gets its object, a collection a member.
        match self.stack.last_mut().map(|s| &mut s.frame) {
            Some(Frame::Property { object, nested, .. }) => {
                if *nested || object.is_some() {
                    return Err(self.error("a property element with two objects"));
                }
                *nested = true;
            }
            Some(Frame::Collection { members, .. }) => members.push(subject.clone()),
            _ => {}
        }
        if let Some(Scope {
            frame:
                Frame::Property {
                    subject: s,
                    predicate,
                    reified,
                    annotation,
                    ..
                },
            ..
        }) = self.stack.last()
        {
            let (s, p, r, a) = (
                s.clone(),
                predicate.clone(),
                reified.clone(),
                annotation.clone(),
            );
            self.statement(s, p, subject.clone().into(), r, a);
        }
        if !element.is_rdf("Description") {
            let class = self.name_iri(&element.namespace, &element.local)?;
            self.emit(subject.clone(), rdf::TYPE.into_owned(), class.into());
        }
        for (predicate, object) in properties {
            self.emit(subject.clone(), predicate, object);
        }
        self.stack.push(Scope {
            frame: Frame::Node { subject, li: 0 },
            inherited: scope.inherited,
        });
        Ok(())
    }

    /// A node element's subject, and the statements of its property attributes.
    fn node_subject(
        &mut self,
        element: &Element,
    ) -> Step<(NamedOrBlankNode, Vec<(NamedNode, Term)>)> {
        let mut subject: Option<NamedOrBlankNode> = None;
        let mut properties = Vec::new();
        let inherited = self
            .stack
            .last()
            .map(|s| s.inherited.clone())
            .unwrap_or_default();
        for attribute in &element.attributes {
            if Self::scoped(attribute, &inherited) {
                continue;
            }
            let rdf_name = rdf_attribute(attribute);
            match rdf_name {
                Some("about") | Some("ID") | Some("nodeID") => {
                    if subject.is_some() {
                        return Err(self.error(
                            "a node element with more than one of rdf:about, rdf:ID and rdf:nodeID",
                        ));
                    }
                    subject = Some(match rdf_name {
                        Some("about") => self.resolve(&attribute.value)?.into(),
                        Some("ID") => self.id(&attribute.value)?.into(),
                        _ => self.labelled(&attribute.value)?.into(),
                    });
                }
                Some("type") => properties.push((
                    rdf::TYPE.into_owned(),
                    self.resolve(&attribute.value)?.into(),
                )),
                Some(
                    name @ ("aboutEach" | "aboutEachPrefix" | "bagID" | "li" | "resource"
                    | "parseType" | "datatype" | "annotation" | "annotationNodeID"),
                ) => {
                    return Err(self.error(format!(
                        "rdf:{name} can't be an attribute of a node element"
                    )));
                }
                _ => {
                    if let Some(predicate) = self.property_attribute(attribute)? {
                        properties.push((predicate, inherited.literal(&*attribute.value).into()));
                    }
                }
            }
        }
        let subject = match subject {
            Some(subject) => subject,
            None => self.fresh().into(),
        };
        Ok((subject, properties))
    }

    /// A property attribute's predicate; `None` for attributes RDF ignores (`xml:…`).
    fn property_attribute(&self, attribute: &Attribute) -> Step<Option<NamedNode>> {
        if &*attribute.namespace == XML
            || attribute.local.starts_with("xml") && attribute.namespace.is_empty()
        {
            return Ok(None);
        }
        if attribute.namespace.is_empty() {
            return Err(self.error(format!(
                "the attribute {:?} has no namespace",
                attribute.local
            )));
        }
        if &*attribute.namespace == RDF
            && matches!(
                attribute.local.as_str(),
                "RDF"
                    | "Description"
                    | "li"
                    | "about"
                    | "ID"
                    | "nodeID"
                    | "resource"
                    | "parseType"
                    | "datatype"
                    | "bagID"
                    | "aboutEach"
                    | "aboutEachPrefix"
                    | "annotation"
                    | "annotationNodeID"
                    | "version"
            )
        {
            return Err(self.error(format!(
                "rdf:{} can't be a property attribute",
                attribute.local
            )));
        }
        Ok(Some(self.name_iri(&attribute.namespace, &attribute.local)?))
    }

    /// A property element (§7.2.14–7.2.21).
    fn property_element(&mut self, element: Element, inherited: Inherited) -> Step<()> {
        let Some(Scope {
            frame: Frame::Node { subject, li },
            ..
        }) = self.stack.last_mut()
        else {
            unreachable!("a property element stands in a node element")
        };
        let subject = subject.clone();
        let predicate = if element.is_rdf("li") {
            *li += 1;
            NamedNode::new_unchecked(format!("{RDF}_{li}"))
        } else {
            if &*element.namespace == RDF
                && matches!(
                    element.local.as_str(),
                    "Description"
                        | "RDF"
                        | "ID"
                        | "about"
                        | "bagID"
                        | "parseType"
                        | "resource"
                        | "nodeID"
                        | "datatype"
                        | "aboutEach"
                        | "aboutEachPrefix"
                )
            {
                return Err(
                    self.error(format!("rdf:{} can't be a property element", element.local))
                );
            }
            if element.namespace.is_empty() {
                return Err(self.error(format!("the element {:?} has no namespace", element.local)));
            }
            self.name_iri(&element.namespace, &element.local)?
        };
        self.stack.push(Scope {
            frame: Frame::Rdf,
            inherited,
        });
        let result = self.property_attributes(&element, subject, predicate);
        let scope = self.stack.pop().expect("pushed");
        let frame = result?;
        if matches!(frame, Frame::Triple { .. }) {
            self.capturing += 1;
        }
        self.stack.push(Scope {
            frame,
            inherited: scope.inherited,
        });
        Ok(())
    }

    /// The frame a property element's attributes make.
    fn property_attributes(
        &mut self,
        element: &Element,
        subject: NamedOrBlankNode,
        predicate: NamedNode,
    ) -> Step<Frame> {
        let inherited = self
            .stack
            .last()
            .map(|s| s.inherited.clone())
            .unwrap_or_default();
        let (mut reified, mut parse_type, mut resource, mut node_id, mut datatype) =
            (None, None, None, None, None);
        let mut annotation: Option<NamedOrBlankNode> = None;
        let mut properties = Vec::new();
        for attribute in &element.attributes {
            if Self::scoped(attribute, &inherited) {
                continue;
            }
            match rdf_attribute(attribute) {
                Some(name @ ("annotation" | "annotationNodeID")) => {
                    if annotation.is_some() {
                        return Err(self.error(
                            "a property element with both rdf:annotation and rdf:annotationNodeID",
                        ));
                    }
                    annotation = Some(if name == "annotation" {
                        self.resolve(&attribute.value)?.into()
                    } else {
                        self.labelled(&attribute.value)?.into()
                    });
                }
                Some("ID") => reified = Some(self.id(&attribute.value)?),
                Some("parseType") => parse_type = Some(attribute.value.clone()),
                Some("resource") => resource = Some(self.resolve(&attribute.value)?),
                Some("nodeID") => node_id = Some(self.labelled(&attribute.value)?),
                Some("datatype") => datatype = Some(self.resolve(&attribute.value)?),
                Some("type") => properties.push((
                    rdf::TYPE.into_owned(),
                    Term::from(self.resolve(&attribute.value)?),
                )),
                Some(name @ ("about" | "aboutEach" | "aboutEachPrefix" | "bagID" | "li")) => {
                    return Err(self.error(format!(
                        "rdf:{name} can't be an attribute of a property element"
                    )));
                }
                _ => {
                    if let Some(p) = self.property_attribute(attribute)? {
                        properties.push((p, inherited.literal(&*attribute.value).into()));
                    }
                }
            }
        }
        if resource.is_some() && node_id.is_some() {
            return Err(self.error("a property element with both rdf:resource and rdf:nodeID"));
        }
        if let Some(parse_type) = parse_type {
            if resource.is_some()
                || node_id.is_some()
                || datatype.is_some()
                || !properties.is_empty()
            {
                return Err(self.error("rdf:parseType with other RDF attributes"));
            }
            return Ok(match parse_type.as_str() {
                "Resource" => {
                    // The object is a new node, whose properties the content gives.
                    let node = self.fresh();
                    self.statement(subject, predicate, node.clone().into(), reified, annotation);
                    Frame::Node {
                        subject: node.into(),
                        li: 0,
                    }
                }
                "Collection" => Frame::Collection {
                    subject,
                    predicate,
                    reified,
                    annotation,
                    members: Vec::new(),
                },
                // A triple term (RDF 1.2), ignored altogether before it.
                "Triple" if !inherited.rdf_12() => Frame::Ignored { depth: 0 },
                "Triple" => {
                    if reified.is_some() || annotation.is_some() {
                        return Err(
                            self.error("rdf:parseType=\"Triple\" with other RDF attributes")
                        );
                    }
                    Frame::Triple {
                        subject,
                        predicate,
                        captured: Vec::new(),
                    }
                }
                // "Literal", and any other value.
                _ => Frame::Literal {
                    subject,
                    predicate,
                    reified,
                    annotation,
                    xml: String::new(),
                    open: Vec::new(),
                },
            });
        }
        if datatype.is_some() && (resource.is_some() || node_id.is_some() || !properties.is_empty())
        {
            return Err(
                self.error("rdf:datatype with rdf:resource, rdf:nodeID or property attributes")
            );
        }
        // An object from the attributes: then the element must be empty.
        let object: Option<NamedOrBlankNode> = match (resource, node_id) {
            (Some(r), _) => Some(r.into()),
            (_, Some(b)) => Some(b.into()),
            _ if !properties.is_empty() => Some(self.fresh().into()),
            _ => None,
        };
        if let Some(object) = &object {
            for (p, o) in properties {
                self.emit(object.clone(), p, o);
            }
        }
        Ok(Frame::Property {
            subject,
            predicate,
            reified,
            annotation,
            datatype,
            object,
            text: String::new(),
            nested: false,
        })
    }

    /// A start tag inside an XML literal, written in exclusive canonical form: the
    /// namespaces its name and attributes use, unless an enclosing element of the literal
    /// already declared them alike; then the attributes, sorted by namespace and name.
    fn literal_start(&mut self, start: &BytesStart<'_>) -> Step<()> {
        let name = start.name().as_ref().to_owned();
        // The prefixes the element uses ("" for the default namespace), with their IRIs.
        let mut used: Vec<(String, String)> = Vec::new();
        let namespace_of = |prefix: &str| self.namespace_of(prefix).map(|ns| ns.to_string());
        let prefix: String = start
            .name()
            .prefix()
            .map(|p| p.as_ref().to_owned())
            .unwrap_or_default();
        let element_namespace = namespace_of(&prefix).unwrap_or_default();
        used.push((prefix, element_namespace));
        let mut attributes: Vec<(String, String, String, String)> = Vec::new();
        for attribute in start.attributes().with_checks(true) {
            let attribute =
                attribute.map_err(|e| self.error(format!("a malformed attribute: {e}")))?;
            let key = attribute.key;
            let key_text: &str = key.as_ref();
            if key_text == "xmlns" || key_text.starts_with("xmlns:") {
                continue;
            }
            let value = self.attribute_value(&attribute.value);
            let (namespace, local) = match key.prefix() {
                Some(p) => {
                    let ns = namespace_of(p.as_ref()).unwrap_or_default();
                    let p = p.as_ref().to_owned();
                    if !used.iter().any(|(u, _)| *u == p) {
                        used.push((p, ns.clone()));
                    }
                    (ns, key.local_name().as_ref().to_owned())
                }
                None => (String::new(), key_text.to_owned()),
            };
            let written = key_text.to_owned();
            attributes.push((namespace, local, written, value));
        }
        let Some(Scope {
            frame: Frame::Literal { xml, open, .. },
            ..
        }) = self.stack.last_mut()
        else {
            unreachable!("inside a literal")
        };
        // Declared already by an enclosing element of the literal, alike?
        let in_output = |prefix: &str| -> Option<&str> {
            open.iter()
                .rev()
                .flat_map(|(_, declared)| declared.iter())
                .find(|(p, _)| p == prefix)
                .map(|(_, ns)| ns.as_str())
        };
        let mut declarations: Vec<(String, String)> = used
            .into_iter()
            .filter(|(prefix, ns)| match in_output(prefix) {
                Some(declared) => declared != ns,
                // An unused empty default namespace needs no declaration.
                None => !(prefix.is_empty() && ns.is_empty()) && !(prefix == "xml"),
            })
            .collect();
        declarations.sort();
        attributes.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        xml.push('<');
        xml.push_str(&name);
        for (prefix, ns) in &declarations {
            if prefix.is_empty() {
                xml.push_str(" xmlns=\"");
            } else {
                xml.push_str(" xmlns:");
                xml.push_str(prefix);
                xml.push_str("=\"");
            }
            escape_attribute(ns, xml);
            xml.push('"');
        }
        for (_, _, written, value) in &attributes {
            xml.push(' ');
            xml.push_str(written);
            xml.push_str("=\"");
            escape_attribute(value, xml);
            xml.push('"');
        }
        xml.push('>');
        open.push((name, declarations));
        Ok(())
    }

    fn text(&mut self, text: &str, _verbatim: bool) -> Step<()> {
        match self.stack.last_mut().map(|s| &mut s.frame) {
            Some(Frame::Property {
                text: collected, ..
            }) => {
                collected.push_str(text);
                Ok(())
            }
            Some(Frame::Literal { xml, .. }) => {
                escape_text(text, xml);
                Ok(())
            }
            // An ignored element is ignored with everything in it, its text too.
            Some(Frame::Ignored { .. }) => Ok(()),
            _ if text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r')) => Ok(()),
            _ => Err(self.error("text where only elements may stand")),
        }
    }

    fn end(&mut self) -> Step<()> {
        // Inside an XML literal, an element of the content ends.
        if let Some(Scope {
            frame: Frame::Literal { xml, open, .. },
            ..
        }) = self.stack.last_mut()
            && let Some((name, _)) = open.pop()
        {
            xml.push_str("</");
            xml.push_str(&name);
            xml.push('>');
            return Ok(());
        }
        let Some(scope) = self.stack.pop() else {
            return Err(self.error("an end tag without a start"));
        };
        let inherited = scope.inherited;
        match scope.frame {
            Frame::Rdf | Frame::Node { .. } | Frame::Ignored { .. } => {}
            Frame::Property {
                subject,
                predicate,
                reified,
                annotation,
                datatype,
                object,
                text,
                nested,
            } => {
                if nested {
                    if !text.trim().is_empty() {
                        return Err(self.error("text beside a node element in a property element"));
                    }
                } else if let Some(object) = object {
                    if !text.trim().is_empty() {
                        return Err(self.error(
                            "text in a property element that has rdf:resource or rdf:nodeID",
                        ));
                    }
                    self.statement(subject, predicate, object.into(), reified, annotation);
                } else {
                    let value = match datatype {
                        Some(datatype) => Literal::new_typed_literal(text, datatype),
                        None => inherited.literal(text),
                    };
                    self.statement(subject, predicate, value.into(), reified, annotation);
                }
            }
            Frame::Collection {
                subject,
                predicate,
                reified,
                annotation,
                members,
            } => {
                let mut head: Term = rdf::NIL.into_owned().into();
                let nodes: Vec<BlankNode> = members.iter().map(|_| self.fresh()).collect();
                if let Some(first) = nodes.first() {
                    head = first.clone().into();
                }
                self.statement(subject, predicate, head, reified, annotation);
                for (i, (node, member)) in nodes.iter().zip(&members).enumerate() {
                    self.emit(
                        node.clone().into(),
                        rdf::FIRST.into_owned(),
                        member.clone().into(),
                    );
                    let rest: Term = match nodes.get(i + 1) {
                        Some(next) => next.clone().into(),
                        None => rdf::NIL.into_owned().into(),
                    };
                    self.emit(node.clone().into(), rdf::REST.into_owned(), rest);
                }
            }
            Frame::Literal {
                subject,
                predicate,
                reified,
                annotation,
                xml,
                ..
            } => {
                let value = Literal::new_typed_literal(xml, rdf::XML_LITERAL);
                self.statement(subject, predicate, value.into(), reified, annotation);
            }
            Frame::Triple {
                subject,
                predicate,
                mut captured,
            } => {
                self.capturing -= 1;
                let (Some(quad), true) = (captured.pop(), captured.is_empty()) else {
                    return Err(
                        self.error("rdf:parseType=\"Triple\" must describe exactly one statement")
                    );
                };
                self.emit(subject, predicate, Triple::from(quad).into());
            }
        }
        Ok(())
    }
}

/// The RDF name of an attribute: `rdf:x`, or one of the unqualified names RDF/XML still
/// takes for compatibility (§6.1.4).
fn rdf_attribute(attribute: &Attribute) -> Option<&str> {
    if &*attribute.namespace == RDF {
        return Some(attribute.local.as_str());
    }
    if attribute.namespace.is_empty()
        && matches!(
            attribute.local.as_str(),
            "about" | "ID" | "resource" | "parseType" | "type"
        )
    {
        return Some(attribute.local.as_str());
    }
    None
}

fn predefined(name: &str) -> Option<&'static str> {
    Some(match name {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "apos" => "'",
        "quot" => "\"",
        _ => return None,
    })
}

/// Attribute-value normalisation (XML 1.0 §3.3.3): whitespace characters become spaces.
fn normalize_attribute(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

/// An XML `NCName` (as `rdf:ID` and `rdf:nodeID` must be).
fn is_nc_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let start = |c: char| {
        c.is_alphabetic()
            || c == '_'
            || matches!(c, '\u{C0}'..='\u{2FF}' | '\u{370}'..='\u{1FFF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FFFD}')
    };
    start(first) && chars.all(|c| {
        start(c)
            || c.is_ascii_digit()
            || matches!(c, '-' | '.' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
    })
}

/// Text escaped as canonical XML has it.
fn escape_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            _ => out.push(c),
        }
    }
}

/// An attribute value escaped as canonical XML has it.
fn escape_attribute(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            _ => out.push(c),
        }
    }
}
