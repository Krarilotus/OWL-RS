//! The Turtle and TriG parser: a statement at a time, by recursive descent, into an arena.
//! It also reads Notation3 ([`n3`]), whose grammar extends Turtle's.
//!
//! - **Statements.** [`TurtleParser::next_ref`] parses one statement (a directive, or the
//!   triples of one subject; in TriG, also the opening and closing of a graph block), then
//!   hands out its triples one by one. Their terms are ranges of one text arena, cleared
//!   for the next statement, so nothing is allocated per term once it has grown.
//! - **Input** refills only between tokens (see [`super::lexer`]); a token's text is copied
//!   or decoded into the arena before the buffer moves on.
//! - **Nesting** (`[ … ]`, `( … )`) is recursive, and limited (`max_depth`): hostile input
//!   gets an error, not a stack overflow.
//! - **Generated blank nodes** (`[]`, lists) are named from a key random per document and a
//!   counter, so they can't meet a label the document writes.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Read};

use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{
    BlankNodeRef, GraphNameRef, Iri, LiteralRef, NamedNodeRef, NamedOrBlankNodeRef, QuadRef,
    TermRef,
};

use super::lexer::{Kind, Lexed, Token, lex};
use crate::blank::BlankNodes;
use crate::error::{RdfParseError, RdfSyntaxError, TextPosition};
use crate::text::{NOT_IN_IRI, decode_iri, decode_string};

mod n3;

/// The settings a Turtle or TriG parser takes from [`crate::RdfParser`].
#[derive(Debug, Clone)]
pub(crate) struct TurtleSettings {
    pub(crate) trig: bool,
    pub(crate) base: Option<Iri<String>>,
    pub(crate) blank_nodes: BlankNodes,
    pub(crate) unchecked: bool,
    pub(crate) max_depth: usize,
    /// Notation3 rather than Turtle or TriG.
    pub(crate) n3: bool,
}

// ---------------------------------------------------------------------------------------
// Input

enum Source<'a, R> {
    Slice(&'a [u8]),
    Reader {
        reader: R,
        buffer: Vec<u8>,
        end: usize,
        eof: bool,
    },
}

/// The input, buffered: `data()[position..]` is unconsumed. Refilling keeps the unconsumed
/// bytes (moved to the front) and reads more.
struct Input<'a, R> {
    source: Source<'a, R>,
    position: usize,
    /// The document offset of `data()[0]`, and of the start of the current line; the
    /// number of line breaks consumed.
    base: u64,
    line: u64,
    line_start: u64,
}

impl<R: Read> Input<'_, R> {
    fn data(&self) -> &[u8] {
        match &self.source {
            Source::Slice(bytes) => bytes,
            Source::Reader { buffer, end, .. } => &buffer[..*end],
        }
    }

    fn eof(&self) -> bool {
        match &self.source {
            Source::Slice(_) => true,
            Source::Reader { eof, .. } => *eof,
        }
    }

    fn refill(&mut self) -> io::Result<()> {
        let Source::Reader {
            reader,
            buffer,
            end,
            eof,
        } = &mut self.source
        else {
            return Ok(());
        };
        if self.position > 0 {
            buffer.copy_within(self.position..*end, 0);
            *end -= self.position;
            self.base += self.position as u64;
            self.position = 0;
        }
        if *end == buffer.len() {
            buffer.resize(buffer.len() * 2, 0);
        }
        loop {
            match reader.read(&mut buffer[*end..]) {
                Ok(0) => *eof = true,
                Ok(n) => *end += n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            return Ok(());
        }
    }

    /// Consumes `n` bytes, counting the line breaks in them.
    fn consume(&mut self, n: usize) {
        let consumed = &self.data()[self.position..self.position + n];
        let breaks = memchr::memchr_iter(b'\n', consumed).count() as u64;
        if breaks > 0 {
            let last = memchr::memrchr(b'\n', consumed).unwrap_or(0);
            self.line += breaks;
            self.line_start = self.base + (self.position + last + 1) as u64;
        }
        self.position += n;
    }

    /// The text position of `data()[at]` (at or after the current position).
    fn position_at(&self, at: usize) -> TextPosition {
        let data = self.data();
        let at = at.min(data.len());
        let before = &data[self.position.min(at)..at];
        let breaks = memchr::memchr_iter(b'\n', before).count() as u64;
        let offset = self.base + at as u64;
        let line_start = match memchr::memrchr(b'\n', before) {
            Some(i) => self.base + (self.position + i + 1) as u64,
            None => self.line_start,
        };
        // Characters when the line is still buffered, bytes otherwise.
        let column = if line_start >= self.base {
            let from = (line_start - self.base) as usize;
            String::from_utf8_lossy(&data[from..at]).chars().count() as u64
        } else {
            offset - line_start
        };
        TextPosition {
            line: self.line + breaks,
            column,
            offset,
        }
    }
}

