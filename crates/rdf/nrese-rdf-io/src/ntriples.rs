//! N-Triples and N-Quads (RDF 1.2): a parser of one line at a time, and the writer.
//!
//! A line is read in one pass over its bytes. Terms are borrowed from the line, except
//! where an escape (`\u`, `\U`, `\n`…) or a fresh blank node name needs text of its own;
//! that goes into scratch strings the parser keeps, so nothing is allocated per line once
//! they have grown. Triple terms (`<<( s p o )>>`, which nest) are the exception: they are
//! rare, and are built as owned terms.

use std::io;
use std::ops::Range;

use memchr::{memchr, memchr2};
use nrese_rdf::{
    BaseDirection, BlankNode, BlankNodeRef, GraphNameRef, Iri, LiteralRef, NamedNode, NamedNodeRef,
    NamedOrBlankNode, NamedOrBlankNodeRef, QuadRef, Term, TermRef, Triple, TripleRef,
};

use crate::blank::BlankNodes;
use crate::error::{RdfSyntaxError, TextPosition};
use crate::text::{NOT_IN_IRI, unicode_escape};

/// Text of a term: a range of the line, or one of the parser's scratch strings.
#[derive(Clone)]
enum Text {
    Line(Range<usize>),
    Scratch(usize),
}

/// The scratch strings, one per place a term can take.
const SUBJECT: usize = 0;
const PREDICATE: usize = 1;
const OBJECT: usize = 2;
const DATATYPE: usize = 3;
const LANGUAGE: usize = 4;
const GRAPH: usize = 5;
/// The subject and predicate of a triple term (made owned before the next term is read).
const TERM: usize = 6;

