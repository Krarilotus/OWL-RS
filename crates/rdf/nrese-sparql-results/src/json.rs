//! SPARQL Query Results JSON: written compactly (the bytes `sparesults` writes), read with
//! `nrese-json`'s pull parser one solution at a time.
//!
//! Reading takes the members of the top object in any order. The usual one (`head`, then
//! `results`) streams; `results` before `head` is legal JSON too, and then the bindings are
//! collected until the variables are known.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::Arc;

use nrese_json::{JsonEvent, ReaderJsonParser, SliceJsonParser, escape};
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{
    BlankNode, Literal, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, Term, TermRef, Triple,
    Variable,
};

use crate::error::{QueryResultsParseError, QueryResultsSyntaxError};

// ---------------------------------------------------------------------------------------
// Writing

fn string(out: &mut Vec<u8>, text: &str) {
    escape(text, |piece| out.extend_from_slice(piece.as_bytes()));
}

pub(crate) fn write_boolean(out: &mut Vec<u8>, value: bool) {
    out.extend_from_slice(if value {
        b"{\"head\":{},\"boolean\":true}"
    } else {
        b"{\"head\":{},\"boolean\":false}"
    });
}

pub(crate) fn write_head(out: &mut Vec<u8>, variables: &[Variable]) {
    out.extend_from_slice(b"{\"head\":{\"vars\":[");
    for (i, variable) in variables.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        string(out, variable.as_str());
    }
    out.extend_from_slice(b"]},\"results\":{\"bindings\":[");
}

pub(crate) fn write_row(
    out: &mut Vec<u8>,
    variables: &[Variable],
    row: &[Option<TermRef<'_>>],
    first: bool,
) {
    if !first {
        out.push(b',');
    }
    out.push(b'{');
    let mut separator = false;
    for (variable, value) in variables.iter().zip(row) {
        let Some(value) = value else { continue };
        if std::mem::replace(&mut separator, true) {
            out.push(b',');
        }
        string(out, variable.as_str());
        out.push(b':');
        write_term(out, *value);
    }
    out.push(b'}');
}

pub(crate) fn write_end(out: &mut Vec<u8>) {
    out.extend_from_slice(b"]}}");
}

pub(crate) fn write_term(out: &mut Vec<u8>, term: TermRef<'_>) {
    match term {
        TermRef::NamedNode(iri) => {
            out.extend_from_slice(b"{\"type\":\"uri\",\"value\":");
            string(out, iri.as_str());
        }
        TermRef::BlankNode(b) => {
            out.extend_from_slice(b"{\"type\":\"bnode\",\"value\":");
            string(out, b.as_str());
        }
        TermRef::Literal(literal) => {
            out.extend_from_slice(b"{\"type\":\"literal\",\"value\":");
            string(out, literal.value());
            if let Some(language) = literal.language() {
                out.extend_from_slice(b",\"xml:lang\":");
                string(out, language);
                if let Some(direction) = literal.direction() {
                    out.extend_from_slice(b",\"its:dir\":");
                    string(out, direction.as_str());
                }
            } else if literal.datatype() != xsd::STRING {
                out.extend_from_slice(b",\"datatype\":");
                string(out, literal.datatype().as_str());
            }
        }
        TermRef::Triple(triple) => {
            out.extend_from_slice(b"{\"type\":\"triple\",\"value\":{\"subject\":");
            write_term(out, NamedOrBlankNodeRef::from(&triple.subject).into());
            out.extend_from_slice(b",\"predicate\":");
            write_term(out, (&triple.predicate).into());
            out.extend_from_slice(b",\"object\":");
            write_term(out, (&triple.object).into());
            out.push(b'}');
        }
    }
    out.push(b'}');
}

// ---------------------------------------------------------------------------------------
// Reading

/// The events of a document, from memory or a reader.
pub(crate) trait Events {
    fn next(&mut self) -> Result<JsonEvent<'_>, QueryResultsParseError>;
}

impl Events for SliceJsonParser<'_> {
    fn next(&mut self) -> Result<JsonEvent<'_>, QueryResultsParseError> {
        Ok(self.next_event()?)
    }
}