// ---------------------------------------------------------------------------------------
// The arena

/// A range of the arena.
#[derive(Clone, Copy)]
struct Span {
    start: u32,
    end: u32,
}

#[derive(Clone, Copy)]
enum Term {
    Iri(Span),
    Static(NamedNodeRef<'static>),
    Blank(Span),
    Literal {
        value: Span,
        kind: Lit,
    },
    /// An N3 quick variable (`?name`): its name.
    Variable(Span),
}

#[derive(Clone, Copy)]
enum Lit {
    Simple,
    Language(Span),
    Typed(Span),
    TypedStatic(NamedNodeRef<'static>),
}

#[derive(Clone, Copy)]
struct Triple {
    subject: Term,
    predicate: Term,
    object: Term,
    /// The N3 formula the triple is in (its blank node); `None` outside formulas.
    formula: Option<Term>,
}

/// The graph a TriG block's statements go to.
enum GraphLabel {
    Iri(String),
    Blank(String),
}

// ---------------------------------------------------------------------------------------
// The parser

pub(crate) struct TurtleParser<'a, R> {
    input: Input<'a, R>,
    settings: TurtleSettings,
    prefixes: HashMap<String, String>,
    base: Option<Iri<String>>,
    /// The key and counter of generated blank nodes.
    generated_key: u64,
    generated: u64,
    lookahead: Option<Token>,
    after_string: bool,
    /// The current statement.
    arena: String,
    triples: Vec<Triple>,
    handed_out: usize,
    /// TriG: inside `{ … }`, and the graph of that block.
    in_block: bool,
    graph: Option<GraphLabel>,
    done: bool,
    /// Reused buffers: an IRI being decoded, and resolved.
    scratch: String,
    resolved: String,
    /// Whether a byte order mark at the start has been looked for.
    started: bool,
    /// N3: the formulas being read, innermost last.
    formulas: Vec<Term>,
}

const INITIAL_BUFFER: usize = 64 * 1024;

impl<'a> TurtleParser<'a, io::Empty> {
    pub(crate) fn from_slice(bytes: &'a [u8], settings: TurtleSettings) -> Self {
        Self::new(Source::Slice(bytes), settings)
    }
}

impl<R: Read> TurtleParser<'static, R> {
    pub(crate) fn from_reader(reader: R, settings: TurtleSettings) -> Self {
        Self::new(
            Source::Reader {
                reader,
                buffer: vec![0; INITIAL_BUFFER],
                end: 0,
                eof: false,
            },
            settings,
        )
    }
}

/// The result of a parsing step.
type Step<T> = Result<T, RdfParseError>;