/// A syntax error within a line: where, and what.
type Failure = (Range<usize>, &'static str);

/// How deep triple terms may nest: deeper input is an error, not a stack overflow.
const MAX_NESTING: usize = 64;

enum Node {
    Iri(Text),
    Blank(Text),
}

enum Object {
    Node(Node),
    Literal {
        value: Text,
        kind: LiteralKind,
    },
    /// A triple term, in the parser's `triple_term`.
    Triple,
}

enum LiteralKind {
    Simple,
    Language(Text),
    DirectionalLanguage(Text, BaseDirection),
    Typed(Text),
}

/// The settings of a line parser, and its scratch space.
pub(crate) struct LineParser {
    /// N-Quads: a graph label may follow the object.
    pub(crate) quads: bool,
    /// Accept IRIs without checking they are absolute and well formed.
    pub(crate) unchecked: bool,
    pub(crate) blank_nodes: BlankNodes,
    scratch: [String; 7],
    /// The object of the last line, if it is a triple term.
    triple_term: Option<Triple>,
}

impl LineParser {
    pub(crate) fn new(quads: bool, unchecked: bool, blank_nodes: BlankNodes) -> Self {
        Self {
            quads,
            unchecked,
            blank_nodes,
            scratch: Default::default(),
            triple_term: None,
        }
    }

    /// The quad on `line` (number `line_number`, starting at byte `offset`), or `None` for a
    /// line with only whitespace or a comment.
    pub(crate) fn parse<'a>(
        &'a mut self,
        bytes: &'a [u8],
        line_number: u64,
        offset: u64,
    ) -> Result<Option<QuadRef<'a>>, RdfSyntaxError> {
        let line = match std::str::from_utf8(bytes) {
            Ok(line) => line,
            Err(error) => {
                let at = error.valid_up_to();
                return Err(syntax(
                    bytes,
                    line_number,
                    offset,
                    at..at + 1,
                    "invalid UTF-8",
                ));
            }
        };
        let fail =
            |range: Range<usize>, message: &str| syntax(bytes, line_number, offset, range, message);
        let mut i = skip_space(bytes, 0);
        if i == bytes.len() || bytes[i] == b'#' {
            return Ok(None);
        }
        if bytes[i..].starts_with(b"VERSION") {
            self.version(line, i + 7).map_err(|(r, m)| fail(r, m))?;
            return Ok(None);
        }
        let subject = match bytes[i] {
            b'<' => Node::Iri(
                self.iri(line, &mut i, SUBJECT)
                    .map_err(|(r, m)| fail(r, m))?,
            ),
            b'_' => Node::Blank(
                self.blank(line, &mut i, SUBJECT)
                    .map_err(|(r, m)| fail(r, m))?,
            ),
            _ => return Err(fail(i..i + 1, "expected an IRI or a blank node as subject")),
        };
        i = skip_space(bytes, i);
        if bytes.get(i) != Some(&b'<') {
            return Err(fail(i..i + 1, "expected an IRI as predicate"));
        }
        let predicate = self
            .iri(line, &mut i, PREDICATE)
            .map_err(|(r, m)| fail(r, m))?;
        i = skip_space(bytes, i);
        let object = self.object(line, &mut i, 0).map_err(|(r, m)| fail(r, m))?;
        i = skip_space(bytes, i);
        let graph = if self.quads && matches!(bytes.get(i), Some(b'<' | b'_')) {
            let graph = if bytes[i] == b'<' {
                Node::Iri(self.iri(line, &mut i, GRAPH).map_err(|(r, m)| fail(r, m))?)
            } else {
                Node::Blank(
                    self.blank(line, &mut i, GRAPH)
                        .map_err(|(r, m)| fail(r, m))?,
                )
            };
            i = skip_space(bytes, i);
            Some(graph)
        } else {
            None
        };
        if bytes.get(i) != Some(&b'.') {
            return Err(fail(i..i + 1, "expected '.' at the end of the statement"));
        }
        i = skip_space(bytes, i + 1);
        if i < bytes.len() && bytes[i] != b'#' {
            return Err(fail(i..bytes.len(), "unexpected text after the statement"));
        }

        // All text is in place: borrow it.
        let this: &'a Self = self;
        let node = |n: &Node| this.node_ref(line, n);
        let object: TermRef<'a> = match &object {
            Object::Triple => match &this.triple_term {
                Some(triple) => triple.into(),
                None => unreachable!("a triple term object is kept in `triple_term`"),
            },
            object => this.object_ref(line, object),
        };
        let graph_name = match &graph {
            None => GraphNameRef::DefaultGraph,
            Some(n) => node(n).into(),
        };
        let predicate = NamedNodeRef::new_unchecked(this.text(line, &predicate));
        Ok(Some(QuadRef::new(
            node(&subject),
            predicate,
            object,
            graph_name,
        )))
    }

    /// The text of `t`.
    fn text<'a>(&'a self, line: &'a str, t: &Text) -> &'a str {
        match t {
            Text::Line(range) => &line[range.clone()],
            Text::Scratch(k) => self.scratch[*k].as_str(),
        }
    }

    fn node_ref<'a>(&'a self, line: &'a str, n: &Node) -> NamedOrBlankNodeRef<'a> {
        match n {
            Node::Iri(t) => NamedNodeRef::new_unchecked(self.text(line, t)).into(),
            Node::Blank(t) => BlankNodeRef::new_unchecked(self.text(line, t)).into(),
        }
    }

    /// An object other than a triple term, borrowed.
    fn object_ref<'a>(&'a self, line: &'a str, object: &Object) -> TermRef<'a> {
        let text = |t: &Text| self.text(line, t);
        match object {
            Object::Node(n) => self.node_ref(line, n).into(),
            Object::Literal { value, kind } => match kind {
                LiteralKind::Simple => LiteralRef::new_simple_literal(text(value)).into(),
                LiteralKind::Language(tag) => {
                    LiteralRef::new_language_tagged_literal_unchecked(text(value), text(tag)).into()
                }
                LiteralKind::DirectionalLanguage(tag, direction) => {
                    LiteralRef::new_directional_language_tagged_literal_unchecked(
                        text(value),
                        text(tag),
                        *direction,
                    )
                    .into()
                }
                LiteralKind::Typed(datatype) => LiteralRef::new_typed_literal(
                    text(value),
                    NamedNodeRef::new_unchecked(text(datatype)),
                )
                .into(),
            },
            Object::Triple => unreachable!("triple terms are owned"),
        }
    }

    /// `versionDirective` after its keyword (ending at `at`): `'VERSION' STRING_LITERAL_QUOTE`
    /// and the end of the line. Every version label is accepted.
    fn version(&mut self, line: &str, at: usize) -> Result<(), Failure> {
        let bytes = line.as_bytes();
        let mut i = skip_space(bytes, at);
        if i == at || bytes.get(i) != Some(&b'"') {
            return Err((at..i + 1, "expected a version label string after VERSION"));
        }
        self.string(line, &mut i)?;
        i = skip_space(bytes, i);
        if i < bytes.len() && bytes[i] != b'#' {
            return Err((
                i..bytes.len(),
                "unexpected text after the version directive",
            ));
        }
        Ok(())
    }

    /// `object` at `*i`: an IRI, a blank node, a literal or a triple term (`depth` triple
    /// terms deep); a triple term goes to `triple_term`.
    fn object(&mut self, line: &str, i: &mut usize, depth: usize) -> Result<Object, Failure> {
        let bytes = line.as_bytes();
        Ok(match bytes.get(*i) {
            Some(b'<') if bytes[*i..].starts_with(b"<<(") => {
                let triple = self.triple_term(line, i, depth + 1)?;
                self.triple_term = Some(triple);
                Object::Triple
            }
            Some(b'<') => Object::Node(Node::Iri(self.iri(line, i, OBJECT)?)),
            Some(b'_') => Object::Node(Node::Blank(self.blank(line, i, OBJECT)?)),
            Some(b'"') => {
                let value = self.string(line, i)?;
                // `literal` is a production, not a terminal: whitespace may come before
                // the language tag or `^^`, and after `^^`.
                let after = skip_space(bytes, *i);
                if matches!(bytes.get(after), Some(b'@' | b'^')) {
                    *i = after;
                }
                let kind = match bytes.get(*i) {
                    Some(b'@') => match self.language(line, i)? {
                        (tag, None) => LiteralKind::Language(tag),
                        (tag, Some(direction)) => LiteralKind::DirectionalLanguage(tag, direction),
                    },
                    Some(b'^') if bytes.get(*i + 1) == Some(&b'^') => {
                        *i = skip_space(bytes, *i + 2);
                        if bytes.get(*i) != Some(&b'<') {
                            return Err((*i..*i + 1, "expected the datatype IRI after ^^"));
                        }
                        let start = *i;
                        let datatype = self.iri(line, i, DATATYPE)?;
                        if is_language_string(self.text(line, &datatype)) {
                            return Err((
                                start..*i,
                                "a language-string datatype without a language tag",
                            ));
                        }
                        LiteralKind::Typed(datatype)
                    }
                    _ => LiteralKind::Simple,
                };
                Object::Literal { value, kind }
            }
            _ => {
                return Err((
                    *i..*i + 1,
                    "expected an IRI, a blank node, a literal or a triple term as object",
                ));
            }
        })
    }

    /// `tripleTerm` at `*i` (on '<<('), owned.
    fn triple_term(&mut self, line: &str, i: &mut usize, depth: usize) -> Result<Triple, Failure> {
        let bytes = line.as_bytes();
        if depth > MAX_NESTING {
            return Err((*i..*i + 3, "triple terms nested too deeply"));
        }
        *i = skip_space(bytes, *i + 3);
        let subject: NamedOrBlankNode = match bytes.get(*i) {
            Some(b'<') if !bytes[*i..].starts_with(b"<<") => {
                let iri = self.iri(line, i, TERM)?;
                NamedNode::new_unchecked(self.text(line, &iri)).into()
            }
            Some(b'_') => {
                let label = self.blank(line, i, TERM)?;
                BlankNode::new_unchecked(self.text(line, &label)).into()
            }
            _ => {
                return Err((
                    *i..*i + 1,
                    "expected an IRI or a blank node as the triple term's subject",
                ));
            }
        };
        *i = skip_space(bytes, *i);
        if bytes.get(*i) != Some(&b'<') || bytes[*i..].starts_with(b"<<") {
            return Err((*i..*i + 1, "expected an IRI as the triple term's predicate"));
        }
        let iri = self.iri(line, i, TERM)?;
        let predicate = NamedNode::new_unchecked(self.text(line, &iri));
        *i = skip_space(bytes, *i);
        let object: Term = match self.object(line, i, depth)? {
            Object::Triple => match self.triple_term.take() {
                Some(triple) => triple.into(),
                None => unreachable!("a triple term object is kept in `triple_term`"),
            },
            object => self.object_ref(line, &object).into_owned(),
        };
        *i = skip_space(bytes, *i);
        if !bytes[*i..].starts_with(b")>>") {
            return Err((*i..*i + 1, "expected ')>>' at the end of the triple term"));
        }
        *i += 3;
        Ok(Triple::new(subject, predicate, object))
    }

    /// `IRIREF` at `*i` (on '<'); escapes decoded into scratch string `slot`.
    fn iri(&mut self, line: &str, i: &mut usize, slot: usize) -> Result<Text, Failure> {
        let bytes = line.as_bytes();
        let start = *i + 1;
        let Some(length) = memchr(b'>', &bytes[start..]) else {
            return Err((*i..bytes.len(), "an IRI without its closing '>'"));
        };
        let end = start + length;
        let content = &bytes[start..end];
        let text = if let Some(bad) = content.iter().position(|&b| NOT_IN_IRI[b as usize]) {
            if content[bad] != b'\\' {
                return Err((start + bad..start + bad + 1, "a character IRIs can't hold"));
            }
            // Escapes: decode into the scratch string.
            let out = &mut self.scratch[slot];
            out.clear();
            let mut j = start;
            while j < end {
                let b = bytes[j];
                if b == b'\\' {
                    let c = unicode_escape(bytes, j)
                        .ok_or((j..j + 2, "an invalid escape in an IRI"))?;
                    if (c as u32) < 0x80 && NOT_IN_IRI[c as usize] {
                        return Err((j..j + 2, "an escape for a character IRIs can't hold"));
                    }
                    out.push(c);
                    j += if bytes[j + 1] == b'u' { 6 } else { 10 };
                } else if NOT_IN_IRI[b as usize] {
                    return Err((j..j + 1, "a character IRIs can't hold"));
                } else {
                    let c = line[j..].chars().next().unwrap_or_default();
                    out.push(c);
                    j += c.len_utf8();
                }
            }
            Text::Scratch(slot)
        } else {
            Text::Line(start..end)
        };
        if !self.unchecked {
            let value = match &text {
                Text::Line(range) => &line[range.clone()],
                Text::Scratch(k) => self.scratch[*k].as_str(),
            };
            if Iri::parse(value).is_err() {
                return Err((start..end, "not an absolute IRI"));
            }
        }
        *i = end + 1;
        Ok(text)
    }

    /// `BLANK_NODE_LABEL` at `*i` (on '_'), named as the parser's mode says.
    fn blank(&mut self, line: &str, i: &mut usize, slot: usize) -> Result<Text, Failure> {
        let bytes = line.as_bytes();
        if bytes.get(*i + 1) != Some(&b':') {
            return Err((*i..*i + 2, "expected '_:' for a blank node"));
        }
        let start = *i + 2;
        let mut chars = line[start..].char_indices();
        let mut end = match chars.next() {
            Some((_, c)) if is_pn_chars_u(c) || c.is_ascii_digit() => start + c.len_utf8(),
            _ => return Err((start..start + 1, "an invalid blank node label")),
        };
        for (at, c) in chars {
            if is_pn_chars(c) || c == '.' {
                end = start + at + c.len_utf8();
            } else {
                break;
            }
        }
        // A label can't end with '.': that is the statement's end.
        while bytes[end - 1] == b'.' {
            end -= 1;
        }
        *i = end;
        let label = &line[start..end];
        Ok(match self.blank_nodes {
            BlankNodes::AsWritten => Text::Line(start..end),
            BlankNodes::Fresh(_) => {
                let mut out = std::mem::take(&mut self.scratch[slot]);
                self.blank_nodes.name(label, &mut out);
                self.scratch[slot] = out;
                Text::Scratch(slot)
            }
        })
    }

    /// `STRING_LITERAL_QUOTE` at `*i` (on '"').
    fn string(&mut self, line: &str, i: &mut usize) -> Result<Text, Failure> {
        let bytes = line.as_bytes();
        let start = *i + 1;
        let Some(stop) = memchr2(b'"', b'\\', &bytes[start..]) else {
            return Err((*i..bytes.len(), "a string without its closing '\"'"));
        };
        if bytes[start + stop] == b'"' {
            *i = start + stop + 1;
            return Ok(Text::Line(start..start + stop));
        }
        // Escapes: decode into the scratch string.
        let out = &mut self.scratch[OBJECT];
        out.clear();
        out.push_str(&line[start..start + stop]);
        let mut j = start + stop;
        loop {
            match bytes.get(j) {
                None => return Err((*i..bytes.len(), "a string without its closing '\"'")),
                Some(b'"') => break,
                Some(b'\\') => {
                    let escaped = match bytes.get(j + 1) {
                        Some(b't') => '\t',
                        Some(b'b') => '\u{8}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b'f') => '\u{c}',
                        Some(b'"') => '"',
                        Some(b'\'') => '\'',
                        Some(b'\\') => '\\',
                        Some(b'u' | b'U') => {
                            let c =
                                unicode_escape(bytes, j).ok_or((j..j + 2, "an invalid escape"))?;
                            out.push(c);
                            j += if bytes[j + 1] == b'u' { 6 } else { 10 };
                            continue;
                        }
                        _ => return Err((j..j + 2, "an invalid escape")),
                    };
                    out.push(escaped);
                    j += 2;
                }
                Some(_) => {
                    let next = memchr2(b'"', b'\\', &bytes[j..]).map_or(bytes.len(), |k| j + k);
                    out.push_str(&line[j..next]);
                    j = next;
                }
            }
        }
        *i = j + 1;
        Ok(Text::Scratch(OBJECT))
    }

    /// `LANG_DIR` at `*i` (on '@'): `[a-zA-Z]+ ('-' [a-zA-Z0-9]+)* ('--' [a-zA-Z]+)?`, the
    /// tag in lower case, and the base direction if there is one.
    fn language(
        &mut self,
        line: &str,
        i: &mut usize,
    ) -> Result<(Text, Option<BaseDirection>), Failure> {
        let bytes = line.as_bytes();
        let LangDir {
            tag: range,
            direction,
            end,
        } = lang_dir(bytes, *i)?;
        *i = end;
        let (start, end) = (range.start, range.end);
        let tag = &line[start..end];
        if !self.unchecked && !nrese_rdf::language::is_well_formed(tag) {
            return Err((start..end, "a language tag that isn't well-formed BCP 47"));
        }
        if tag.bytes().any(|b| b.is_ascii_uppercase()) {
            let out = &mut self.scratch[LANGUAGE];
            out.clear();
            out.push_str(tag);
            out.make_ascii_lowercase();
            Ok((Text::Scratch(LANGUAGE), direction))
        } else {
            Ok((Text::Line(start..end), direction))
        }
    }
}