impl<R: Read> Events for ReaderJsonParser<R> {
    fn next(&mut self) -> Result<JsonEvent<'_>, QueryResultsParseError> {
        Ok(self.next_event()?)
    }
}

fn syntax(message: impl Into<String>) -> QueryResultsParseError {
    QueryResultsSyntaxError::msg(message).into()
}

/// What the start of a document says.
pub(crate) enum Start {
    Boolean(bool),
    Solutions,
}

/// Solutions read from a JSON document.
pub(crate) struct JsonSolutions<E: Events> {
    events: E,
    variables: Arc<[Variable]>,
    /// Rows collected before the head (`results` before `head`), handed out first.
    buffered: VecDeque<Vec<Option<Term>>>,
    /// Inside the bindings array.
    streaming: bool,
    done: bool,
}

/// How deep triple terms may nest: deeper input is an error, not a stack overflow.
const MAX_NESTING: usize = 64;

impl<E: Events> JsonSolutions<E> {
    /// Reads up to the first binding (or the whole document, for a boolean or when the
    /// bindings come before the head).
    pub(crate) fn start(mut events: E) -> Result<(Start, Self), QueryResultsParseError> {
        if !matches!(events.next()?, JsonEvent::StartObject) {
            return Err(syntax("SPARQL JSON results must be an object"));
        }
        let mut variables: Option<Vec<Variable>> = None;
        let mut boolean = None;
        let mut early: Vec<Vec<(String, Term)>> = Vec::new();
        let mut saw_results = false;
        loop {
            let key = match events.next()? {
                JsonEvent::ObjectKey(key) => key.into_owned(),
                JsonEvent::EndObject => break,
                _ => return Err(syntax("expected a member of the results object")),
            };
            match key.as_str() {
                "head" => variables = Some(read_head(&mut events)?),
                "boolean" => match events.next()? {
                    JsonEvent::Boolean(value) => boolean = Some(value),
                    _ => return Err(syntax("'boolean' must be true or false")),
                },
                "results" => {
                    saw_results = true;
                    if !matches!(events.next()?, JsonEvent::StartObject) {
                        return Err(syntax("'results' must be an object"));
                    }
                    loop {
                        let key = match events.next()? {
                            JsonEvent::ObjectKey(key) => key.into_owned(),
                            JsonEvent::EndObject => break,
                            _ => return Err(syntax("expected a member of 'results'")),
                        };
                        if key != "bindings" {
                            skip_value(&mut events)?;
                            continue;
                        }
                        if !matches!(events.next()?, JsonEvent::StartArray) {
                            return Err(syntax("'bindings' must be an array"));
                        }
                        if let Some(variables) = &variables {
                            // The usual order: stream from here.
                            let variables: Arc<[Variable]> = variables.clone().into();
                            return Ok((
                                Start::Solutions,
                                Self {
                                    events,
                                    variables,
                                    buffered: VecDeque::new(),
                                    streaming: true,
                                    done: false,
                                },
                            ));
                        }
                        loop {
                            match events.next()? {
                                JsonEvent::StartObject => early.push(read_binding(&mut events)?),
                                JsonEvent::EndArray => break,
                                _ => return Err(syntax("a binding must be an object")),
                            }
                        }
                    }
                }
                _ => skip_value(&mut events)?,
            }
        }
        if !matches!(events.next()?, JsonEvent::Eof) {
            return Err(syntax("text after the results object"));
        }
        let (start, variables): (Start, Arc<[Variable]>) = match (boolean, variables) {
            (Some(value), _) if !saw_results => (Start::Boolean(value), Arc::from(Vec::new())),
            (_, Some(variables)) => (Start::Solutions, variables.into()),
            (None, None) => return Err(syntax("neither 'boolean' nor 'head' and 'results'")),
            (Some(_), None) => return Err(syntax("'results' without 'head'")),
        };
        let mut buffered = VecDeque::with_capacity(early.len());
        for binding in early {
            buffered.push_back(place(&variables, binding)?);
        }
        Ok((
            start,
            Self {
                events,
                variables,
                buffered,
                streaming: false,
                done: true,
            },
        ))
    }

