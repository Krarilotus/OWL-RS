//! The RDF/XML parser (RDF 1.1 XML Syntax, §7 grammar), on `quick-xml`'s events.
//!
//! A stack of frames, one per open element, says what an element means where it stands: a
//! node element (`rdf:Description` or typed) gives a subject; a property element below it
//! gives a predicate, and an object from its attributes, its text, a nested node element,
//! or `rdf:parseType` (`Resource`, `Collection`, `Literal`). Each frame keeps the `xml:base`
//! and `xml:lang` in scope. Statements go to a queue as they are complete.
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
use nrese_rdf::{BlankNode, GraphName, Iri, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::blank::BlankNodes;
use crate::error::{RdfParseError, RdfSyntaxError, TextPosition};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XML: &str = "http://www.w3.org/XML/1998/namespace";

/// The settings an RDF/XML parser takes from [`crate::RdfParser`].
#[derive(Debug, Clone)]
pub(crate) struct RdfXmlSettings {
    pub(crate) base: Option<Iri<String>>,
    pub(crate) blank_nodes: BlankNodes,
    pub(crate) unchecked: bool,
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
        members: Vec<NamedOrBlankNode>,
    },
    /// `rdf:parseType="Literal"`: the content, as canonical XML.
    Literal {
        subject: NamedOrBlankNode,
        predicate: NamedNode,
        reified: Option<NamedNode>,
        xml: String,
        /// Elements open inside the literal: the name as written, and the namespaces it
        /// declared (prefix, IRI).
        open: Vec<(String, Vec<(String, String)>)>,
    },
}

/// A frame with the scope it opened in.
struct Scope {
    frame: Frame,
    /// Shared with the scopes inside: an element's scope costs no copy of either.
    base: Option<Arc<Iri<String>>>,
    language: Option<Arc<str>>,
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
    pub(crate) current: Option<Quad>,
    done: bool,
}

type Step<T> = Result<T, RdfParseError>;