/// A `LANG_DIR`: where its tag is, its base direction, and where it ends.
struct LangDir {
    tag: Range<usize>,
    direction: Option<BaseDirection>,
    end: usize,
}

/// `LANG_DIR` at `at` (on '@'): the tag, and the base direction of a `--ltr` or `--rtl`
/// suffix.
fn lang_dir(bytes: &[u8], at: usize) -> Result<LangDir, Failure> {
    let start = at + 1;
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
        end += 1;
    }
    if end == start {
        return Err((at..at + 1, "an empty language tag"));
    }
    while bytes.get(end) == Some(&b'-') && bytes.get(end + 1) != Some(&b'-') {
        let part = end + 1;
        let mut stop = part;
        while stop < bytes.len() && bytes[stop].is_ascii_alphanumeric() {
            stop += 1;
        }
        if stop == part {
            return Err((end..end + 1, "an empty language subtag"));
        }
        end = stop;
    }
    if !bytes[end..].starts_with(b"--") {
        return Ok(LangDir {
            tag: start..end,
            direction: None,
            end,
        });
    }
    let from = end + 2;
    let mut stop = from;
    while stop < bytes.len() && bytes[stop].is_ascii_alphabetic() {
        stop += 1;
    }
    let direction = match &bytes[from..stop] {
        b"ltr" => BaseDirection::Ltr,
        b"rtl" => BaseDirection::Rtl,
        _ => {
            return Err((
                end..stop.max(from + 1),
                "a base direction other than ltr or rtl",
            ));
        }
    };
    Ok(LangDir {
        tag: start..end,
        direction: Some(direction),
        end: stop,
    })
}