    pub(crate) fn variables(&self) -> &Arc<[Variable]> {
        &self.variables
    }

    /// The next solution's values, or `None` after the last.
    pub(crate) fn next_row(&mut self) -> Result<Option<Vec<Option<Term>>>, QueryResultsParseError> {
        if let Some(row) = self.buffered.pop_front() {
            return Ok(Some(row));
        }
        if self.done || !self.streaming {
            return Ok(None);
        }
        match self.events.next()? {
            JsonEvent::StartObject => read_row(&mut self.events, &self.variables).map(Some),
            JsonEvent::EndArray => {
                self.done = true;
                // The rest of `results`, and of the document.
                let mut depth = 2;
                loop {
                    match self.events.next()? {
                        JsonEvent::StartObject | JsonEvent::StartArray => depth += 1,
                        JsonEvent::EndObject | JsonEvent::EndArray => depth -= 1,
                        JsonEvent::Eof if depth == 0 => return Ok(None),
                        JsonEvent::Eof => return Err(syntax("the document ends inside 'results'")),
                        _ => {}
                    }
                }
            }
            _ => Err(syntax("a binding must be an object")),
        }
    }
}

/// `head`: `{"vars": [...], "link": [...]}`.
fn read_head(events: &mut impl Events) -> Result<Vec<Variable>, QueryResultsParseError> {
    if !matches!(events.next()?, JsonEvent::StartObject) {
        return Err(syntax("'head' must be an object"));
    }
    let mut variables = Vec::new();
    loop {
        let key = match events.next()? {
            JsonEvent::ObjectKey(key) => key.into_owned(),
            JsonEvent::EndObject => return Ok(variables),
            _ => return Err(syntax("expected a member of 'head'")),
        };
        if key != "vars" {
            skip_value(events)?;
            continue;
        }
        if !matches!(events.next()?, JsonEvent::StartArray) {
            return Err(syntax("'vars' must be an array"));
        }
        loop {
            match events.next()? {
                JsonEvent::String(name) => {
                    let variable = Variable::new(name.as_ref())
                        .map_err(|e| syntax(format!("an invalid variable: {e}")))?;
                    if variables.contains(&variable) {
                        return Err(syntax(format!("the variable {variable} twice in 'vars'")));
                    }
                    variables.push(variable);
                }
                JsonEvent::EndArray => break,
                _ => return Err(syntax("a variable must be a string")),
            }
        }
    }
}

/// A binding object after its `{`: variable names and terms.
fn read_binding(events: &mut impl Events) -> Result<Vec<(String, Term)>, QueryResultsParseError> {
    let mut binding = Vec::new();
    loop {
        let name = match events.next()? {
            JsonEvent::ObjectKey(name) => name.into_owned(),
            JsonEvent::EndObject => return Ok(binding),
            _ => return Err(syntax("expected a variable of the binding")),
        };
        if !matches!(events.next()?, JsonEvent::StartObject) {
            return Err(syntax(format!("the value of {name} must be an object")));
        }
        binding.push((name, read_term(events, 0)?));
    }
}

