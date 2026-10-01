//! The parser: one pass of recursive descent over the text, building the algebra as it
//! goes (SPARQL 1.1 §18.2 and the SPARQL 1.2 additions).
//!
//! It is scannerless: each rule looks at the next bytes itself, so a `<` can be an IRI
//! where a term may stand and an operator where an operator may, and a keyword is a
//! keyword only where one may stand (`select:x` is a prefixed name). Each rule dispatches
//! on the next byte or word; nothing is tried and undone.
//!
//! Names the translation needs (aggregate results, `GROUP BY` expressions, `DESCRIBE`
//! IRIs, anonymous blank nodes) are numbered: `__agg0`, `__b1`, … If the query itself uses
//! a name with that prefix, the text is parsed again with a longer prefix, so the same
//! text always gives the same algebra.

mod expr;
mod lexical;
mod pattern;
mod query;

use std::collections::{HashMap, HashSet};
use std::fmt;

use nrese_rdf::{BlankNode, Iri, IriParseError, NamedNode, Variable};

use crate::algebra::AggregateExpression;
use crate::query::{Query, Update};

/// Nesting allowed by default: groups, brackets, paths, collections and triple terms
/// together. Deeper input is an error, not a stack overflow.
pub const DEFAULT_MAX_NESTING: usize = 128;

/// Parses SPARQL queries and updates. The options are those of the text's surroundings:
/// a base IRI, prefixes, functions the engine treats as aggregates, and which extensions
/// of SPARQL 1.1 are accepted.
#[derive(Clone, Debug)]
pub struct SparqlParser {
    base_iri: Option<Iri<String>>,
    prefixes: HashMap<String, Namespace>,
    custom_aggregates: HashSet<NamedNode>,
    sparql_12: bool,
    lateral: bool,
    adjust: bool,
    max_nesting: usize,
}

impl Default for SparqlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SparqlParser {
    /// SPARQL 1.1 with SPARQL 1.2, `LATERAL` (SEP-0006) and `ADJUST` (SEP-0002).
    pub fn new() -> Self {
        Self {
            base_iri: None,
            prefixes: HashMap::new(),
            custom_aggregates: HashSet::new(),
            sparql_12: true,
            lateral: true,
            adjust: true,
            max_nesting: DEFAULT_MAX_NESTING,
        }
    }

    /// The IRI relative IRIs resolve against, unless the text declares its own `BASE`.
    pub fn with_base_iri(mut self, base_iri: impl Into<String>) -> Result<Self, IriParseError> {
        self.base_iri = Some(Iri::parse(base_iri.into())?);
        Ok(self)
    }

    /// A prefix the text may use without declaring it.
    pub fn with_prefix(
        mut self,
        prefix: impl Into<String>,
        iri: impl Into<String>,
    ) -> Result<Self, IriParseError> {
        let iri = Iri::parse(iri.into())?;
        self.prefixes.insert(prefix.into(), Namespace::new(iri));
        Ok(self)
    }

    /// A function called by its IRI that is an aggregate (`<f>(DISTINCT ?x)`), not a
    /// function of one row.
    pub fn with_custom_aggregate_function(mut self, name: impl Into<NamedNode>) -> Self {
        self.custom_aggregates.insert(name.into());
        self
    }

    /// Whether SPARQL 1.2 syntax is accepted (default: yes).
    pub fn with_sparql_12(mut self, enabled: bool) -> Self {
        self.sparql_12 = enabled;
        self
    }

    /// Whether `LATERAL` (SEP-0006) is accepted (default: yes).
    pub fn with_lateral(mut self, enabled: bool) -> Self {
        self.lateral = enabled;
        self
    }

    /// Whether `ADJUST` (SEP-0002) is accepted (default: yes).
    pub fn with_adjust(mut self, enabled: bool) -> Self {
        self.adjust = enabled;
        self
    }

    /// How deep groups, brackets, paths, collections and triple terms may nest.
    pub fn with_max_nesting(mut self, depth: usize) -> Self {
        self.max_nesting = depth;
        self
    }

    pub fn parse_query(&self, text: &str) -> Result<Query, SparqlSyntaxError> {
        self.parse_with_names(text, |p| p.query())
    }

    pub fn parse_update(&self, text: &str) -> Result<Update, SparqlSyntaxError> {
        self.parse_with_names(text, |p| p.update())
    }