/// `rdf:langString` or `rdf:dirLangString`: a literal gets them from a language tag only.
pub(crate) fn is_language_string(datatype: &str) -> bool {
    datatype == nrese_rdf::vocab::rdf::LANG_STRING.as_str()
        || datatype == nrese_rdf::vocab::rdf::DIR_LANG_STRING.as_str()
}

fn skip_space(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// `PN_CHARS_U`. The N-Triples recommendation also lists ':', an erratum the W3C test suite
/// (`nt-syntax-bad-bnode-01`) rejects, as Turtle's grammar does.
pub(crate) fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}

pub(crate) fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || matches!(c, '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// A syntax error at bytes `range` of a line.
fn syntax(
    bytes: &[u8],
    line: u64,
    offset: u64,
    range: Range<usize>,
    message: &str,
) -> RdfSyntaxError {
    let position = |at: usize| {
        let at = at.min(bytes.len());
        TextPosition {
            line,
            column: String::from_utf8_lossy(&bytes[..at]).chars().count() as u64,
            offset: offset + at as u64,
        }
    };
    RdfSyntaxError::new(message, position(range.start)..position(range.end))
}

// ---------------------------------------------------------------------------------------
// Writing

/// Bytes a string can't hold as they are (N-Triples §4): `"`, `\\`, the control characters
/// and DEL. 0xEF may start U+FFFE or U+FFFF, which are written as `\\u` escapes too.
const fn special() -> [bool; 256] {
    let mut table = [false; 256];
    let mut b = 0;
    while b < 0x20 {
        table[b] = true;
        b += 1;
    }
    table[b'"' as usize] = true;
    table[b'\\' as usize] = true;
    table[0x7F] = true;
    table[0xEF] = true;
    table
}
const SPECIAL: [bool; 256] = special();

/// `text` quoted and escaped as N-Triples writes a string: what needs no escape is
/// copied in one piece.
fn quoted(out: &mut Vec<u8>, text: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = text.as_bytes();
    out.push(b'"');
    let mut run = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !SPECIAL[b as usize] {
            i += 1;
            continue;
        }
        if b == 0xEF {
            // U+FFFE and U+FFFF; any other character starting with 0xEF is written as is.
            match bytes.get(i + 1..i + 3) {
                Some([0xBF, last @ (0xBE | 0xBF)]) => {
                    out.extend_from_slice(&bytes[run..i]);
                    out.extend_from_slice(if *last == 0xBE {
                        b"\\uFFFE"
                    } else {
                        b"\\uFFFF"
                    });
                    i += 3;
                    run = i;
                }
                _ => i += 1,
            }
            continue;
        }
        out.extend_from_slice(&bytes[run..i]);
        match b {
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0C => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ => out.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(b >> 4) as usize],
                HEX[(b & 15) as usize],
            ]),
        }
        i += 1;
        run = i;
    }
    out.extend_from_slice(&bytes[run..]);
    out.push(b'"');
}

