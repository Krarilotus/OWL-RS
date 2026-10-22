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

pub(crate) fn write_boolean(out: &mut Vec<u8>, value: bool, version: Option<&str>) {
    out.extend_from_slice(b"{\"head\":{");
    if let Some(version) = version {
        out.extend_from_slice(b"\"version\":");
        string(out, version);
    }
    out.extend_from_slice(if value {
        b"},\"boolean\":true}"
    } else {
        b"},\"boolean\":false}"
    });
}

pub(crate) fn write_head(out: &mut Vec<u8>, variables: &[Variable], version: Option<&str>) {
    out.extend_from_slice(b"{\"head\":{\"vars\":[");
    for (i, variable) in variables.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        string(out, variable.as_str());
    }
    out.push(b']');
    if let Some(version) = version {
        out.extend_from_slice(b",\"version\":");
        string(out, version);
    }
    out.extend_from_slice(b"},\"results\":{\"bindings\":[");
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

use crate::parser::scoped::{self, Held, Owner, Policy, Unchecked};
use nrese_json::SliceJsonSession;
use std::io;

pub(crate) trait Events {
    type P: Policy;
    fn with_next<R>(
        &mut self,
        f: impl for<'a> FnOnce(&mut Self::P, JsonEvent<'a>) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError>;
    fn with_owner<R>(
        &mut self,
        f: impl FnOnce(&mut Self::P) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError>;
}
impl Events for SliceJsonParser<'_> {
    type P = Unchecked;
    fn with_next<R>(
        &mut self,
        f: impl for<'a> FnOnce(&mut Self::P, JsonEvent<'a>) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError> {
        f(&mut Unchecked, self.next_event()?)
    }
    fn with_owner<R>(
        &mut self,
        f: impl FnOnce(&mut Self::P) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError> {
        f(&mut Unchecked)
    }
}
impl<Rd: Read> Events for ReaderJsonParser<Rd> {
    type P = Unchecked;
    fn with_next<R>(
        &mut self,
        f: impl for<'a> FnOnce(&mut Self::P, JsonEvent<'a>) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError> {
        f(&mut Unchecked, self.next_event()?)
    }
    fn with_owner<R>(
        &mut self,
        f: impl FnOnce(&mut Self::P) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError> {
        f(&mut Unchecked)
    }
}
pub(crate) struct ScopedEvents<'c, 'i, 's, 'o, C>(
    pub &'c mut SliceJsonSession<'i, 's, Owner<'o, C>>,
);
impl<'o, C> Events for ScopedEvents<'_, '_, '_, 'o, C> {
    type P = Owner<'o, C>;
    fn with_next<R>(
        &mut self,
        f: impl for<'a> FnOnce(&mut Self::P, JsonEvent<'a>) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError> {
        self.0
            .next_event(|p, event| {
                f(p, event)
                    .map_err(io::Error::from)
                    .map_err(nrese_json::JsonParseError::from)
            })
            .map_err(scoped::from_json)
    }
    fn with_owner<R>(
        &mut self,
        f: impl FnOnce(&mut Self::P) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError> {
        f(self.0.owner_mut())
    }
}

fn syntax(message: impl Into<String>) -> QueryResultsParseError {
    QueryResultsSyntaxError::msg(message).into()
}
pub(crate) enum Start {
    Boolean(bool),
    Solutions,
}
type Sparse<P> = Held<Vec<(Held<String, P>, Held<Term, P>)>, P>;
type Early<P> = Held<std::vec::IntoIter<Sparse<P>>, P>;
type Row<P> = Held<Vec<Option<Term>>, P>;

pub(crate) struct JsonSolutions<E: Events> {
    variables: Arc<[Variable]>,
    buffered: VecDeque<Vec<Option<Term>>>,
    early: Option<Early<E::P>>,
    streaming: bool,
    done: bool,
    events: E, // Payload fields drop before the scoped event owner.
}
const MAX_NESTING: usize = 64;

#[derive(PartialEq)]
enum Shape {
    Object,
    EndObject,
    Array,
    EndArray,
    Eof,
    Other,
}
fn shape<E: Events>(events: &mut E) -> Result<Shape, QueryResultsParseError> {
    events.with_next(|_, event| {
        Ok(match event {
            JsonEvent::StartObject => Shape::Object,
            JsonEvent::EndObject => Shape::EndObject,
            JsonEvent::StartArray => Shape::Array,
            JsonEvent::EndArray => Shape::EndArray,
            JsonEvent::Eof => Shape::Eof,
            _ => Shape::Other,
        })
    })
}
enum Key {
    Head,
    Boolean,
    Results,
    Bindings,
    Vars,
    Other,
    End,
}
fn key<E: Events>(events: &mut E, error: &'static str) -> Result<Key, QueryResultsParseError> {
    events.with_next(|_, event| match event {
        JsonEvent::EndObject => Ok(Key::End),
        JsonEvent::ObjectKey(name) => Ok(match name.as_ref() {
            "head" => Key::Head,
            "boolean" => Key::Boolean,
            "results" => Key::Results,
            "bindings" => Key::Bindings,
            "vars" => Key::Vars,
            _ => Key::Other,
        }),
        _ => Err(syntax(error)),
    })
}

impl<E: Events> JsonSolutions<E> {
    pub(crate) fn start(mut events: E) -> Result<(Start, Self), QueryResultsParseError> {
        if shape(&mut events)? != Shape::Object {
            return Err(syntax("SPARQL JSON results must be an object"));
        }
        let mut variables: Option<Vec<Variable>> = None;
        let mut boolean = None;
        let mut early: Vec<Sparse<E::P>> = Vec::new();
        let mut saw_results = false;
        loop {
            match key(&mut events, "expected a member of the results object")? {
                Key::End => break,
                Key::Head => variables = Some(read_head(&mut events)?),
                Key::Boolean => {
                    boolean = Some(events.with_next(|_, event| match event {
                        JsonEvent::Boolean(value) => Ok(value),
                        _ => Err(syntax("'boolean' must be true or false")),
                    })?)
                }
                Key::Results => {
                    saw_results = true;
                    if shape(&mut events)? != Shape::Object {
                        return Err(syntax("'results' must be an object"));
                    }
                    loop {
                        match key(&mut events, "expected a member of 'results'")? {
                            Key::End => break,
                            Key::Bindings => {}
                            _ => {
                                skip_value(&mut events)?;
                                continue;
                            }
                        }
                        if shape(&mut events)? != Shape::Array {
                            return Err(syntax("'bindings' must be an array"));
                        }
                        if let Some(variables) = variables.take() {
                            return Ok((
                                Start::Solutions,
                                Self {
                                    variables: variables.into(),
                                    buffered: VecDeque::new(),
                                    early: None,
                                    streaming: true,
                                    done: false,
                                    events,
                                },
                            ));
                        }
                        loop {
                            match shape(&mut events)? {
                                Shape::Object => {
                                    events.with_owner(|p| p.row_start())?;
                                    let binding = read_binding(&mut events)?;
                                    events.with_owner(|p| scoped::grow(p, &mut early, 1))?;
                                    early.push(binding);
                                }
                                Shape::EndArray => break,
                                _ => return Err(syntax("a binding must be an object")),
                            }
                        }
                    }
                }
                _ => skip_value(&mut events)?,
            }
        }
        if shape(&mut events)? != Shape::Eof {
            return Err(syntax("text after the results object"));
        }
        let (start, variables): (Start, Arc<[Variable]>) = match (boolean, variables) {
            (Some(value), _) if !saw_results => (Start::Boolean(value), Arc::from(Vec::new())),
            (_, Some(variables)) => (Start::Solutions, variables.into()),
            (None, None) => return Err(syntax("neither 'boolean' nor 'head' and 'results'")),
            (Some(_), None) => return Err(syntax("'results' without 'head'")),
        };
        let mut buffered = VecDeque::new();
        let early = if E::P::CHECKED {
            let bytes = early
                .capacity()
                .checked_mul(size_of::<Sparse<E::P>>())
                .ok_or_else(scoped::overflow)?;
            Some(Held::new(early.into_iter(), bytes))
        } else {
            buffered = VecDeque::with_capacity(early.len());
            // Ordinary start keeps its established eager error timing.
            for binding in early {
                buffered.push_back(place(&mut events, &variables, binding)?.value);
            }
            None
        };
        Ok((
            start,
            Self {
                variables,
                buffered,
                early,
                streaming: false,
                done: true,
                events,
            },
        ))
    }
    pub(crate) fn variables(&self) -> &Arc<[Variable]> {
        &self.variables
    }
    pub(crate) fn with_owner<R>(
        &mut self,
        f: impl FnOnce(&mut E::P) -> Result<R, QueryResultsParseError>,
    ) -> Result<R, QueryResultsParseError> {
        self.events.with_owner(f)
    }
    pub(crate) fn next_row(&mut self) -> Result<Option<Vec<Option<Term>>>, QueryResultsParseError> {
        self.next_held().map(|row| row.map(|row| row.value))
    }
    pub(crate) fn next_held(&mut self) -> Result<Option<Row<E::P>>, QueryResultsParseError> {
        if let Some(row) = self.buffered.pop_front() {
            return Ok(Some(Held::new(row, 0)));
        }
        if let Some(early) = &mut self.early {
            if let Some(binding) = early.value.next() {
                return place(&mut self.events, &self.variables, binding).map(Some);
            }
            let early = self.early.take().expect("present");
            self.events.with_owner(|p| {
                scoped::release(p, early);
                Ok(())
            })?;
        }
        if self.done || !self.streaming {
            return Ok(None);
        }
        match shape(&mut self.events)? {
            Shape::Object => {
                self.events.with_owner(|p| p.row_start())?;
                read_row(&mut self.events, &self.variables).map(Some)
            }
            Shape::EndArray => {
                self.done = true;
                let mut depth = 2;
                loop {
                    match shape(&mut self.events)? {
                        Shape::Object | Shape::Array => depth += 1,
                        Shape::EndObject | Shape::EndArray => depth -= 1,
                        Shape::Eof if depth == 0 => return Ok(None),
                        Shape::Eof => return Err(syntax("the document ends inside 'results'")),
                        _ => {}
                    }
                }
            }
            _ => Err(syntax("a binding must be an object")),
        }
    }
}

fn read_head<E: Events>(events: &mut E) -> Result<Vec<Variable>, QueryResultsParseError> {
    if shape(events)? != Shape::Object {
        return Err(syntax("'head' must be an object"));
    }
    let mut variables = Vec::new(); // Metadata exemption is deliberately unchanged.
    loop {
        match key(events, "expected a member of 'head'")? {
            Key::End => return Ok(variables),
            Key::Vars => {}
            _ => {
                skip_value(events)?;
                continue;
            }
        }
        if shape(events)? != Shape::Array {
            return Err(syntax("'vars' must be an array"));
        }
        loop {
            let variable = events.with_next(|_, event| match event {
                JsonEvent::String(name) => Variable::new(name.as_ref())
                    .map(Some)
                    .map_err(|e| syntax(format!("an invalid variable: {e}"))),
                JsonEvent::EndArray => Ok(None),
                _ => Err(syntax("a variable must be a string")),
            })?;
            let Some(variable) = variable else {
                break;
            };
            if variables.contains(&variable) {
                return Err(syntax(format!("the variable {variable} twice in 'vars'")));
            }
            variables.push(variable);
        }
    }
}

fn read_binding<E: Events>(events: &mut E) -> Result<Sparse<E::P>, QueryResultsParseError> {
    let mut binding = Vec::new();
    let mut payload = 0usize;
    loop {
        let name = events.with_next(|p, event| match event {
            JsonEvent::ObjectKey(name) => scoped::text(p, name).map(Some),
            JsonEvent::EndObject => Ok(None),
            _ => Err(syntax("expected a variable of the binding")),
        })?;
        let Some(name) = name else {
            let slots = binding
                .capacity()
                .checked_mul(size_of::<(Held<String, E::P>, Held<Term, E::P>)>())
                .ok_or_else(scoped::overflow)?;
            return Ok(Held::new(
                binding,
                slots.checked_add(payload).ok_or_else(scoped::overflow)?,
            ));
        };
        if shape(events)? != Shape::Object {
            return Err(syntax(format!(
                "the value of {} must be an object",
                name.value
            )));
        }
        let term = read_term(events, 0)?;
        payload = payload
            .checked_add(name.bytes())
            .and_then(|n| n.checked_add(term.bytes()))
            .ok_or_else(scoped::overflow)?;
        events.with_owner(|p| scoped::grow(p, &mut binding, 1))?;
        binding.push((name, term));
    }
}

fn read_row<E: Events>(
    events: &mut E,
    variables: &[Variable],
) -> Result<Held<Vec<Option<Term>>, E::P>, QueryResultsParseError> {
    let mut row = events.with_owner(|p| scoped::slots(p, variables.len()))?;
    let mut next = 0;
    loop {
        let index = events.with_next(|p, event| match event {
            JsonEvent::ObjectKey(name) => {
                if variables.get(next).is_some_and(|v| v.as_str() == name) {
                    return Ok(Some(next));
                }
                for (i, variable) in variables.iter().enumerate() {
                    p.work(1)?;
                    if variable.as_str() == name {
                        return Ok(Some(i));
                    }
                }
                Err(syntax(format!("the variable {name} isn't in 'head'")))
            }
            JsonEvent::EndObject => Ok(None),
            _ => Err(syntax("expected a variable of the binding")),
        })?;
        let Some(i) = index else {
            return Ok(row);
        };
        next = i + 1;
        if shape(events)? != Shape::Object {
            return Err(syntax(format!(
                "the value of {} must be an object",
                variables[i]
            )));
        }
        let term = read_term(events, 0)?;
        row.add(term.bytes());
        if row.value[i].replace(term.value).is_some() {
            return Err(syntax(format!(
                "the variable {} twice in a binding",
                variables[i]
            )));
        }
    }
}

fn place<E: Events>(
    events: &mut E,
    variables: &[Variable],
    binding: Sparse<E::P>,
) -> Result<Held<Vec<Option<Term>>, E::P>, QueryResultsParseError> {
    let mut row = events.with_owner(|p| scoped::slots(p, variables.len()))?;
    let slots = binding
        .value
        .capacity()
        .checked_mul(size_of::<(Held<String, E::P>, Held<Term, E::P>)>())
        .ok_or_else(scoped::overflow)?;
    for (name, term) in binding.value {
        let mut index = None;
        for (i, variable) in variables.iter().enumerate() {
            events.with_owner(|p| p.work(1))?;
            if variable.as_str() == name.value {
                index = Some(i);
                break;
            }
        }
        let i =
            index.ok_or_else(|| syntax(format!("the variable {} isn't in 'head'", name.value)))?;
        let message_name = &variables[i];
        row.add(term.bytes());
        if row.value[i].replace(term.value).is_some() {
            return Err(syntax(format!(
                "the variable {message_name} twice in a binding"
            )));
        }
        events.with_owner(|p| {
            scoped::release(p, name);
            Ok(())
        })?;
    }
    events.with_owner(|p| {
        p.free(slots);
        Ok(())
    })?;
    Ok(row)
}

enum TermType<P: Policy> {
    Uri,
    Bnode,
    Literal,
    Triple,
    Unknown(Held<String, P>),
}
enum Value {
    Text(String),
    Triple(Box<Triple>),
}
enum Member {
    Type,
    Value,
    Datatype,
    Language,
    Direction,
    Other,
    End,
}
fn discard<E: Events, T>(
    events: &mut E,
    value: Option<Held<T, E::P>>,
) -> Result<(), QueryResultsParseError> {
    if let Some(value) = value {
        events.with_owner(|p| {
            scoped::release(p, value);
            Ok(())
        })?;
    }
    Ok(())
}
fn replace<E: Events, T>(
    events: &mut E,
    slot: &mut Option<Held<T, E::P>>,
    value: Held<T, E::P>,
) -> Result<(), QueryResultsParseError> {
    discard(events, slot.replace(value))
}
fn read_string<E: Events>(events: &mut E) -> Result<Held<String, E::P>, QueryResultsParseError> {
    events.with_next(|p, event| match event {
        JsonEvent::String(text) => scoped::text(p, text),
        _ => Err(syntax("the members of a term must be strings")),
    })
}
fn as_text<P: Policy>(value: Held<Value, P>) -> Result<Held<String, P>, QueryResultsParseError> {
    let (value, bytes) = value.parts();
    match value {
        Value::Text(text) => Ok(Held::new(text, bytes)),
        Value::Triple(_) => Err(syntax(
            "an IRI, blank node or literal with a triple as value",
        )),
    }
}
fn read_term<E: Events>(
    events: &mut E,
    depth: usize,
) -> Result<Held<Term, E::P>, QueryResultsParseError> {
    let (mut kind, mut value, mut datatype, mut language, mut direction) =
        (None, None, None, None, None);
    loop {
        let member = events.with_next(|_, event| match event {
            JsonEvent::ObjectKey(key) => Ok(match key.as_ref() {
                "type" => Member::Type,
                "value" => Member::Value,
                "datatype" => Member::Datatype,
                "xml:lang" => Member::Language,
                "its:dir" => Member::Direction,
                _ => Member::Other,
            }),
            JsonEvent::EndObject => Ok(Member::End),
            _ => Err(syntax("expected a member of a term")),
        })?;
        match member {
            Member::End => break,
            Member::Value => {
                let text = events.with_next(|p, event| match event {
                    JsonEvent::String(text) => scoped::text(p, text).map(Some),
                    JsonEvent::StartObject => Ok(None),
                    _ => Err(syntax("a term's value must be a string or a triple")),
                })?;
                let next = if let Some(text) = text {
                    text.map(Value::Text)
                } else {
                    if depth >= MAX_NESTING {
                        return Err(syntax("triple terms nested too deeply"));
                    }
                    let triple = read_triple(events, depth + 1)?;
                    events.with_owner(|p| p.reserve(size_of::<Triple>()))?;
                    let (triple, bytes) = triple.parts();
                    Held::new(
                        Value::Triple(Box::new(triple)),
                        bytes
                            .checked_add(size_of::<Triple>())
                            .ok_or_else(scoped::overflow)?,
                    )
                };
                replace(events, &mut value, next)?;
            }
            Member::Type => {
                let next = events.with_next(|p, event| {
                    let JsonEvent::String(text) = event else {
                        return Err(syntax("the members of a term must be strings"));
                    };
                    Ok(match text.as_ref() {
                        "uri" => TermType::Uri,
                        "bnode" => TermType::Bnode,
                        "literal" | "typed-literal" => TermType::Literal,
                        "triple" => TermType::Triple,
                        _ => TermType::Unknown(scoped::text(p, text)?),
                    })
                })?;
                if let Some(TermType::Unknown(old)) = kind.replace(next) {
                    discard(events, Some(old))?;
                }
            }
            Member::Datatype => {
                let text = read_string(events)?;
                replace(events, &mut datatype, text)?;
            }
            Member::Language => {
                let text = read_string(events)?;
                replace(events, &mut language, text)?;
            }
            Member::Direction => {
                let text = read_string(events)?;
                replace(events, &mut direction, text)?;
            }
            Member::Other => events.with_next(|_, event| match event {
                JsonEvent::String(_) => Ok(()),
                _ => Err(syntax("the members of a term must be strings")),
            })?,
        }
    }
    let kind = kind.ok_or_else(|| syntax("a term without 'type'"))?;
    let value = value.ok_or_else(|| syntax("a term without 'value'"))?;
    match kind {
        TermType::Uri | TermType::Bnode => {
            discard(events, datatype)?;
            discard(events, language)?;
            discard(events, direction)?;
            let (text, bytes) = as_text(value)?.parts();
            let term = match kind {
                TermType::Uri => NamedNode::new(text)
                    .map(Term::from)
                    .map_err(|e| syntax(format!("an invalid IRI: {e}")))?,
                _ => BlankNode::new(text)
                    .map(Term::from)
                    .map_err(|e| syntax(format!("an invalid blank node: {e}")))?,
            };
            Ok(Held::new(term, bytes))
        }
        TermType::Literal => {
            let (text, text_bytes) = as_text(value)?.parts();
            if let Some(language) = language {
                discard(events, datatype)?;
                let (language, language_bytes) = language.parts();
                let literal = if let Some(direction) = direction {
                    let parsed = direction
                        .value
                        .parse()
                        .map_err(|e: nrese_rdf::TermParseError| syntax(e.to_string()))?;
                    discard(events, Some(direction))?;
                    Literal::new_directional_language_tagged_literal(text, language, parsed)
                } else {
                    Literal::new_language_tagged_literal(text, language)
                }
                .map_err(|e| syntax(e.to_string()))?;
                Ok(Held::new(
                    literal.into(),
                    text_bytes
                        .checked_add(language_bytes)
                        .ok_or_else(scoped::overflow)?,
                ))
            } else {
                discard(events, direction)?;
                if let Some(datatype) = datatype {
                    let (name, datatype_bytes) = datatype.parts();
                    let datatype = NamedNode::new(name)
                        .map_err(|e| syntax(format!("an invalid datatype: {e}")))?;
                    if datatype == rdf::LANG_STRING || datatype == rdf::DIR_LANG_STRING {
                        return Err(syntax("a language-string datatype without 'xml:lang'"));
                    }
                    let simple = datatype == nrese_rdf::vocab::xsd::STRING;
                    let term = Literal::new_typed_literal(text, datatype).into();
                    if simple {
                        events.with_owner(|p| {
                            p.free(datatype_bytes);
                            Ok(())
                        })?;
                    }
                    Ok(Held::new(
                        term,
                        text_bytes
                            .checked_add(if simple { 0 } else { datatype_bytes })
                            .ok_or_else(scoped::overflow)?,
                    ))
                } else {
                    Ok(Held::new(
                        Literal::new_simple_literal(text).into(),
                        text_bytes,
                    ))
                }
            }
        }
        TermType::Triple => {
            discard(events, datatype)?;
            discard(events, language)?;
            discard(events, direction)?;
            let (value, bytes) = value.parts();
            match value {
                Value::Triple(triple) => Ok(Held::new(Term::Triple(triple), bytes)),
                Value::Text(_) => Err(syntax("a triple term's value must be an object")),
            }
        }
        TermType::Unknown(other) => Err(syntax(format!("an unknown term type '{}'", other.value))),
    }
}

fn read_triple<E: Events>(
    events: &mut E,
    depth: usize,
) -> Result<Held<Triple, E::P>, QueryResultsParseError> {
    let (mut subject, mut predicate, mut object) = (None, None, None);
    loop {
        let key = events.with_next(|p, event| match event {
            JsonEvent::ObjectKey(key) => scoped::text(p, key).map(Some),
            JsonEvent::EndObject => Ok(None),
            _ => Err(syntax("expected a member of a triple")),
        })?;
        let Some(key) = key else {
            break;
        };
        if shape(events)? != Shape::Object {
            return Err(syntax(format!(
                "the {} of a triple must be a term",
                key.value
            )));
        }
        let term = read_term(events, depth)?;
        match key.value.as_str() {
            "subject" => replace(events, &mut subject, term)?,
            "predicate" => replace(events, &mut predicate, term)?,
            "object" => replace(events, &mut object, term)?,
            other => return Err(syntax(format!("an unknown member '{other}' of a triple"))),
        }
        discard(events, Some(key))?;
    }
    let (subject, sb) = subject
        .ok_or_else(|| syntax("a triple without subject"))?
        .parts();
    let subject = NamedOrBlankNode::try_from(subject)
        .map_err(|e| syntax(format!("a triple's subject: {e}")))?;
    let (predicate, pb) = predicate
        .ok_or_else(|| syntax("a triple's predicate must be an IRI"))?
        .parts();
    let Term::NamedNode(predicate) = predicate else {
        return Err(syntax("a triple's predicate must be an IRI"));
    };
    let (object, ob) = object
        .ok_or_else(|| syntax("a triple without object"))?
        .parts();
    let bytes = sb
        .checked_add(pb)
        .and_then(|n| n.checked_add(ob))
        .ok_or_else(scoped::overflow)?;
    Ok(Held::new(Triple::new(subject, predicate, object), bytes))
}

fn skip_value<E: Events>(events: &mut E) -> Result<(), QueryResultsParseError> {
    let mut depth = 0usize;
    loop {
        // ObjectKey is distinct from scalar for the existing skip rule.
        let item = events.with_next(|_, event| {
            Ok(match event {
                JsonEvent::StartObject | JsonEvent::StartArray => 1,
                JsonEvent::EndObject | JsonEvent::EndArray => 2,
                JsonEvent::ObjectKey(_) => 3,
                JsonEvent::Eof => 4,
                _ => 0,
            })
        })?;
        match item {
            1 => depth += 1,
            2 => depth -= 1,
            3 => continue,
            4 => return Err(syntax("the document ends inside a value")),
            _ => {}
        }
        if depth == 0 {
            return Ok(());
        }
    }
}