impl<'a, R: Read> TurtleParser<'a, R> {
    /// This parser for a chunk of a larger document ([`super::split`]): where the chunk
    /// starts there (so errors give the document's lines), and the directives in force at
    /// its start, read now.
    pub(crate) fn for_chunk(
        mut self,
        offset: u64,
        line: u64,
        line_start: u64,
        directives: &str,
    ) -> Result<Self, RdfParseError> {
        self.input.base = offset;
        self.input.line = line;
        self.input.line_start = line_start;
        // A byte order mark can only be at the document's start.
        self.started = offset > 0;
        if !directives.is_empty() {
            let mut header = TurtleParser::from_slice(directives.as_bytes(), self.settings.clone());
            while let Some(step) = header.next_ref() {
                step?;
            }
            self.prefixes = header.prefixes;
            self.base = header.base;
        }
        Ok(self)
    }

    fn new(source: Source<'a, R>, settings: TurtleSettings) -> Self {
        let input = Input {
            source,
            position: 0,
            base: 0,
            line: 0,
            line_start: 0,
        };
        Self {
            input,
            base: settings.base.clone(),
            settings,
            prefixes: HashMap::new(),
            generated_key: rand_key(),
            generated: 0,
            lookahead: None,
            after_string: false,
            arena: String::new(),
            triples: Vec::new(),
            handed_out: 0,
            in_block: false,
            graph: None,
            done: false,
            scratch: String::new(),
            resolved: String::new(),
            started: false,
            formulas: Vec::new(),
        }
    }