/// A scope's base and language.
type ScopeParts = (Option<Arc<Iri<String>>>, Option<Arc<str>>);

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
            current: None,
            done: false,
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
                self.done = true;
                self.queue.clear();
                return Err(error);
            }
        }
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
            .map_or(self.settings.base.as_ref(), |s| s.base.as_deref())
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
        Ok(BlankNode::new_unchecked(
            self.settings.blank_nodes.name(label, &mut out),
        ))
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
        self.queue.push_back(Quad::new(
            subject,
            predicate,
            object,
            GraphName::DefaultGraph,
        ));
    }

    /// A statement, and its reification if a property element had `rdf:ID`.
    fn statement(
        &mut self,
        subject: NamedOrBlankNode,
        predicate: NamedNode,
        object: Term,
        reified: Option<NamedNode>,
    ) {
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
        let event = self
            .reader
            .read_event_into(buffer)
            .map_err(|e| self.error(format!("not well-formed XML: {e}")))?;
        match event {
            Event::Start(start) => {
                self.open_namespaces(&start)?;
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
                let ended = self.end();
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

    /// The scope a new element opens: its `xml:base` and `xml:lang`, or those around it.
    fn scope(&self, element: &Element) -> Step<ScopeParts> {
        let (mut base, mut language) = match self.stack.last() {
            Some(scope) => (scope.base.clone(), scope.language.clone()),
            None => (self.settings.base.clone().map(Arc::new), None),
        };
        for attribute in &element.attributes {
            if &*attribute.namespace != XML {
                continue;
            }
            match attribute.local.as_str() {
                "base" => {
                    // The base without its fragment.
                    let reference = attribute.value.split('#').next().unwrap_or("");
                    let resolved = self.resolve(reference)?;
                    base = Some(Arc::new(
                        Iri::parse(resolved.into_string())
                            .map_err(|e| self.error(format!("an invalid xml:base: {e}")))?,
                    ));
                }
                "lang" => {
                    language = (!attribute.value.is_empty())
                        .then(|| Arc::from(attribute.value.to_ascii_lowercase()));
                }
                _ => {}
            }
        }
        Ok((base, language))
    }

    fn start(&mut self, element: Element) -> Step<()> {
        let (base, language) = self.scope(&element)?;
        let expects_node = match self.stack.last().map(|s| &s.frame) {
            None | Some(Frame::Rdf) | Some(Frame::Collection { .. }) => true,
            Some(Frame::Property { .. }) => true,
            Some(Frame::Node { .. }) => false,
            Some(Frame::Literal { .. }) => unreachable!("handled before"),
        };
        if self.stack.is_empty() && element.is_rdf("RDF") {
            self.stack.push(Scope {
                frame: Frame::Rdf,
                base,
                language,
            });
            return Ok(());
        }
        if expects_node {
            self.node_element(element, base, language)
        } else {
            self.property_element(element, base, language)
        }
    }

    /// A node element (§7.2.11).
    fn node_element(
        &mut self,
        element: Element,
        base: Option<Arc<Iri<String>>>,
        language: Option<Arc<str>>,
    ) -> Step<()> {
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
            base,
            language,
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
                    ..
                },
            ..
        }) = self.stack.last()
        {
            let (s, p, r) = (s.clone(), predicate.clone(), reified.clone());
            self.statement(s, p, subject.clone().into(), r);
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
            base: scope.base,
            language: scope.language,
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
        let language = self.stack.last().and_then(|s| s.language.clone());
        for attribute in &element.attributes {
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
                    | "parseType" | "datatype"),
                ) => {
                    return Err(self.error(format!(
                        "rdf:{name} can't be an attribute of a node element"
                    )));
                }
                _ => {
                    if let Some(predicate) = self.property_attribute(attribute)? {
                        properties
                            .push((predicate, literal(&attribute.value, language.as_deref())));
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
    fn property_element(
        &mut self,
        element: Element,
        base: Option<Arc<Iri<String>>>,
        language: Option<Arc<str>>,
    ) -> Step<()> {
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
            base,
            language,
        });
        let result = self.property_attributes(&element, subject, predicate);
        let scope = self.stack.pop().expect("pushed");
        let frame = result?;
        self.stack.push(Scope {
            frame,
            base: scope.base,
            language: scope.language,
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
        let language = self.stack.last().and_then(|s| s.language.clone());
        let (mut reified, mut parse_type, mut resource, mut node_id, mut datatype) =
            (None, None, None, None, None);
        let mut properties = Vec::new();
        for attribute in &element.attributes {
            match rdf_attribute(attribute) {
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
                        properties.push((p, literal(&attribute.value, language.as_deref())));
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
                    self.statement(subject, predicate, node.clone().into(), reified);
                    Frame::Node {
                        subject: node.into(),
                        li: 0,
                    }
                }
                "Collection" => Frame::Collection {
                    subject,
                    predicate,
                    reified,
                    members: Vec::new(),
                },
                // "Literal", and any other value.
                _ => Frame::Literal {
                    subject,
                    predicate,
                    reified,
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
        let language = scope.language.clone();
        match scope.frame {
            Frame::Rdf | Frame::Node { .. } => {}
            Frame::Property {
                subject,
                predicate,
                reified,
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
                    self.statement(subject, predicate, object.into(), reified);
                } else {
                    let value = match datatype {
                        Some(datatype) => Literal::new_typed_literal(text, datatype),
                        None => match &language {
                            Some(tag) => {
                                Literal::new_language_tagged_literal_unchecked(text, &**tag)
                            }
                            None => Literal::new_simple_literal(text),
                        },
                    };
                    self.statement(subject, predicate, value.into(), reified);
                }
            }
            Frame::Collection {
                subject,
                predicate,
                reified,
                members,
            } => {
                let mut head: Term = rdf::NIL.into_owned().into();
                let nodes: Vec<BlankNode> = members.iter().map(|_| self.fresh()).collect();
                if let Some(first) = nodes.first() {
                    head = first.clone().into();
                }
                self.statement(subject, predicate, head, reified);
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
                xml,
                ..
            } => {
                let value = Literal::new_typed_literal(xml, rdf::XML_LITERAL);
                self.statement(subject, predicate, value.into(), reified);
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

fn literal(value: &str, language: Option<&str>) -> Term {
    match language {
        Some(tag) => Literal::new_language_tagged_literal_unchecked(value, tag).into(),
        None => Literal::new_simple_literal(value).into(),
    }
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