fn named(out: &mut Vec<u8>, iri: &str) {
    out.push(b'<');
    out.extend_from_slice(iri.as_bytes());
    out.push(b'>');
}

fn node(out: &mut Vec<u8>, node: NamedOrBlankNodeRef<'_>) {
    match node {
        NamedOrBlankNodeRef::NamedNode(n) => named(out, n.as_str()),
        NamedOrBlankNodeRef::BlankNode(b) => {
            out.extend_from_slice(b"_:");
            out.extend_from_slice(b.as_str().as_bytes());
        }
    }
}

fn term(out: &mut Vec<u8>, term: TermRef<'_>) {
    match term {
        TermRef::NamedNode(n) => named(out, n.as_str()),
        TermRef::BlankNode(b) => {
            out.extend_from_slice(b"_:");
            out.extend_from_slice(b.as_str().as_bytes());
        }
        TermRef::Literal(literal) => {
            quoted(out, literal.value());
            if let Some(language) = literal.language() {
                out.push(b'@');
                out.extend_from_slice(language.as_bytes());
                if let Some(direction) = literal.direction() {
                    out.extend_from_slice(b"--");
                    out.extend_from_slice(direction.as_str().as_bytes());
                }
            } else if literal.datatype() != nrese_rdf::vocab::xsd::STRING {
                out.extend_from_slice(b"^^");
                named(out, literal.datatype().as_str());
            }
        }
        TermRef::Triple(triple) => {
            out.extend_from_slice(b"<<( ");
            node(out, triple.subject.as_ref());
            out.push(b' ');
            named(out, triple.predicate.as_str());
            out.push(b' ');
            self::term(out, triple.object.as_ref());
            out.extend_from_slice(b" )>>");
        }
    }
}