    /// The next quad, borrowed until the next call; `None` at the end.
    pub(crate) fn next_ref(&mut self) -> Option<Step<QuadRef<'_>>> {
        loop {
            if self.handed_out < self.triples.len() {
                let triple = self.triples[self.handed_out];
                self.handed_out += 1;
                return Some(Ok(self.quad(triple)));
            }
            if self.done {
                return None;
            }
            self.arena.clear();
            self.triples.clear();
            self.handed_out = 0;
            if let Err(error) = self.statement() {
                self.done = true;
                return Some(Err(error));
            }
        }
    }

    fn quad(&self, triple: Triple) -> QuadRef<'_> {
        let text = |s: Span| &self.arena[s.start as usize..s.end as usize];
        let term = |t: Term| -> TermRef<'_> {
            match t {
                Term::Iri(s) => NamedNodeRef::new_unchecked(text(s)).into(),
                Term::Static(n) => n.into(),
                Term::Blank(s) => BlankNodeRef::new_unchecked(text(s)).into(),
                Term::Variable(_) => unreachable!("variables only come out of an N3 parser"),
                Term::Literal { value, kind } => match kind {
                    Lit::Simple => LiteralRef::new_simple_literal(text(value)).into(),
                    Lit::Language(tag) => {
                        LiteralRef::new_language_tagged_literal_unchecked(text(value), text(tag))
                            .into()
                    }
                    Lit::Typed(datatype) => LiteralRef::new_typed_literal(
                        text(value),
                        NamedNodeRef::new_unchecked(text(datatype)),
                    )
                    .into(),
                    Lit::TypedStatic(datatype) => {
                        LiteralRef::new_typed_literal(text(value), datatype).into()
                    }
                },
            }
        };
        let subject = match term(triple.subject) {
            TermRef::NamedNode(n) => NamedOrBlankNodeRef::NamedNode(n),
            TermRef::BlankNode(b) => NamedOrBlankNodeRef::BlankNode(b),
            TermRef::Literal(_) => unreachable!("subjects are nodes"),
        };
        let TermRef::NamedNode(predicate) = term(triple.predicate) else {
            unreachable!("predicates are IRIs")
        };
        let graph_name = match &self.graph {
            None => GraphNameRef::DefaultGraph,
            Some(GraphLabel::Iri(iri)) => NamedNodeRef::new_unchecked(iri).into(),
            Some(GraphLabel::Blank(label)) => BlankNodeRef::new_unchecked(label).into(),
        };
        QuadRef::new(subject, predicate, term(triple.object), graph_name)
    }

    // --- Tokens ----------------------------------------------------------------------

    fn fill(&mut self) -> Step<()> {
        if self.lookahead.is_some() {
            return Ok(());
        }
        if !self.started {
            // A byte order mark at the start is no text.
            self.started = true;
            while self.input.data().len() < 3 && !self.input.eof() {
                self.input.refill()?;
            }
            if self.input.data().starts_with("\u{FEFF}".as_bytes()) {
                self.input.consume(3);
            }
        }
        loop {
            let position = self.input.position;
            let lexed = lex(
                &self.input.data()[position..],
                self.input.eof(),
                self.after_string,
                self.settings.n3,
            );
            match lexed {
                Lexed::Token(mut token) => {
                    token.start += position;
                    token.text = token.text.start + position..token.text.end + position;
                    self.lookahead = Some(token);
                    return Ok(());
                }
                Lexed::NeedMore => self.input.refill()?,
                Lexed::Error(at, message) => return Err(self.error(position + at, message)),
            }
        }
    }

    /// The kind of the next token.
    fn peek(&mut self) -> Step<Kind> {
        self.fill()?;
        Ok(self.lookahead.as_ref().map_or(Kind::Eof, |t| t.kind))
    }

    /// The next token, consumed: its text stays readable until the next `fill`.
    fn take(&mut self) -> Step<Token> {
        self.fill()?;
        let token = self.lookahead.take().expect("filled");
        self.input.consume(token.consumed);
        self.after_string = matches!(token.kind, Kind::String { .. });
        Ok(token)
    }

    fn expect(&mut self, kind: Kind, what: &'static str) -> Step<Token> {
        let token = self.take()?;
        if token.kind != kind {
            return Err(self.error(token.start, what));
        }
        Ok(token)
    }

    /// The text of a token taken last.
    fn text(&self, token: &Token) -> Step<&str> {
        std::str::from_utf8(&self.input.data()[token.text.clone()])
            .map_err(|e| self.error(token.text.start + e.valid_up_to(), "invalid UTF-8"))
    }

    fn error(&self, at: usize, message: impl Into<String>) -> RdfParseError {
        let position = self.input.position_at(at);
        RdfSyntaxError::new(message, position..position).into()
    }

    // --- Terms into the arena ----------------------------------------------------------
    //
    // Text goes from the input buffer to the arena directly: these functions borrow
    // `self.input` and write `self.arena` (or a scratch string) at once, through the fields,
    // so nothing is allocated per term.

    fn span_from(&self, start: usize) -> Span {
        Span {
            start: start as u32,
            end: self.arena.len() as u32,
        }
    }

    /// The token's text, copied into the arena.
    fn push_text(&mut self, token: &Token) -> Step<Span> {
        let start = self.arena.len();
        match std::str::from_utf8(&self.input.data()[token.text.clone()]) {
            Ok(text) => self.arena.push_str(text),
            Err(e) => return Err(self.error(token.text.start + e.valid_up_to(), "invalid UTF-8")),
        }
        Ok(self.span_from(start))
    }

    /// A string token's content, its escapes decoded, into the arena.
    fn push_string(&mut self, token: &Token, escaped: bool) -> Step<Span> {
        if !escaped {
            return self.push_text(token);
        }
        let start = self.arena.len();
        let decoded = match std::str::from_utf8(&self.input.data()[token.text.clone()]) {
            Ok(text) => decode_string(text, &mut self.arena),
            Err(e) => return Err(self.error(token.text.start + e.valid_up_to(), "invalid UTF-8")),
        };
        decoded.map_err(|i| self.error(token.text.start + i, "an invalid escape"))?;
        Ok(self.span_from(start))
    }

    /// An IRI reference, decoded and resolved against the base, into `self.resolved`.
    fn resolve_iri(&mut self, token: &Token) -> Step<()> {
        let escaped = matches!(token.kind, Kind::IriRef { escaped: true });
        self.scratch.clear();
        let checked = match std::str::from_utf8(&self.input.data()[token.text.clone()]) {
            Ok(text) if escaped => decode_iri(text, &mut self.scratch),
            Ok(text) => match text.bytes().position(|b| NOT_IN_IRI[b as usize]) {
                Some(i) => Err(i),
                None => {
                    self.scratch.push_str(text);
                    Ok(())
                }
            },
            Err(e) => return Err(self.error(token.text.start + e.valid_up_to(), "invalid UTF-8")),
        };
        checked.map_err(|i| {
            self.error(
                token.text.start + i,
                "a character or escape IRIs can't hold",
            )
        })?;
        match &self.base {
            Some(base) if self.settings.unchecked => {
                self.resolved = base.resolve_unchecked(&self.scratch).into_inner();
            }
            Some(base) => {
                if let Err(e) = base.resolve_into(&self.scratch, &mut self.resolved) {
                    return Err(self.error(token.text.start, format!("an invalid IRI: {e}")));
                }
            }
            None => {
                if !self.settings.unchecked && Iri::parse(self.scratch.as_str()).is_err() {
                    return Err(
                        self.error(token.text.start, "a relative or invalid IRI without a base")
                    );
                }
                std::mem::swap(&mut self.scratch, &mut self.resolved);
            }
        }
        Ok(())
    }

    /// The IRI of `<…>` or `prefix:local`, in the arena.
    fn iri(&mut self, token: &Token) -> Step<Span> {
        let start = self.arena.len();
        match token.kind {
            Kind::IriRef { .. } => {
                self.resolve_iri(token)?;
                self.arena.push_str(&self.resolved);
            }
            Kind::PrefixedName { colon, escaped } => {
                let text = match std::str::from_utf8(&self.input.data()[token.text.clone()]) {
                    Ok(text) => text,
                    Err(e) => {
                        return Err(self.error(token.text.start + e.valid_up_to(), "invalid UTF-8"));
                    }
                };
                let (prefix, local) = (&text[..colon], &text[colon + 1..]);
                let default;
                let namespace = match self.prefixes.get(prefix) {
                    Some(namespace) => namespace.as_str(),
                    // N3: an undeclared empty prefix is the document's own `<#>`.
                    None if self.settings.n3 && prefix.is_empty() => {
                        default = match &self.base {
                            Some(base) => base.resolve_unchecked("#").into_inner(),
                            None => "#".to_owned(),
                        };
                        default.as_str()
                    }
                    None => {
                        let message = format!("the prefix '{prefix}:' isn't declared");
                        return Err(self.error(token.start, message));
                    }
                };
                self.arena.push_str(namespace);
                if escaped {
                    // PN_LOCAL_ESC: the escaped character without its '\'.
                    let mut chars = local.chars();
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            self.arena.extend(chars.next());
                        } else {
                            self.arena.push(c);
                        }
                    }
                } else {
                    self.arena.push_str(local);
                }
                if !self.settings.unchecked && Iri::parse(&self.arena[start..]).is_err() {
                    return Err(self.error(token.start, "a prefixed name that isn't an IRI"));
                }
            }
            _ => return Err(self.error(token.start, "expected an IRI")),
        }
        Ok(self.span_from(start))
    }

    /// A labelled blank node, named as the settings say, in the arena.
    fn blank_label(&mut self, token: &Token) -> Step<Span> {
        let start = self.arena.len();
        match std::str::from_utf8(&self.input.data()[token.text.clone()]) {
            Ok(label) => {
                let name = self.settings.blank_nodes.name(label, &mut self.scratch);
                self.arena.push_str(name);
            }
            Err(e) => return Err(self.error(token.text.start + e.valid_up_to(), "invalid UTF-8")),
        }
        Ok(self.span_from(start))
    }

    /// A blank node no label names (`[]`, list nodes).
    fn fresh(&mut self) -> Term {
        let start = self.arena.len();
        self.generated += 1;
        let _ = write!(
            self.arena,
            "g{:016x}x{}",
            self.generated_key, self.generated
        );
        Term::Blank(self.span_from(start))
    }

    fn emit(&mut self, subject: Term, predicate: Term, object: Term) {
        self.triples.push(Triple {
            subject,
            predicate,
            object,
            formula: self.formulas.last().copied(),
        });
    }

    // --- Statements ----------------------------------------------------------------------

    fn statement(&mut self) -> Step<()> {
        if self.settings.n3 {
            return self.n3_statement();
        }
        let token = self.take()?;
        let trig = self.settings.trig;
        match token.kind {
            Kind::Eof => {
                if self.in_block {
                    return Err(self.error(token.start, "a graph block without its closing '}'"));
                }
                self.done = true;
                Ok(())
            }
            Kind::AtPrefix | Kind::Prefix | Kind::AtBase | Kind::Base if self.in_block => {
                Err(self.error(token.start, "a directive inside a graph block"))
            }
            Kind::AtPrefix => self.prefix(true),
            Kind::Prefix => self.prefix(false),
            Kind::AtBase => self.base(true),
            Kind::Base => self.base(false),
            Kind::Graph if trig && !self.in_block => {
                let label = self.take()?;
                let label = self.graph_label(&label)?;
                self.expect(Kind::OpenBrace, "expected '{' after the graph's name")?;
                self.open_block(Some(label))
            }
            Kind::OpenBrace if trig && !self.in_block => self.open_block(None),
            Kind::CloseBrace if self.in_block => {
                self.in_block = false;
                self.graph = None;
                Ok(())
            }
            // A graph's name, or a subject: decoded first (a peek may refill the input and
            // move the token's text), then told apart by what follows.
            Kind::IriRef { .. } | Kind::PrefixedName { .. } | Kind::BlankLabel
                if trig && !self.in_block =>
            {
                let term = match token.kind {
                    Kind::BlankLabel => Term::Blank(self.blank_label(&token)?),
                    _ => Term::Iri(self.iri(&token)?),
                };
                if self.peek()? == Kind::OpenBrace {
                    self.take()?;
                    let label = match term {
                        Term::Iri(span) => GraphLabel::Iri(
                            self.arena[span.start as usize..span.end as usize].to_owned(),
                        ),
                        Term::Blank(span) => GraphLabel::Blank(
                            self.arena[span.start as usize..span.end as usize].to_owned(),
                        ),
                        _ => unreachable!("a node"),
                    };
                    return self.open_block(Some(label));
                }
                self.predicate_object_list(term, 0)?;
                self.end_of_triples()
            }
            Kind::OpenBracket if trig && !self.in_block && self.peek()? == Kind::CloseBracket => {
                // `[]`: a graph's name, or the subject of triples.
                self.take()?;
                if self.peek()? == Kind::OpenBrace {
                    self.take()?;
                    let Term::Blank(span) = self.fresh() else {
                        unreachable!()
                    };
                    let label = self.arena[span.start as usize..span.end as usize].to_owned();
                    return self.open_block(Some(GraphLabel::Blank(label)));
                }
                let subject = self.fresh();
                self.predicate_object_list(subject, 0)?;
                self.end_of_triples()
            }
            _ => {
                self.triples_from(token)?;
                self.end_of_triples()
            }
        }
    }

    /// The '.' after triples; in a TriG block, optional before '}'.
    fn end_of_triples(&mut self) -> Step<()> {
        if self.in_block && self.peek()? == Kind::CloseBrace {
            return Ok(());
        }
        self.expect(Kind::Dot, "expected '.' at the end of the triples")
            .map(|_| ())
    }

    fn open_block(&mut self, label: Option<GraphLabel>) -> Step<()> {
        self.in_block = true;
        self.graph = label;
        Ok(())
    }

    fn graph_label(&mut self, token: &Token) -> Step<GraphLabel> {
        match token.kind {
            Kind::IriRef { .. } | Kind::PrefixedName { .. } => {
                let span = self.iri(token)?;
                Ok(GraphLabel::Iri(
                    self.arena[span.start as usize..span.end as usize].to_owned(),
                ))
            }
            Kind::BlankLabel => {
                let span = self.blank_label(token)?;
                Ok(GraphLabel::Blank(
                    self.arena[span.start as usize..span.end as usize].to_owned(),
                ))
            }
            Kind::OpenBracket => {
                self.expect(Kind::CloseBracket, "expected '[]' as a graph's name")?;
                let Term::Blank(span) = self.fresh() else {
                    unreachable!()
                };
                Ok(GraphLabel::Blank(
                    self.arena[span.start as usize..span.end as usize].to_owned(),
                ))
            }
            _ => Err(self.error(token.start, "expected a graph's name")),
        }
    }

    /// `@prefix p: <iri> .` or `PREFIX p: <iri>`.
    fn prefix(&mut self, dot: bool) -> Step<()> {
        let name = self.take()?;
        let Kind::PrefixedName { colon, .. } = name.kind else {
            return Err(self.error(name.start, "expected a prefix name ending with ':'"));
        };
        let text = self.text(&name)?;
        if text.len() != colon + 1 {
            return Err(self.error(name.start, "a prefix name can't have a local part"));
        }
        let prefix = text[..colon].to_owned();
        let iri = self.take()?;
        if !matches!(iri.kind, Kind::IriRef { .. }) {
            return Err(self.error(iri.start, "expected the prefix's IRI"));
        }
        self.resolve_iri(&iri)?;
        self.prefixes.insert(prefix, self.resolved.clone());
        if dot {
            self.expect(Kind::Dot, "expected '.' after @prefix")?;
        }
        Ok(())
    }

    /// `@base <iri> .` or `BASE <iri>`.
    fn base(&mut self, dot: bool) -> Step<()> {
        let iri = self.take()?;
        if !matches!(iri.kind, Kind::IriRef { .. }) {
            return Err(self.error(iri.start, "expected the base IRI"));
        }
        self.resolve_iri(&iri)?;
        self.base = Some(
            Iri::parse(self.resolved.clone())
                .map_err(|e| self.error(iri.text.start, format!("an invalid base IRI: {e}")))?,
        );
        if dot {
            self.expect(Kind::Dot, "expected '.' after @base")?;
        }
        Ok(())
    }

    /// `triples ::= subject predicateObjectList | blankNodePropertyList predicateObjectList?`
    fn triples_from(&mut self, first: Token) -> Step<()> {
        match first.kind {
            Kind::IriRef { .. } | Kind::PrefixedName { .. } => {
                let subject = Term::Iri(self.iri(&first)?);
                self.predicate_object_list(subject, 0)
            }
            Kind::BlankLabel => {
                let subject = Term::Blank(self.blank_label(&first)?);
                self.predicate_object_list(subject, 0)
            }
            Kind::OpenBracket => {
                let subject = self.fresh();
                if self.peek()? == Kind::CloseBracket {
                    // `[]` as subject: a predicate-object list must follow.
                    self.take()?;
                    return self.predicate_object_list(subject, 0);
                }
                self.predicate_object_list(subject, 1)?;
                self.expect(Kind::CloseBracket, "expected ']'")?;
                if self.peek_is_verb()? {
                    self.predicate_object_list(subject, 0)?;
                }
                Ok(())
            }
            Kind::OpenParen => {
                let subject = self.collection(1)?;
                self.predicate_object_list(subject, 0)
            }
            _ => Err(self.error(first.start, "expected a subject")),
        }
    }

    fn peek_is_verb(&mut self) -> Step<bool> {
        Ok(matches!(
            self.peek()?,
            Kind::IriRef { .. } | Kind::PrefixedName { .. } | Kind::A
        ))
    }

    /// `verb objectList (';' (verb objectList)?)*`
    fn predicate_object_list(&mut self, subject: Term, depth: usize) -> Step<()> {
        loop {
            let verb = self.take()?;
            let predicate = match verb.kind {
                Kind::A => Term::Static(rdf::TYPE),
                Kind::IriRef { .. } | Kind::PrefixedName { .. } => Term::Iri(self.iri(&verb)?),
                _ => return Err(self.error(verb.start, "expected a predicate")),
            };
            self.object(subject, predicate, depth)?;
            while self.peek()? == Kind::Comma {
                self.take()?;
                self.object(subject, predicate, depth)?;
            }
            if self.peek()? != Kind::Semicolon {
                return Ok(());
            }
            while self.peek()? == Kind::Semicolon {
                self.take()?;
            }
            if !self.peek_is_verb()? {
                return Ok(());
            }
        }
    }

    fn deeper(&self, depth: usize, at: usize) -> Step<usize> {
        if depth >= self.settings.max_depth {
            return Err(self.error(
                at,
                format!("nesting deeper than {}", self.settings.max_depth),
            ));
        }
        Ok(depth + 1)
    }

    /// One object of `subject predicate`.
    fn object(&mut self, subject: Term, predicate: Term, depth: usize) -> Step<()> {
        let token = self.take()?;
        let object = match token.kind {
            Kind::IriRef { .. } | Kind::PrefixedName { .. } => Term::Iri(self.iri(&token)?),
            Kind::BlankLabel => Term::Blank(self.blank_label(&token)?),
            Kind::String { .. }
            | Kind::Integer
            | Kind::Decimal
            | Kind::Double
            | Kind::True
            | Kind::False => self.literal(&token)?,
            Kind::OpenBracket => {
                let depth = self.deeper(depth, token.start)?;
                let node = self.fresh();
                self.emit(subject, predicate, node);
                if self.peek()? != Kind::CloseBracket {
                    self.predicate_object_list(node, depth)?;
                }
                self.expect(Kind::CloseBracket, "expected ']'")?;
                return Ok(());
            }
            Kind::OpenParen => {
                let depth = self.deeper(depth, token.start)?;
                let head = self.collection(depth)?;
                self.emit(subject, predicate, head);
                return Ok(());
            }
            _ => return Err(self.error(token.start, "expected an object")),
        };
        self.emit(subject, predicate, object);
        Ok(())
    }

    /// A literal from its first token (a string, a number, `true` or `false`); a string's
    /// language tag or datatype follows.
    fn literal(&mut self, token: &Token) -> Step<Term> {
        if let Kind::String { escaped } = token.kind {
            let value = self.push_string(token, escaped)?;
            let kind = match self.peek()? {
                Kind::LangTag => {
                    let tag = self.take()?;
                    let span = self.push_text(&tag)?;
                    self.arena[span.start as usize..span.end as usize].make_ascii_lowercase();
                    Lit::Language(span)
                }
                Kind::Datatype => {
                    self.take()?;
                    let datatype = self.take()?;
                    Lit::Typed(self.iri(&datatype)?)
                }
                _ => Lit::Simple,
            };
            return Ok(Term::Literal { value, kind });
        }
        let value = self.push_text(token)?;
        let datatype = match token.kind {
            Kind::Integer => xsd::INTEGER,
            Kind::Decimal => xsd::DECIMAL,
            Kind::Double => xsd::DOUBLE,
            _ => xsd::BOOLEAN,
        };
        Ok(Term::Literal {
            value,
            kind: Lit::TypedStatic(datatype),
        })
    }

    /// `'(' object* ')'` after the '(': its first node, or `rdf:nil`.
    fn collection(&mut self, depth: usize) -> Step<Term> {
        let mut head = None;
        let mut last: Option<Term> = None;
        while self.peek()? != Kind::CloseParen {
            let node = self.fresh();
            match last {
                None => head = Some(node),
                Some(previous) => self.emit(previous, Term::Static(rdf::REST), node),
            }
            self.object(node, Term::Static(rdf::FIRST), depth)?;
            last = Some(node);
        }
        self.take()?;
        Ok(match (head, last) {
            (Some(head), Some(last)) => {
                self.emit(last, Term::Static(rdf::REST), Term::Static(rdf::NIL));
                head
            }
            _ => Term::Static(rdf::NIL),
        })
    }
}

/// A key random per document, for generated blank nodes.
fn rand_key() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(0_u8)
}