/// A binding object after its `{`, its values put in the order of `variables` as they
/// come (no name is copied).
fn read_row(
    events: &mut impl Events,
    variables: &[Variable],
) -> Result<Vec<Option<Term>>, QueryResultsParseError> {
    let mut row = vec![None; variables.len()];
    // Values usually come in the variables' order: try the next one first.
    let mut next = 0;
    loop {
        let i = match events.next()? {
            JsonEvent::ObjectKey(name) => match variables.get(next) {
                Some(v) if v.as_str() == name => next,
                _ => variables
                    .iter()
                    .position(|v| v.as_str() == name)
                    .ok_or_else(|| syntax(format!("the variable {name} isn't in 'head'")))?,
            },
            JsonEvent::EndObject => return Ok(row),
            _ => return Err(syntax("expected a variable of the binding")),
        };
        next = i + 1;
        if !matches!(events.next()?, JsonEvent::StartObject) {
            return Err(syntax(format!(
                "the value of {} must be an object",
                variables[i]
            )));
        }
        let term = read_term(events, 0)?;
        if row[i].replace(term).is_some() {
            return Err(syntax(format!(
                "the variable {} twice in a binding",
                variables[i]
            )));
        }
    }
}

/// The values of a binding in the order of `variables`.
fn place(
    variables: &[Variable],
    binding: Vec<(String, Term)>,
) -> Result<Vec<Option<Term>>, QueryResultsParseError> {
    let mut row = vec![None; variables.len()];
    for (name, term) in binding {
        let i = variables
            .iter()
            .position(|v| v.as_str() == name)
            .ok_or_else(|| syntax(format!("the variable {name} isn't in 'head'")))?;
        if row[i].replace(term).is_some() {
            return Err(syntax(format!("the variable {name} twice in a binding")));
        }
    }
    Ok(row)
}

/// The `type` of a term object.
enum TermType {
    Uri,
    Bnode,
    Literal,
    Triple,
    Unknown(String),
}

/// A term object after its `{`.
fn read_term(events: &mut impl Events, depth: usize) -> Result<Term, QueryResultsParseError> {
    enum Value {
        Text(String),
        Triple(Box<Triple>),
    }
    /// The members of a term object.
    enum Member {
        Type,
        Value,
        Datatype,
        Language,
        Direction,
        Other,
    }
    let (mut kind, mut value, mut datatype, mut language, mut direction) =
        (None, None, None, None, None);
    loop {
        let member = match events.next()? {
            JsonEvent::ObjectKey(key) => match key.as_ref() {
                "type" => Member::Type,
                "value" => Member::Value,
                "datatype" => Member::Datatype,
                "xml:lang" => Member::Language,
                "its:dir" => Member::Direction,
                _ => Member::Other,
            },
            JsonEvent::EndObject => break,
            _ => return Err(syntax("expected a member of a term")),
        };
        if let Member::Value = member {
            value = Some(match events.next()? {
                JsonEvent::String(text) => Value::Text(text.into_owned()),
                JsonEvent::StartObject => {
                    if depth >= MAX_NESTING {
                        return Err(syntax("triple terms nested too deeply"));
                    }
                    Value::Triple(Box::new(read_triple(events, depth + 1)?))
                }
                _ => return Err(syntax("a term's value must be a string or a triple")),
            });
            continue;
        }
        let JsonEvent::String(text) = events.next()? else {
            return Err(syntax("the members of a term must be strings"));
        };
        match member {
            Member::Type => {
                kind = Some(match text.as_ref() {
                    "uri" => TermType::Uri,
                    "bnode" => TermType::Bnode,
                    "literal" | "typed-literal" => TermType::Literal,
                    "triple" => TermType::Triple,
                    _ => TermType::Unknown(text.into_owned()),
                });
            }
            Member::Datatype => datatype = Some(text.into_owned()),
            Member::Language => language = Some(text.into_owned()),
            Member::Direction => direction = Some(text.into_owned()),
            Member::Value | Member::Other => {}
        }
    }
    let kind = kind.ok_or_else(|| syntax("a term without 'type'"))?;
    let value = value.ok_or_else(|| syntax("a term without 'value'"))?;
    let text = |value: Value| match value {
        Value::Text(text) => Ok(text),
        Value::Triple(_) => Err(syntax(
            "an IRI, blank node or literal with a triple as value",
        )),
    };
    match kind {
        TermType::Uri => Ok(NamedNode::new(text(value)?)
            .map_err(|e| syntax(format!("an invalid IRI: {e}")))?
            .into()),
        TermType::Bnode => Ok(BlankNode::new(text(value)?)
            .map_err(|e| syntax(format!("an invalid blank node: {e}")))?
            .into()),
        TermType::Literal => {
            let value = text(value)?;
            match (language, datatype, direction) {
                (Some(language), _, None) => Literal::new_language_tagged_literal(value, language)
                    .map(Term::from)
                    .map_err(|e| syntax(e.to_string())),
                (Some(language), _, Some(direction)) => {
                    let direction = direction
                        .parse()
                        .map_err(|e: nrese_rdf::TermParseError| syntax(e.to_string()))?;
                    Literal::new_directional_language_tagged_literal(value, language, direction)
                        .map(Term::from)
                        .map_err(|e| syntax(e.to_string()))
                }
                (None, Some(datatype), _) => {
                    let datatype = NamedNode::new(datatype)
                        .map_err(|e| syntax(format!("an invalid datatype: {e}")))?;
                    if datatype == rdf::LANG_STRING || datatype == rdf::DIR_LANG_STRING {
                        return Err(syntax("a language-string datatype without 'xml:lang'"));
                    }
                    Ok(Literal::new_typed_literal(value, datatype).into())
                }
                (None, None, _) => Ok(Literal::new_simple_literal(value).into()),
            }
        }
        TermType::Triple => match value {
            Value::Triple(triple) => Ok(Term::Triple(triple)),
            Value::Text(_) => Err(syntax("a triple term's value must be an object")),
        },
        TermType::Unknown(other) => Err(syntax(format!("an unknown term type '{other}'"))),
    }
}