/// Writes a quad as one N-Quads line (the same bytes as its `Display` form and " .").
pub(crate) fn write_quad(out: &mut Vec<u8>, quad: QuadRef<'_>) -> io::Result<()> {
    node(out, quad.subject);
    out.push(b' ');
    named(out, quad.predicate.as_str());
    out.push(b' ');
    term(out, quad.object);
    match quad.graph_name {
        GraphNameRef::DefaultGraph => {}
        GraphNameRef::NamedNode(n) => {
            out.push(b' ');
            named(out, n.as_str());
        }
        GraphNameRef::BlankNode(b) => {
            out.extend_from_slice(b" _:");
            out.extend_from_slice(b.as_str().as_bytes());
        }
    }
    out.extend_from_slice(b" .\n");
    Ok(())
}

/// Writes a triple as one N-Triples line.
pub(crate) fn write_triple(out: &mut Vec<u8>, triple: TripleRef<'_>) -> io::Result<()> {
    node(out, triple.subject);
    out.push(b' ');
    named(out, triple.predicate.as_str());
    out.push(b' ');
    term(out, triple.object);
    out.extend_from_slice(b" .\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use nrese_rdf::{BlankNode, GraphName, Literal, NamedNode, Quad};

    use super::*;

    /// The byte writer writes exactly what the terms' `Display` writes.
    #[test]
    fn writes_what_display_writes() {
        let mut text: String = (0_u32..0x80).filter_map(char::from_u32).collect();
        text.push_str("é€😀\u{FFFE}\u{FFFF}\u{FFFD}\u{EFFF}");
        let s = NamedNode::new_unchecked("http://e/s");
        let p = NamedNode::new_unchecked("http://e/p");
        let objects: Vec<nrese_rdf::Term> = vec![
            Literal::new_simple_literal(text.clone()).into(),
            Literal::new_language_tagged_literal_unchecked(text.clone(), "en-gb").into(),
            Literal::new_typed_literal(text.clone(), NamedNode::new_unchecked("http://e/dt"))
                .into(),
            BlankNode::new_unchecked("b1").into(),
            s.clone().into(),
            Literal::new_directional_language_tagged_literal_unchecked(
                text.clone(),
                "ar",
                nrese_rdf::BaseDirection::Rtl,
            )
            .into(),
            nrese_rdf::Triple::new(
                BlankNode::new_unchecked("b2"),
                p.clone(),
                nrese_rdf::Triple::new(s.clone(), p.clone(), Literal::new_simple_literal("x")),
            )
            .into(),
        ];
        for object in objects {
            for graph in [
                GraphName::DefaultGraph,
                NamedNode::new_unchecked("http://e/g").into(),
                BlankNode::new_unchecked("g").into(),
            ] {
                let quad = Quad::new(s.clone(), p.clone(), object.clone(), graph);
                let mut out = Vec::new();
                write_quad(&mut out, quad.as_ref()).unwrap();
                assert_eq!(String::from_utf8(out).unwrap(), format!("{quad} .\n"));
            }
            let quad = Quad::new(
                s.clone(),
                p.clone(),
                object.clone(),
                GraphName::DefaultGraph,
            );
            let mut out = Vec::new();
            write_triple(&mut out, quad.as_ref().into()).unwrap();
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!("{} .\n", TripleRef::from(quad.as_ref()))
            );
        }
    }
}