    /// Parses with the shortest prefix for generated names that the text doesn't use.
    fn parse_with_names<T>(
        &self,
        text: &str,
        rule: impl Fn(&mut Parser<'_>) -> Result<T, SparqlSyntaxError>,
    ) -> Result<T, SparqlSyntaxError> {
        let mut prefix = String::from("__");
        loop {
            let mut parser = Parser::new(text, self, &prefix);
            let result = rule(&mut parser);
            if !parser.names_collide {
                return result;
            }
            prefix.push('_');
        }
    }
}

/// A declared prefix's IRI, and whether a local name of plain characters (letters,
/// digits, `_`, `-`, `.`) appended to it is sure to give a valid IRI: it is when the IRI
/// already has a path, query or fragment (after an authority without a path the
/// characters would land in the host or port), so such prefixed names skip the IRI check.
#[derive(Clone, Debug)]
pub(crate) struct Namespace {
    pub(crate) iri: String,
    pub(crate) extendable: bool,
}

impl Namespace {
    pub(crate) fn new(iri: Iri<String>) -> Self {
        let extendable =
            !iri.path().is_empty() || iri.query().is_some() || iri.fragment().is_some();
        Self {
            iri: iri.into_inner(),
            extendable,
        }
    }
}

/// A position in the text: line and column (from 0, in characters) and byte offset.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextPosition {
    pub line: u64,
    pub column: u64,
    pub offset: u64,
}

/// Why a text is not SPARQL, and where.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct SparqlSyntaxError {
    message: String,
    location: Option<TextPosition>,
}

impl SparqlSyntaxError {
    pub(crate) fn new(message: impl Into<String>, location: Option<TextPosition>) -> Self {
        Self {
            message: message.into(),
            location,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where the text stops being SPARQL; `None` for an error of the options.
    pub fn location(&self) -> Option<TextPosition> {
        self.location
    }
}

impl fmt::Display for SparqlSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.location {
            Some(at) => write!(
                f,
                "line {}, column {}: {}",
                at.line + 1,
                at.column + 1,
                self.message
            ),
            None => f.write_str(&self.message),
        }
    }
}

impl From<IriParseError> for SparqlSyntaxError {
    fn from(error: IriParseError) -> Self {
        Self::new(format!("an invalid base IRI: {error}"), None)
    }
}

pub(crate) type ParseResult<T> = Result<T, SparqlSyntaxError>;