/// `{"subject": …, "predicate": …, "object": …}` after its `{`.
fn read_triple(events: &mut impl Events, depth: usize) -> Result<Triple, QueryResultsParseError> {
    let (mut subject, mut predicate, mut object) = (None, None, None);
    loop {
        let key = match events.next()? {
            JsonEvent::ObjectKey(key) => key.into_owned(),
            JsonEvent::EndObject => break,
            _ => return Err(syntax("expected a member of a triple")),
        };
        if !matches!(events.next()?, JsonEvent::StartObject) {
            return Err(syntax(format!("the {key} of a triple must be a term")));
        }
        let term = read_term(events, depth)?;
        match key.as_str() {
            "subject" => subject = Some(term),
            "predicate" => predicate = Some(term),
            "object" => object = Some(term),
            other => return Err(syntax(format!("an unknown member '{other}' of a triple"))),
        }
    }
    let subject =
        NamedOrBlankNode::try_from(subject.ok_or_else(|| syntax("a triple without subject"))?)
            .map_err(|e| syntax(format!("a triple's subject: {e}")))?;
    let Some(Term::NamedNode(predicate)) = predicate else {
        return Err(syntax("a triple's predicate must be an IRI"));
    };
    let object = object.ok_or_else(|| syntax("a triple without object"))?;
    Ok(Triple::new(subject, predicate, object))
}

/// Skips the next value (whatever its nesting).
fn skip_value(events: &mut impl Events) -> Result<(), QueryResultsParseError> {
    let mut depth = 0_usize;
    loop {
        match events.next()? {
            JsonEvent::StartObject | JsonEvent::StartArray => depth += 1,
            JsonEvent::EndObject | JsonEvent::EndArray => depth -= 1,
            JsonEvent::ObjectKey(_) => continue,
            JsonEvent::Eof => return Err(syntax("the document ends inside a value")),
            _ => {}
        }
        if depth == 0 {
            return Ok(());
        }
    }
}
