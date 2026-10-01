//! SPARQL Query Results XML: written as bytes (the layout `sparesults` writes), read from
//! `quick-xml` events one solution at a time.
//!
//! Reading goes by local names, so the results namespace may be the default one or have a
//! prefix; `its:dir` is any attribute named `dir` with a prefix.

use std::io::BufRead;
use std::sync::Arc;

use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{
    BlankNode, Literal, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, Term, TermRef, Triple,
    Variable,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::error::{QueryResultsParseError, QueryResultsSyntaxError};

const NAMESPACE: &str = "http://www.w3.org/2005/sparql-results#";
const ITS: &str = "http://www.w3.org/2005/11/its";

// ---------------------------------------------------------------------------------------
// Writing

/// `text` with `& < > ' "` escaped.
fn escaped(out: &mut Vec<u8>, text: &str) {
    let mut run = 0;
    for (i, b) in text.bytes().enumerate() {
        let entity: &[u8] = match b {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b'\'' => b"&apos;",
            b'"' => b"&quot;",
            _ => continue,
        };
        out.extend_from_slice(&text.as_bytes()[run..i]);
        out.extend_from_slice(entity);
        run = i + 1;
    }
    out.extend_from_slice(&text.as_bytes()[run..]);
}

/// A literal's text: escaped, and whitespace at either end as character references, so
/// that no reader takes it for layout.
fn literal_text(out: &mut Vec<u8>, text: &str) {
    let blank = |c: char| matches!(c, '\t' | '\n' | '\r' | ' ');
    let trimmed = text.trim_matches(blank);
    let reference = |out: &mut Vec<u8>, c: char| {
        out.extend_from_slice(match c {
            '\t' => b"&#9;",
            '\n' => b"&#10;",
            '\r' => b"&#13;",
            _ => b"&#32;",
        });
    };
    let leading = text.len() - text.trim_start_matches(blank).len();
    text[..leading].chars().for_each(|c| reference(out, c));
    escaped(out, trimmed);
    if !trimmed.is_empty() {
        let end = leading + trimmed.len();
        text[end..].chars().for_each(|c| reference(out, c));
    }
}

pub(crate) fn write_boolean(out: &mut Vec<u8>, value: bool) {
    out.extend_from_slice(b"<?xml version=\"1.0\"?><sparql xmlns=\"");
    out.extend_from_slice(NAMESPACE.as_bytes());
    out.extend_from_slice(b"\"><head></head><boolean>");
    out.extend_from_slice(if value { b"true" } else { b"false" });
    out.extend_from_slice(b"</boolean></sparql>");
}

pub(crate) fn write_head(out: &mut Vec<u8>, variables: &[Variable]) {
    out.extend_from_slice(b"<?xml version=\"1.0\"?><sparql xmlns=\"");
    out.extend_from_slice(NAMESPACE.as_bytes());
    out.extend_from_slice(b"\"><head>");
    for variable in variables {
        out.extend_from_slice(b"<variable name=\"");
        escaped(out, variable.as_str());
        out.extend_from_slice(b"\"/>");
    }
    out.extend_from_slice(b"</head><results>");
}

pub(crate) fn write_row(out: &mut Vec<u8>, variables: &[Variable], row: &[Option<TermRef<'_>>]) {
    out.extend_from_slice(b"<result>");
    for (variable, value) in variables.iter().zip(row) {
        let Some(value) = value else { continue };
        out.extend_from_slice(b"<binding name=\"");
        escaped(out, variable.as_str());
        out.extend_from_slice(b"\">");
        write_term(out, *value);
        out.extend_from_slice(b"</binding>");
    }
    out.extend_from_slice(b"</result>");
}

pub(crate) fn write_end(out: &mut Vec<u8>) {
    out.extend_from_slice(b"</results></sparql>");
}

fn write_term(out: &mut Vec<u8>, term: TermRef<'_>) {
    match term {
        TermRef::NamedNode(iri) => {
            out.extend_from_slice(b"<uri>");
            escaped(out, iri.as_str());
            out.extend_from_slice(b"</uri>");
        }
        TermRef::BlankNode(b) => {
            out.extend_from_slice(b"<bnode>");
            escaped(out, b.as_str());
            out.extend_from_slice(b"</bnode>");
        }
        TermRef::Literal(literal) => {
            out.extend_from_slice(b"<literal");
            if let Some(language) = literal.language() {
                out.extend_from_slice(b" xml:lang=\"");
                escaped(out, language);
                out.push(b'"');
                if let Some(direction) = literal.direction() {
                    out.extend_from_slice(b" its:dir=\"");
                    out.extend_from_slice(direction.as_str().as_bytes());
                    out.extend_from_slice(b"\" xmlns:its=\"");
                    out.extend_from_slice(ITS.as_bytes());
                    out.extend_from_slice(b"\" its:version=\"2.0\"");
                }
            } else if literal.datatype() != xsd::STRING {
                out.extend_from_slice(b" datatype=\"");
                escaped(out, literal.datatype().as_str());
                out.push(b'"');
            }
            out.push(b'>');
            literal_text(out, literal.value());
            out.extend_from_slice(b"</literal>");
        }
        TermRef::Triple(triple) => {
            out.extend_from_slice(b"<triple><subject>");
            write_term(out, NamedOrBlankNodeRef::from(&triple.subject).into());
            out.extend_from_slice(b"</subject><predicate>");
            write_term(out, (&triple.predicate).into());
            out.extend_from_slice(b"</predicate><object>");
            write_term(out, (&triple.object).into());
            out.extend_from_slice(b"</object></triple>");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Reading

fn syntax(message: impl Into<String>) -> QueryResultsParseError {
    QueryResultsSyntaxError::msg(message).into()
}

/// The local part of a qualified name.
fn local(name: &[u8]) -> &[u8] {
    name.iter()
        .position(|&b| b == b':')
        .map_or(name, |i| &name[i + 1..])
}

/// The elements of the format, by local name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    Sparql,
    Head,
    Variable,
    Boolean,
    Results,
    Result,
    Binding,
    Uri,
    Bnode,
    Literal,
    Triple,
    Subject,
    Predicate,
    Object,
    Other,
}

impl Tag {
    fn of(name: &[u8]) -> Self {
        match local(name) {
            b"sparql" => Self::Sparql,
            b"head" => Self::Head,
            b"variable" => Self::Variable,
            b"boolean" => Self::Boolean,
            b"results" => Self::Results,
            b"result" => Self::Result,
            b"binding" => Self::Binding,
            b"uri" => Self::Uri,
            b"bnode" => Self::Bnode,
            b"literal" => Self::Literal,
            b"triple" => Self::Triple,
            b"subject" => Self::Subject,
            b"predicate" => Self::Predicate,
            b"object" => Self::Object,
            _ => Self::Other,
        }
    }
}

/// What an element or the end of one is, without the layout between them. The name and
/// attributes of a start are kept in the reader's reused buffers until the next item.
enum Item {
    Start(Tag),
    End,
    Eof,
}

/// How deep triple terms may nest: deeper input is an error, not a stack overflow.
const MAX_NESTING: usize = 64;

/// What the start of a document says.
pub(crate) enum Start {
    Boolean(bool),
    Solutions,
}

/// The attributes of the last element start: names and values one after the other in
/// two buffers, and where each ends. Reused, so reading allocates nothing per element.
#[derive(Default)]
struct Attributes {
    keys: Vec<u8>,
    values: String,
    /// The end of each key in `keys` and of each value in `values`.
    ends: Vec<(usize, usize)>,
}

impl Attributes {
    fn clear(&mut self) {
        self.keys.clear();
        self.values.clear();
        self.ends.clear();
    }

    fn iter(&self) -> impl Iterator<Item = (&[u8], &str)> {
        let mut start = (0, 0);
        self.ends.iter().map(move |&(key, value)| {
            let item = (&self.keys[start.0..key], &self.values[start.1..value]);
            start = (key, value);
            item
        })
    }

    fn get(&self, name: &[u8]) -> Option<&str> {
        self.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    }

    fn read(&mut self, start: &BytesStart<'_>) -> Result<(), QueryResultsParseError> {
        self.clear();
        for attribute in start.attributes().with_checks(true) {
            let attribute = attribute.map_err(|e| syntax(format!("a malformed attribute: {e}")))?;
            let value = attribute
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|e| syntax(format!("an invalid attribute value: {e}")))?;
            self.keys
                .extend_from_slice(attribute.key.as_ref().as_bytes());
            self.values.push_str(&value);
            self.ends.push((self.keys.len(), self.values.len()));
        }
        Ok(())
    }
}

/// Solutions read from an XML document.
pub(crate) struct XmlSolutions<R: BufRead> {
    reader: Reader<R>,
    buffer: Vec<u8>,
    /// The name of the last element start.
    name: Vec<u8>,
    attributes: Attributes,
    variables: Arc<[Variable]>,
    done: bool,
}

impl<R: BufRead> XmlSolutions<R> {
    /// Reads up to the first result (or the whole document, for a boolean).
    pub(crate) fn start(input: R) -> Result<(Start, Self), QueryResultsParseError> {
        let mut reader = Reader::from_reader(input);
        let config = reader.config_mut();
        config.expand_empty_elements = true;
        config.check_end_names = true;
        let mut this = Self {
            reader,
            buffer: Vec::new(),
            name: Vec::new(),
            attributes: Attributes::default(),
            variables: Arc::from(Vec::new()),
            done: false,
        };
        if !matches!(this.item()?, Item::Start(Tag::Sparql)) {
            return Err(syntax("expected <sparql>"));
        }
        let mut variables = Vec::new();
        loop {
            match this.item()? {
                Item::Start(Tag::Head) => loop {
                    match this.item()? {
                        Item::Start(tag) => {
                            if tag == Tag::Variable {
                                let name = this
                                    .attributes
                                    .get(b"name")
                                    .ok_or_else(|| syntax("<variable> without name"))?;
                                let variable = Variable::new(name)
                                    .map_err(|e| syntax(format!("an invalid variable: {e}")))?;
                                if variables.contains(&variable) {
                                    return Err(syntax(format!("the variable {variable} twice")));
                                }
                                variables.push(variable);
                            }
                            this.skip()?;
                        }
                        Item::End => break,
                        Item::Eof => return Err(syntax("the document ends in <head>")),
                    }
                },
                Item::Start(Tag::Boolean) => {
                    let text = this.text()?;
                    let value = match text.trim() {
                        "true" | "1" => true,
                        "false" | "0" => false,
                        other => return Err(syntax(format!("not a boolean: {other:?}"))),
                    };
                    this.finish()?;
                    return Ok((Start::Boolean(value), this));
                }
                Item::Start(Tag::Results) => {
                    this.variables = variables.into();
                    return Ok((Start::Solutions, this));
                }
                Item::Start(_) => this.skip()?,
                Item::End | Item::Eof => {
                    return Err(syntax("neither <boolean> nor <results>"));
                }
            }
        }
    }

    pub(crate) fn variables(&self) -> &Arc<[Variable]> {
        &self.variables
    }

    /// The next solution's values, or `None` after the last.
    pub(crate) fn next_row(&mut self) -> Result<Option<Vec<Option<Term>>>, QueryResultsParseError> {
        if self.done {
            return Ok(None);
        }
        loop {
            match self.item()? {
                Item::Start(Tag::Result) => break,
                Item::Start(_) => self.skip()?,
                Item::End => {
                    // </results>: the rest of the document.
                    self.done = true;
                    self.finish()?;
                    return Ok(None);
                }
                Item::Eof => return Err(syntax("the document ends in <results>")),
            }
        }
        let mut row = vec![None; self.variables.len()];
        // Bindings usually come in the variables' order: try the next one first.
        let mut next = 0;
        loop {
            match self.item()? {
                Item::Start(Tag::Binding) => {
                    let variable = self
                        .attributes
                        .get(b"name")
                        .ok_or_else(|| syntax("<binding> without name"))?;
                    let i = match self.variables.get(next) {
                        Some(v) if v.as_str() == variable => next,
                        _ => self
                            .variables
                            .iter()
                            .position(|v| v.as_str() == variable)
                            .ok_or_else(|| {
                                syntax(format!("the variable {variable} isn't in <head>"))
                            })?,
                    };
                    next = i + 1;
                    let term = self.term_element(0)?;
                    match self.item()? {
                        Item::End => {}
                        _ => return Err(syntax("more than one term in a <binding>")),
                    }
                    if row[i].replace(term).is_some() {
                        return Err(syntax(format!(
                            "the variable {} twice in a result",
                            self.variables[i]
                        )));
                    }
                }
                Item::Start(_) => self.skip()?,
                Item::End => return Ok(Some(row)),
                Item::Eof => return Err(syntax("the document ends in <result>")),
            }
        }
    }

    /// The next term element and its content.
    fn term_element(&mut self, depth: usize) -> Result<Term, QueryResultsParseError> {
        let Item::Start(tag) = self.item()? else {
            return Err(syntax("expected a term"));
        };
        Ok(match tag {
            Tag::Uri => NamedNode::new(self.text()?)
                .map_err(|e| syntax(format!("an invalid IRI: {e}")))?
                .into(),
            Tag::Bnode => BlankNode::new(self.text()?)
                .map_err(|e| syntax(format!("an invalid blank node: {e}")))?
                .into(),
            Tag::Literal => {
                // The attributes before the text: reading it doesn't touch them, but they
                // are needed owned anyway.
                let language = self.attributes.get(b"xml:lang").map(str::to_owned);
                let direction = self
                    .attributes
                    .iter()
                    .find(|(key, _)| {
                        *key != b"xml:lang" && key.contains(&b':') && local(key) == b"dir"
                    })
                    .map(|(_, value)| value.parse());
                let datatype = self.attributes.get(b"datatype").map(str::to_owned);
                let value = self.text()?;
                match (language, direction, datatype) {
                    (Some(language), Some(direction), _) => {
                        let direction = direction
                            .map_err(|e: nrese_rdf::TermParseError| syntax(e.to_string()))?;
                        Literal::new_directional_language_tagged_literal(value, language, direction)
                            .map_err(|e| syntax(e.to_string()))?
                            .into()
                    }
                    (Some(language), None, _) => {
                        Literal::new_language_tagged_literal(value, language)
                            .map_err(|e| syntax(e.to_string()))?
                            .into()
                    }
                    (None, _, Some(datatype)) => {
                        let datatype = NamedNode::new(datatype)
                            .map_err(|e| syntax(format!("an invalid datatype: {e}")))?;
                        if datatype == rdf::LANG_STRING || datatype == rdf::DIR_LANG_STRING {
                            return Err(syntax("a language-string datatype without xml:lang"));
                        }
                        Literal::new_typed_literal(value, datatype).into()
                    }
                    (None, _, None) => Literal::new_simple_literal(value).into(),
                }
            }
            Tag::Triple => {
                if depth >= MAX_NESTING {
                    return Err(syntax("triple terms nested too deeply"));
                }
                let (mut subject, mut predicate, mut object) = (None, None, None);
                loop {
                    match self.item()? {
                        Item::Start(part) => {
                            let term = self.term_element(depth + 1)?;
                            if !matches!(self.item()?, Item::End) {
                                return Err(syntax("more than one term in a part of a triple"));
                            }
                            match part {
                                Tag::Subject => subject = Some(term),
                                Tag::Predicate => predicate = Some(term),
                                Tag::Object => object = Some(term),
                                _ => return Err(syntax("an unknown part of a triple")),
                            }
                        }
                        Item::End => break,
                        Item::Eof => return Err(syntax("the document ends in <triple>")),
                    }
                }
                let subject = NamedOrBlankNode::try_from(
                    subject.ok_or_else(|| syntax("a triple without subject"))?,
                )
                .map_err(|e| syntax(format!("a triple's subject: {e}")))?;
                let Some(Term::NamedNode(predicate)) = predicate else {
                    return Err(syntax("a triple's predicate must be an IRI"));
                };
                let object = object.ok_or_else(|| syntax("a triple without object"))?;
                Triple::new(subject, predicate, object).into()
            }
            _ => {
                return Err(syntax(format!(
                    "an unknown term <{}>",
                    String::from_utf8_lossy(local(&self.name))
                )));
            }
        })
    }

    /// The next element start or end, skipping layout, comments and processing
    /// instructions.
    fn item(&mut self) -> Result<Item, QueryResultsParseError> {
        loop {
            self.buffer.clear();
            match self.reader.read_event_into(&mut self.buffer)? {
                Event::Start(start) => {
                    self.name.clear();
                    self.name
                        .extend_from_slice(start.name().as_ref().as_bytes());
                    self.attributes.read(&start)?;
                    return Ok(Item::Start(Tag::of(&self.name)));
                }
                Event::End(_) => return Ok(Item::End),
                Event::Eof => return Ok(Item::Eof),
                Event::Text(text) => {
                    if !text.bytes().all(|b| b.is_ascii_whitespace()) {
                        return Err(syntax("text where only elements may stand"));
                    }
                }
                Event::CData(_) | Event::GeneralRef(_) => {
                    return Err(syntax("text where only elements may stand"));
                }
                _ => {}
            }
        }
    }

    /// The text up to the end of the current element (references resolved).
    fn text(&mut self) -> Result<String, QueryResultsParseError> {
        let mut out = String::new();
        loop {
            self.buffer.clear();
            match self.reader.read_event_into(&mut self.buffer)? {
                Event::Text(text) => {
                    let text = text.xml10_content();
                    if out.is_empty() {
                        // Most terms are one piece of text: take it without a copy when
                        // it is already owned.
                        out = text.into_owned();
                    } else {
                        out.push_str(&text);
                    }
                }
                Event::CData(data) => out.push_str(&data.xml10_content()),
                Event::GeneralRef(reference) => match reference.resolve_char_ref() {
                    Ok(Some(c)) => out.push(c),
                    Ok(None) => {
                        let name = reference.xml10_content();
                        out.push_str(match name.as_ref() {
                            "lt" => "<",
                            "gt" => ">",
                            "amp" => "&",
                            "apos" => "'",
                            "quot" => "\"",
                            other => return Err(syntax(format!("an undeclared entity &{other};"))),
                        });
                    }
                    Err(e) => return Err(syntax(format!("an invalid character reference: {e}"))),
                },
                Event::End(_) => return Ok(out),
                Event::Start(_) => return Err(syntax("an element inside a term's text")),
                Event::Eof => return Err(syntax("the document ends in a term")),
                _ => {}
            }
        }
    }

    /// Skips the rest of the current element.
    fn skip(&mut self) -> Result<(), QueryResultsParseError> {
        let mut depth = 1_usize;
        loop {
            self.buffer.clear();
            match self.reader.read_event_into(&mut self.buffer)? {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Event::Eof => return Err(syntax("the document ends inside an element")),
                _ => {}
            }
        }
    }

    /// The rest of the document after the results: the end of `<sparql>` and nothing
    /// more.
    fn finish(&mut self) -> Result<(), QueryResultsParseError> {
        loop {
            match self.item()? {
                Item::Start(_) => self.skip()?,
                Item::End => break,
                Item::Eof => return Err(syntax("the document ends inside <sparql>")),
            }
        }
        match self.item()? {
            Item::Eof => Ok(()),
            _ => Err(syntax("content after </sparql>")),
        }
    }
}