/// The state of one parse.
pub(crate) struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    options: &'a SparqlParser,
    base: Option<Iri<String>>,
    prefixes: HashMap<String, Namespace>,
    /// Blank node labels of groups already closed: a label belongs to one group.
    used_blank_nodes: HashSet<String>,
    /// Blank node labels of the group being read.
    current_blank_nodes: HashSet<String>,
    /// The aggregates of each `SELECT` being read, innermost last.
    aggregates: Vec<Vec<(Variable, AggregateExpression)>>,
    /// Whether an aggregate may stand here (select expressions, `HAVING`, `ORDER BY`).
    aggregates_allowed: bool,
    name_prefix: &'a str,
    next_name: usize,
    /// The text uses a name that starts like a generated one.
    names_collide: bool,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str, options: &'a SparqlParser, name_prefix: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
            options,
            base: options.base_iri.clone(),
            prefixes: options.prefixes.clone(),
            used_blank_nodes: HashSet::new(),
            current_blank_nodes: HashSet::new(),
            aggregates: Vec::new(),
            aggregates_allowed: false,
            name_prefix,
            next_name: 0,
            names_collide: false,
            depth: 0,
        }
    }

    // --- Errors ---------------------------------------------------------------------

    fn position(&self, offset: usize) -> TextPosition {
        let offset = offset.min(self.bytes.len());
        let before = &self.text[..floor_char_boundary(self.text, offset)];
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        TextPosition {
            line: before.bytes().filter(|&b| b == b'\n').count() as u64,
            column: before[line_start..].chars().count() as u64,
            offset: offset as u64,
        }
    }

    /// An error at `offset`.
    fn error_at(&self, offset: usize, message: impl Into<String>) -> SparqlSyntaxError {
        SparqlSyntaxError::new(message, Some(self.position(offset)))
    }

    /// An error at the next token.
    fn error(&mut self, message: impl Into<String>) -> SparqlSyntaxError {
        self.ws();
        self.error_at(self.pos, message)
    }

    /// "expected X, found …" at the next token.
    fn expected(&mut self, what: &str) -> SparqlSyntaxError {
        self.ws();
        let found = self.describe_next();
        self.error_at(self.pos, format!("expected {what}, found {found}"))
    }

    fn describe_next(&self) -> String {
        let rest = &self.text[self.pos..];
        if rest.is_empty() {
            return "the end of the text".into();
        }
        let token: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace())
            .take(20)
            .collect();
        format!("'{token}'")
    }

    // --- Nesting --------------------------------------------------------------------

    fn enter(&mut self) -> ParseResult<()> {
        self.depth += 1;
        if self.depth > self.options.max_nesting {
            return Err(self.error(format!(
                "nested deeper than {} levels",
                self.options.max_nesting
            )));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // --- The cursor -----------------------------------------------------------------

    /// Skips white space and comments.
    fn ws(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                b'#' => {
                    while let Some(&b) = self.bytes.get(self.pos) {
                        if b == b'\n' || b == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// The next byte after white space.
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.bytes.get(self.pos).copied()
    }

    /// The offset of the next token.
    fn peek_offset(&mut self) -> usize {
        self.ws();
        self.pos
    }

    fn byte_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(offset).copied()
    }

    fn at_end(&mut self) -> bool {
        self.peek().is_none()
    }

    /// Whether the next bytes are `s` (after white space).
    fn looking_at(&mut self, s: &str) -> bool {
        self.ws();
        self.bytes[self.pos..].starts_with(s.as_bytes())
    }

    /// Consumes `s` if it comes next.
    fn eat(&mut self, s: &str) -> bool {
        if self.looking_at(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str) -> ParseResult<()> {
        if self.eat(s) {
            Ok(())
        } else {
            Err(self.expected(&format!("'{s}'")))
        }
    }

    /// Whether the keyword `kw` (any case) comes next, as a whole word.
    fn looking_at_keyword(&mut self, kw: &str) -> bool {
        self.ws();
        let end = self.pos + kw.len();
        self.bytes
            .get(self.pos..end)
            .is_some_and(|word| word.eq_ignore_ascii_case(kw.as_bytes()))
            && !self.byte_at(end).is_some_and(is_word_byte)
    }

    /// Consumes the keyword `kw` (any case) if it comes next.
    fn keyword(&mut self, kw: &str) -> bool {
        if self.looking_at_keyword(kw) {
            self.pos += kw.len();
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, kw: &str) -> ParseResult<()> {
        if self.keyword(kw) {
            Ok(())
        } else {
            Err(self.expected(kw))
        }
    }

    /// The word (letters, digits, `_`) at the cursor, without consuming it.
    fn peek_word(&mut self) -> &'a str {
        self.ws();
        let start = self.pos;
        let mut end = start;
        while self
            .byte_at(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            end += 1;
        }
        &self.text[start..end]
    }

    /// Whether a prefixed name starts at the cursor: name characters, then `:`.
    fn at_prefixed_name(&mut self) -> bool {
        self.ws();
        let mut i = self.pos;
        while let Some(b) = self.byte_at(i) {
            if b == b':' {
                return true;
            }
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.') || b >= 0x80 {
                i += 1;
            } else {
                return false;
            }
        }
        false
    }

    // --- Generated names ------------------------------------------------------------

    fn generated_name(&mut self, kind: &str) -> String {
        let name = format!("{}{kind}{}", self.name_prefix, self.next_name);
        self.next_name += 1;
        name
    }

    pub(crate) fn fresh_variable(&mut self, kind: &str) -> Variable {
        Variable::new_unchecked(self.generated_name(kind))
    }

    pub(crate) fn fresh_blank_node(&mut self) -> BlankNode {
        BlankNode::new_unchecked(self.generated_name("b"))
    }

    /// Notes a name the text uses, so generated ones keep clear of it.
    fn note_name(&mut self, name: &str) {
        if name.starts_with(self.name_prefix) {
            self.names_collide = true;
        }
    }
}

/// Bytes that continue a keyword (so `SELECTx` and `select:x` are no keywords).
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b':' || b >= 0x80
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}
