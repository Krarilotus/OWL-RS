//! RDF terms (RDF 1.2 Concepts §3): IRIs ([`NamedNode`]), blank nodes, literals (with a
//! base direction where RDF 1.2 has one), triple terms, and their unions
//! ([`NamedOrBlankNode`], [`Term`], [`GraphName`]); each with a borrowed form (`…Ref`) that
//! costs no allocation. They print in N-Triples syntax.

use std::fmt::{self, Write};

use crate::iri::{Iri, IriParseError};
use crate::triple::Triple;
use crate::vocab::{rdf, xsd};

/// Why a text isn't a blank node identifier or a language tag.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {what} {text:?}")]
pub struct TermParseError {
    what: &'static str,
    text: String,
}

// ---------------------------------------------------------------------------------------
// IRIs

/// An IRI (RDF 1.1 Concepts §3.2).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NamedNode {
    iri: String,
}

/// A borrowed [`NamedNode`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NamedNodeRef<'a> {
    iri: &'a str,
}

impl NamedNode {
    /// `iri` if it is an absolute IRI (RFC 3987).
    pub fn new(iri: impl Into<String>) -> Result<Self, IriParseError> {
        Ok(Self::new_from_iri(Iri::parse(iri.into())?))
    }

    pub fn new_from_iri(iri: Iri<String>) -> Self {
        Self {
            iri: iri.into_inner(),
        }
    }

    /// Without validation: `iri` must be an absolute IRI.
    pub fn new_unchecked(iri: impl Into<String>) -> Self {
        Self { iri: iri.into() }
    }

    pub fn as_str(&self) -> &str {
        &self.iri
    }

    pub fn into_string(self) -> String {
        self.iri
    }

    pub fn as_ref(&self) -> NamedNodeRef<'_> {
        NamedNodeRef { iri: &self.iri }
    }
}

impl<'a> NamedNodeRef<'a> {
    pub fn new(iri: &'a str) -> Result<Self, IriParseError> {
        Iri::parse(iri)?;
        Ok(Self { iri })
    }

    pub const fn new_unchecked(iri: &'a str) -> Self {
        Self { iri }
    }

    pub const fn as_str(self) -> &'a str {
        self.iri
    }

    pub fn into_owned(self) -> NamedNode {
        NamedNode::new_unchecked(self.iri)
    }
}

impl fmt::Display for NamedNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl fmt::Display for NamedNodeRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.iri)
    }
}

impl<'a> From<&'a NamedNode> for NamedNodeRef<'a> {
    fn from(node: &'a NamedNode) -> Self {
        node.as_ref()
    }
}

impl From<NamedNodeRef<'_>> for NamedNode {
    fn from(node: NamedNodeRef<'_>) -> Self {
        node.into_owned()
    }
}

impl PartialEq<NamedNodeRef<'_>> for NamedNode {
    fn eq(&self, other: &NamedNodeRef<'_>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<NamedNode> for NamedNodeRef<'_> {
    fn eq(&self, other: &NamedNode) -> bool {
        self.as_str() == other.as_str()
    }
}

// ---------------------------------------------------------------------------------------
// Blank nodes

/// A blank node (RDF 1.1 Concepts §3.4), by its local identifier.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct BlankNode {
    id: String,
}

/// A borrowed [`BlankNode`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct BlankNodeRef<'a> {
    id: &'a str,
}

/// Whether `id` is a blank node label of N-Triples, Turtle and SPARQL (`BLANK_NODE_LABEL`
/// without its `_:`).
fn valid_blank_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(is_pn_chars_u(first) || first.is_ascii_digit()) {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    if rest.last() == Some(&'.') {
        return false;
    }
    rest.iter().all(|&c| is_pn_chars(c) || c == '.')
}

fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}

fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || matches!(c, '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

impl BlankNode {
    pub fn new(id: impl Into<String>) -> Result<Self, TermParseError> {
        let id = id.into();
        if valid_blank_id(&id) {
            Ok(Self { id })
        } else {
            Err(TermParseError {
                what: "blank node identifier",
                text: id,
            })
        }
    }

    pub fn new_unchecked(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }

    pub fn into_string(self) -> String {
        self.id
    }

    pub fn as_ref(&self) -> BlankNodeRef<'_> {
        BlankNodeRef { id: &self.id }
    }
}

/// A fresh blank node: 128 random bits in hexadecimal, starting with a letter (so the
/// identifier is also an RDF/XML `nodeID`).
impl Default for BlankNode {
    fn default() -> Self {
        loop {
            let id = format!("{:x}", rand::random::<u128>());
            if id.as_bytes()[0].is_ascii_alphabetic() {
                return Self { id };
            }
        }
    }
}

impl<'a> BlankNodeRef<'a> {
    pub fn new(id: &'a str) -> Result<Self, TermParseError> {
        if valid_blank_id(id) {
            Ok(Self { id })
        } else {
            Err(TermParseError {
                what: "blank node identifier",
                text: id.to_owned(),
            })
        }
    }

    pub const fn new_unchecked(id: &'a str) -> Self {
        Self { id }
    }

    pub const fn as_str(self) -> &'a str {
        self.id
    }

    pub fn into_owned(self) -> BlankNode {
        BlankNode::new_unchecked(self.id)
    }
}

impl fmt::Display for BlankNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl fmt::Display for BlankNodeRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "_:{}", self.id)
    }
}

impl<'a> From<&'a BlankNode> for BlankNodeRef<'a> {
    fn from(node: &'a BlankNode) -> Self {
        node.as_ref()
    }
}

impl From<BlankNodeRef<'_>> for BlankNode {
    fn from(node: BlankNodeRef<'_>) -> Self {
        node.into_owned()
    }
}

// ---------------------------------------------------------------------------------------
// Literals

/// A literal (RDF 1.2 Concepts §3.3): a lexical form with a language tag (and possibly a
/// base direction) or a datatype. A simple literal's datatype is `xsd:string`, a
/// language-tagged string's `rdf:langString`, a directional one's `rdf:dirLangString`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Literal {
    value: String,
    kind: Kind,
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Kind {
    Simple,
    Language(String),
    DirectionalLanguage(String, BaseDirection),
    Typed(NamedNode),
}

/// A borrowed [`Literal`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct LiteralRef<'a> {
    value: &'a str,
    kind: KindRef<'a>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum KindRef<'a> {
    Simple,
    Language(&'a str),
    DirectionalLanguage(&'a str, BaseDirection),
    Typed(NamedNodeRef<'a>),
}

/// The base direction of a directional language-tagged string (RDF 1.2 Concepts §3.3):
/// `ltr` or `rtl`, as in `"…"@ar--rtl`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum BaseDirection {
    Ltr,
    Rtl,
}

impl BaseDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

impl std::str::FromStr for BaseDirection {
    type Err = TermParseError;

    /// `ltr` or `rtl`, in lower case: RDF 1.2 allows no other spelling.
    fn from_str(text: &str) -> Result<Self, TermParseError> {
        match text {
            "ltr" => Ok(Self::Ltr),
            "rtl" => Ok(Self::Rtl),
            _ => Err(TermParseError {
                what: "base direction",
                text: text.to_owned(),
            }),
        }
    }
}

impl fmt::Display for BaseDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Literal {
    pub fn new_simple_literal(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            kind: Kind::Simple,
        }
    }

    /// A literal of `datatype`; of `xsd:string`, a simple literal.
    pub fn new_typed_literal(value: impl Into<String>, datatype: impl Into<NamedNode>) -> Self {
        let datatype = datatype.into();
        Self {
            value: value.into(),
            kind: if datatype == xsd::STRING {
                Kind::Simple
            } else {
                Kind::Typed(datatype)
            },
        }
    }

    /// A language-tagged string; the tag is checked against BCP 47 and put in lower case.
    pub fn new_language_tagged_literal(
        value: impl Into<String>,
        language: impl Into<String>,
    ) -> Result<Self, TermParseError> {
        Ok(Self::new_language_tagged_literal_unchecked(
            value,
            checked_language(language.into())?,
        ))
    }

    /// Without checking: `language` must be a lower-case BCP 47 tag.
    pub fn new_language_tagged_literal_unchecked(
        value: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        Self {
            value: value.into(),
            kind: Kind::Language(language.into()),
        }
    }

    /// A directional language-tagged string (RDF 1.2); the tag is checked against BCP 47
    /// and put in lower case.
    pub fn new_directional_language_tagged_literal(
        value: impl Into<String>,
        language: impl Into<String>,
        direction: BaseDirection,
    ) -> Result<Self, TermParseError> {
        Ok(Self::new_directional_language_tagged_literal_unchecked(
            value,
            checked_language(language.into())?,
            direction,
        ))
    }

    /// Without checking: `language` must be a lower-case BCP 47 tag.
    pub fn new_directional_language_tagged_literal_unchecked(
        value: impl Into<String>,
        language: impl Into<String>,
        direction: BaseDirection,
    ) -> Self {
        Self {
            value: value.into(),
            kind: Kind::DirectionalLanguage(language.into(), direction),
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn language(&self) -> Option<&str> {
        match &self.kind {
            Kind::Language(tag) | Kind::DirectionalLanguage(tag, _) => Some(tag),
            _ => None,
        }
    }

    /// The base direction of a directional language-tagged string.
    pub fn direction(&self) -> Option<BaseDirection> {
        match self.kind {
            Kind::DirectionalLanguage(_, direction) => Some(direction),
            _ => None,
        }
    }

    pub fn datatype(&self) -> NamedNodeRef<'_> {
        self.as_ref().datatype()
    }

    /// A simple literal or a language-tagged string (with or without a direction).
    pub fn is_plain(&self) -> bool {
        self.as_ref().is_plain()
    }

    pub fn as_ref(&self) -> LiteralRef<'_> {
        LiteralRef {
            value: &self.value,
            kind: match &self.kind {
                Kind::Simple => KindRef::Simple,
                Kind::Language(tag) => KindRef::Language(tag),
                Kind::DirectionalLanguage(tag, direction) => {
                    KindRef::DirectionalLanguage(tag, *direction)
                }
                Kind::Typed(datatype) => KindRef::Typed(datatype.as_ref()),
            },
        }
    }

    /// The lexical form, the datatype (`None` for a simple literal or a language-tagged
    /// string), the language tag and the base direction.
    pub fn destruct(
        self,
    ) -> (
        String,
        Option<NamedNode>,
        Option<String>,
        Option<BaseDirection>,
    ) {
        match self.kind {
            Kind::Simple => (self.value, None, None, None),
            Kind::Language(tag) => (self.value, None, Some(tag), None),
            Kind::DirectionalLanguage(tag, direction) => {
                (self.value, None, Some(tag), Some(direction))
            }
            Kind::Typed(datatype) => (self.value, Some(datatype), None, None),
        }
    }
}

/// `language` in lower case, if it is a well-formed BCP 47 tag.
fn checked_language(mut language: String) -> Result<String, TermParseError> {
    language.make_ascii_lowercase();
    if crate::language::is_well_formed(&language) {
        Ok(language)
    } else {
        Err(TermParseError {
            what: "language tag",
            text: language,
        })
    }
}

impl<'a> LiteralRef<'a> {
    pub const fn new_simple_literal(value: &'a str) -> Self {
        Self {
            value,
            kind: KindRef::Simple,
        }
    }

    pub fn new_typed_literal(value: &'a str, datatype: impl Into<NamedNodeRef<'a>>) -> Self {
        let datatype = datatype.into();
        Self {
            value,
            kind: if datatype == xsd::STRING {
                KindRef::Simple
            } else {
                KindRef::Typed(datatype)
            },
        }
    }

    pub const fn new_language_tagged_literal_unchecked(value: &'a str, language: &'a str) -> Self {
        Self {
            value,
            kind: KindRef::Language(language),
        }
    }

    pub const fn new_directional_language_tagged_literal_unchecked(
        value: &'a str,
        language: &'a str,
        direction: BaseDirection,
    ) -> Self {
        Self {
            value,
            kind: KindRef::DirectionalLanguage(language, direction),
        }
    }

    pub const fn value(self) -> &'a str {
        self.value
    }

    pub const fn language(self) -> Option<&'a str> {
        match self.kind {
            KindRef::Language(tag) | KindRef::DirectionalLanguage(tag, _) => Some(tag),
            _ => None,
        }
    }

    pub const fn direction(self) -> Option<BaseDirection> {
        match self.kind {
            KindRef::DirectionalLanguage(_, direction) => Some(direction),
            _ => None,
        }
    }

    pub fn datatype(self) -> NamedNodeRef<'a> {
        match self.kind {
            KindRef::Simple => xsd::STRING,
            KindRef::Language(_) => rdf::LANG_STRING,
            KindRef::DirectionalLanguage(..) => rdf::DIR_LANG_STRING,
            KindRef::Typed(datatype) => datatype,
        }
    }

    pub const fn is_plain(self) -> bool {
        matches!(
            self.kind,
            KindRef::Simple | KindRef::Language(_) | KindRef::DirectionalLanguage(..)
        )
    }

    pub fn into_owned(self) -> Literal {
        Literal {
            value: self.value.to_owned(),
            kind: match self.kind {
                KindRef::Simple => Kind::Simple,
                KindRef::Language(tag) => Kind::Language(tag.to_owned()),
                KindRef::DirectionalLanguage(tag, direction) => {
                    Kind::DirectionalLanguage(tag.to_owned(), direction)
                }
                KindRef::Typed(datatype) => Kind::Typed(datatype.into_owned()),
            },
        }
    }
}

/// `text` quoted and escaped as N-Triples writes a string (RDF 1.1 N-Triples §4: `ECHAR`,
/// and `UCHAR` for the other control characters).
pub fn write_quoted_str(text: &str, f: &mut impl Write) -> fmt::Result {
    f.write_char('"')?;
    for c in text.chars() {
        match c {
            '\u{8}' => f.write_str("\\b"),
            '\t' => f.write_str("\\t"),
            '\n' => f.write_str("\\n"),
            '\u{C}' => f.write_str("\\f"),
            '\r' => f.write_str("\\r"),
            '"' => f.write_str("\\\""),
            '\\' => f.write_str("\\\\"),
            '\0'..='\u{1F}' | '\u{7F}' | '\u{FFFE}' | '\u{FFFF}' => {
                write!(f, "\\u{:04X}", u32::from(c))
            }
            _ => f.write_char(c),
        }?;
    }
    f.write_char('"')
}

impl fmt::Display for LiteralRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_quoted_str(self.value, f)?;
        match self.kind {
            KindRef::Simple => Ok(()),
            KindRef::Language(tag) => write!(f, "@{tag}"),
            KindRef::DirectionalLanguage(tag, direction) => write!(f, "@{tag}--{direction}"),
            KindRef::Typed(datatype) => write!(f, "^^{datatype}"),
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl<'a> From<&'a Literal> for LiteralRef<'a> {
    fn from(literal: &'a Literal) -> Self {
        literal.as_ref()
    }
}

impl From<LiteralRef<'_>> for Literal {
    fn from(literal: LiteralRef<'_>) -> Self {
        literal.into_owned()
    }
}

impl From<&str> for Literal {
    fn from(value: &str) -> Self {
        Self::new_simple_literal(value)
    }
}

impl From<String> for Literal {
    fn from(value: String) -> Self {
        Self::new_simple_literal(value)
    }
}

impl From<bool> for Literal {
    fn from(value: bool) -> Self {
        Self::new_typed_literal(value.to_string(), xsd::BOOLEAN)
    }
}

macro_rules! integer_literals {
    ($($t:ty),*) => {$(
        impl From<$t> for Literal {
            fn from(value: $t) -> Self {
                Self::new_typed_literal(value.to_string(), xsd::INTEGER)
            }
        }
    )*};
}
integer_literals!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

impl From<f32> for Literal {
    fn from(value: f32) -> Self {
        let text = if value == f32::INFINITY {
            "INF".to_owned()
        } else if value == f32::NEG_INFINITY {
            "-INF".to_owned()
        } else {
            value.to_string()
        };
        Self::new_typed_literal(text, xsd::FLOAT)
    }
}

impl From<f64> for Literal {
    fn from(value: f64) -> Self {
        let text = if value == f64::INFINITY {
            "INF".to_owned()
        } else if value == f64::NEG_INFINITY {
            "-INF".to_owned()
        } else {
            value.to_string()
        };
        Self::new_typed_literal(text, xsd::DOUBLE)
    }
}

// ---------------------------------------------------------------------------------------
// Unions

macro_rules! union {
    ($owned:ident, $borrowed:ident { $($variant:ident($ty:ident, $ref:ident)),* }) => {
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub enum $owned {
            $($variant($ty),)*
        }

        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub enum $borrowed<'a> {
            $($variant($ref<'a>),)*
        }

        impl $owned {
            pub fn as_ref(&self) -> $borrowed<'_> {
                match self {
                    $(Self::$variant(x) => $borrowed::$variant(x.as_ref()),)*
                }
            }
        }

        impl $borrowed<'_> {
            pub fn into_owned(self) -> $owned {
                match self {
                    $(Self::$variant(x) => $owned::$variant(x.into_owned()),)*
                }
            }
        }

        impl fmt::Display for $owned {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.as_ref().fmt(f)
            }
        }

        impl fmt::Display for $borrowed<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(Self::$variant(x) => x.fmt(f),)*
                }
            }
        }

        impl<'a> From<&'a $owned> for $borrowed<'a> {
            fn from(value: &'a $owned) -> Self {
                value.as_ref()
            }
        }

        impl From<$borrowed<'_>> for $owned {
            fn from(value: $borrowed<'_>) -> Self {
                value.into_owned()
            }
        }

        $(
            impl From<$ty> for $owned {
                fn from(value: $ty) -> Self {
                    Self::$variant(value)
                }
            }

            impl<'a> From<$ref<'a>> for $borrowed<'a> {
                fn from(value: $ref<'a>) -> Self {
                    Self::$variant(value)
                }
            }

            impl<'a> From<&'a $ty> for $borrowed<'a> {
                fn from(value: &'a $ty) -> Self {
                    Self::$variant(value.as_ref())
                }
            }
        )*
    };
}

union!(NamedOrBlankNode, NamedOrBlankNodeRef {
    NamedNode(NamedNode, NamedNodeRef),
    BlankNode(BlankNode, BlankNodeRef)
});

/// Any RDF term: an IRI, a blank node, a literal, or a triple term (RDF 1.2 Concepts §3.6,
/// in object position only). Triple terms nest, so the owned form boxes them.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Term {
    NamedNode(NamedNode),
    BlankNode(BlankNode),
    Literal(Literal),
    Triple(Box<Triple>),
}

/// A borrowed [`Term`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum TermRef<'a> {
    NamedNode(NamedNodeRef<'a>),
    BlankNode(BlankNodeRef<'a>),
    Literal(LiteralRef<'a>),
    Triple(&'a Triple),
}

impl Term {
    pub fn as_ref(&self) -> TermRef<'_> {
        match self {
            Self::NamedNode(n) => TermRef::NamedNode(n.as_ref()),
            Self::BlankNode(b) => TermRef::BlankNode(b.as_ref()),
            Self::Literal(l) => TermRef::Literal(l.as_ref()),
            Self::Triple(t) => TermRef::Triple(t),
        }
    }
}

impl TermRef<'_> {
    pub fn into_owned(self) -> Term {
        match self {
            Self::NamedNode(n) => Term::NamedNode(n.into_owned()),
            Self::BlankNode(b) => Term::BlankNode(b.into_owned()),
            Self::Literal(l) => Term::Literal(l.into_owned()),
            Self::Triple(t) => Term::Triple(Box::new(t.clone())),
        }
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

/// A triple term prints as N-Triples 1.2 writes it: `<<( s p o )>>`.
impl fmt::Display for TermRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => n.fmt(f),
            Self::BlankNode(b) => b.fmt(f),
            Self::Literal(l) => l.fmt(f),
            Self::Triple(t) => write!(f, "<<( {t} )>>"),
        }
    }
}

impl<'a> From<&'a Term> for TermRef<'a> {
    fn from(term: &'a Term) -> Self {
        term.as_ref()
    }
}

impl From<TermRef<'_>> for Term {
    fn from(term: TermRef<'_>) -> Self {
        term.into_owned()
    }
}

macro_rules! term_variants {
    ($($variant:ident($ty:ident, $ref:ident)),*) => {$(
        impl From<$ty> for Term {
            fn from(value: $ty) -> Self {
                Self::$variant(value)
            }
        }

        impl<'a> From<$ref<'a>> for TermRef<'a> {
            fn from(value: $ref<'a>) -> Self {
                Self::$variant(value)
            }
        }

        impl<'a> From<&'a $ty> for TermRef<'a> {
            fn from(value: &'a $ty) -> Self {
                Self::$variant(value.as_ref())
            }
        }
    )*};
}
term_variants!(
    NamedNode(NamedNode, NamedNodeRef),
    BlankNode(BlankNode, BlankNodeRef),
    Literal(Literal, LiteralRef)
);

impl From<Triple> for Term {
    fn from(triple: Triple) -> Self {
        Self::Triple(Box::new(triple))
    }
}

impl From<Box<Triple>> for Term {
    fn from(triple: Box<Triple>) -> Self {
        Self::Triple(triple)
    }
}

impl<'a> From<&'a Triple> for TermRef<'a> {
    fn from(triple: &'a Triple) -> Self {
        Self::Triple(triple)
    }
}

/// The subject of a triple: an IRI or a blank node.
pub type Subject = NamedOrBlankNode;
pub type SubjectRef<'a> = NamedOrBlankNodeRef<'a>;

impl NamedOrBlankNode {
    pub fn is_named_node(&self) -> bool {
        matches!(self, Self::NamedNode(_))
    }

    pub fn is_blank_node(&self) -> bool {
        matches!(self, Self::BlankNode(_))
    }
}

impl Term {
    pub fn is_named_node(&self) -> bool {
        matches!(self, Self::NamedNode(_))
    }

    pub fn is_blank_node(&self) -> bool {
        matches!(self, Self::BlankNode(_))
    }

    pub fn is_literal(&self) -> bool {
        matches!(self, Self::Literal(_))
    }

    pub fn is_triple(&self) -> bool {
        matches!(self, Self::Triple(_))
    }
}

impl TermRef<'_> {
    pub fn is_named_node(&self) -> bool {
        matches!(self, Self::NamedNode(_))
    }

    pub fn is_blank_node(&self) -> bool {
        matches!(self, Self::BlankNode(_))
    }

    pub fn is_literal(&self) -> bool {
        matches!(self, Self::Literal(_))
    }

    pub fn is_triple(&self) -> bool {
        matches!(self, Self::Triple(_))
    }
}

impl From<NamedOrBlankNode> for Term {
    fn from(node: NamedOrBlankNode) -> Self {
        match node {
            NamedOrBlankNode::NamedNode(n) => Self::NamedNode(n),
            NamedOrBlankNode::BlankNode(b) => Self::BlankNode(b),
        }
    }
}

impl<'a> From<NamedOrBlankNodeRef<'a>> for TermRef<'a> {
    fn from(node: NamedOrBlankNodeRef<'a>) -> Self {
        match node {
            NamedOrBlankNodeRef::NamedNode(n) => Self::NamedNode(n),
            NamedOrBlankNodeRef::BlankNode(b) => Self::BlankNode(b),
        }
    }
}

/// A term that isn't an IRI or a blank node (or not the kind asked for).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not an IRI or a blank node")]
pub struct NotANodeError(pub String);

impl TryFrom<Term> for NamedOrBlankNode {
    type Error = NotANodeError;

    fn try_from(term: Term) -> Result<Self, NotANodeError> {
        match term {
            Term::NamedNode(n) => Ok(n.into()),
            Term::BlankNode(b) => Ok(b.into()),
            other => Err(NotANodeError(other.to_string())),
        }
    }
}

impl<'a> TryFrom<TermRef<'a>> for NamedOrBlankNodeRef<'a> {
    type Error = NotANodeError;

    fn try_from(term: TermRef<'a>) -> Result<Self, NotANodeError> {
        match term {
            TermRef::NamedNode(n) => Ok(n.into()),
            TermRef::BlankNode(b) => Ok(b.into()),
            other => Err(NotANodeError(other.to_string())),
        }
    }
}

impl TryFrom<Term> for NamedNode {
    type Error = NotANodeError;

    fn try_from(term: Term) -> Result<Self, NotANodeError> {
        match term {
            Term::NamedNode(n) => Ok(n),
            other => Err(NotANodeError(other.to_string())),
        }
    }
}

impl TryFrom<Term> for Literal {
    type Error = NotANodeError;

    fn try_from(term: Term) -> Result<Self, NotANodeError> {
        match term {
            Term::Literal(l) => Ok(l),
            other => Err(NotANodeError(other.to_string())),
        }
    }
}

macro_rules! literal_terms {
    ($($t:ty),*) => {$(
        impl From<$t> for Term {
            fn from(value: $t) -> Self {
                Self::Literal(Literal::from(value))
            }
        }
    )*};
}
literal_terms!(bool, i32, i64, u64, f32, f64, &str, String);

/// The graph a quad is in: the default graph, or a named graph (an IRI or a blank node).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub enum GraphName {
    NamedNode(NamedNode),
    BlankNode(BlankNode),
    #[default]
    DefaultGraph,
}

/// A borrowed [`GraphName`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub enum GraphNameRef<'a> {
    NamedNode(NamedNodeRef<'a>),
    BlankNode(BlankNodeRef<'a>),
    #[default]
    DefaultGraph,
}

impl GraphName {
    pub fn is_default_graph(&self) -> bool {
        matches!(self, Self::DefaultGraph)
    }

    pub fn as_ref(&self) -> GraphNameRef<'_> {
        match self {
            Self::NamedNode(n) => GraphNameRef::NamedNode(n.as_ref()),
            Self::BlankNode(b) => GraphNameRef::BlankNode(b.as_ref()),
            Self::DefaultGraph => GraphNameRef::DefaultGraph,
        }
    }
}

impl GraphNameRef<'_> {
    pub fn is_default_graph(&self) -> bool {
        matches!(self, Self::DefaultGraph)
    }

    pub fn into_owned(self) -> GraphName {
        match self {
            Self::NamedNode(n) => GraphName::NamedNode(n.into_owned()),
            Self::BlankNode(b) => GraphName::BlankNode(b.into_owned()),
            Self::DefaultGraph => GraphName::DefaultGraph,
        }
    }
}

impl fmt::Display for GraphName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl fmt::Display for GraphNameRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => n.fmt(f),
            Self::BlankNode(b) => b.fmt(f),
            Self::DefaultGraph => f.write_str("DEFAULT"),
        }
    }
}

impl From<NamedNode> for GraphName {
    fn from(n: NamedNode) -> Self {
        Self::NamedNode(n)
    }
}

impl From<BlankNode> for GraphName {
    fn from(b: BlankNode) -> Self {
        Self::BlankNode(b)
    }
}

impl From<NamedOrBlankNode> for GraphName {
    fn from(node: NamedOrBlankNode) -> Self {
        match node {
            NamedOrBlankNode::NamedNode(n) => Self::NamedNode(n),
            NamedOrBlankNode::BlankNode(b) => Self::BlankNode(b),
        }
    }
}

impl<'a> From<NamedNodeRef<'a>> for GraphNameRef<'a> {
    fn from(n: NamedNodeRef<'a>) -> Self {
        Self::NamedNode(n)
    }
}

impl<'a> From<BlankNodeRef<'a>> for GraphNameRef<'a> {
    fn from(b: BlankNodeRef<'a>) -> Self {
        Self::BlankNode(b)
    }
}

impl<'a> From<NamedOrBlankNodeRef<'a>> for GraphNameRef<'a> {
    fn from(node: NamedOrBlankNodeRef<'a>) -> Self {
        match node {
            NamedOrBlankNodeRef::NamedNode(n) => Self::NamedNode(n),
            NamedOrBlankNodeRef::BlankNode(b) => Self::BlankNode(b),
        }
    }
}

impl<'a> From<&'a GraphName> for GraphNameRef<'a> {
    fn from(g: &'a GraphName) -> Self {
        g.as_ref()
    }
}

impl From<GraphNameRef<'_>> for GraphName {
    fn from(g: GraphNameRef<'_>) -> Self {
        g.into_owned()
    }
}

// ---------------------------------------------------------------------------------------
// Variables

/// A SPARQL query variable, by name (without `?`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Variable {
    name: String,
}

impl Variable {
    /// `name` if it is a SPARQL `VARNAME`.
    pub fn new(name: impl Into<String>) -> Result<Self, TermParseError> {
        let name = name.into();
        let mut chars = name.chars();
        let valid = chars
            .next()
            .is_some_and(|c| is_pn_chars_u(c) || c.is_ascii_digit())
            && chars.all(|c| {
                is_pn_chars_u(c)
                    || c.is_ascii_digit()
                    || c == '\u{B7}'
                    || matches!(c, '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
            });
        if valid {
            Ok(Self { name })
        } else {
            Err(TermParseError {
                what: "variable name",
                text: name,
            })
        }
    }

    pub fn new_unchecked(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }

    pub fn into_string(self) -> String {
        self.name
    }
}

impl fmt::Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "?{}", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_print_as_n_triples() {
        assert_eq!(
            NamedNode::new_unchecked("http://e/a").to_string(),
            "<http://e/a>"
        );
        assert_eq!(BlankNode::new_unchecked("b1").to_string(), "_:b1");
        assert_eq!(
            Literal::new_simple_literal("a\"b\\c\nd\u{1}").to_string(),
            "\"a\\\"b\\\\c\\nd\\u0001\""
        );
        assert_eq!(
            Literal::new_language_tagged_literal("x", "EN-gb")
                .unwrap()
                .to_string(),
            "\"x\"@en-gb"
        );
        assert_eq!(
            Literal::from(true).to_string(),
            "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"
        );
        assert_eq!(
            Literal::from(1.5f64).to_string(),
            "\"1.5\"^^<http://www.w3.org/2001/XMLSchema#double>"
        );
        assert_eq!(Literal::from(f64::NEG_INFINITY).value(), "-INF");
        assert_eq!(Variable::new_unchecked("x").to_string(), "?x");
        assert_eq!(GraphName::DefaultGraph.to_string(), "DEFAULT");
    }

    #[test]
    fn rdf_1_2_terms_print_as_n_triples_1_2() {
        let rtl = Literal::new_directional_language_tagged_literal("x", "AR", BaseDirection::Rtl)
            .unwrap();
        assert_eq!(rtl.to_string(), "\"x\"@ar--rtl");
        assert_eq!(rtl.language(), Some("ar"));
        assert_eq!(rtl.direction(), Some(BaseDirection::Rtl));
        assert_eq!(rtl.datatype(), rdf::DIR_LANG_STRING);
        assert!(rtl.is_plain());
        assert_ne!(
            rtl,
            Literal::new_language_tagged_literal_unchecked("x", "ar")
        );
        assert!(
            Literal::new_directional_language_tagged_literal("x", "a-", BaseDirection::Ltr)
                .is_err()
        );
        assert_eq!("ltr".parse::<BaseDirection>().unwrap(), BaseDirection::Ltr);
        assert!("LTR".parse::<BaseDirection>().is_err());
        assert!("up".parse::<BaseDirection>().is_err());
        let inner = Triple::new(
            BlankNode::new_unchecked("b"),
            NamedNode::new_unchecked("http://e/p"),
            rtl.clone(),
        );
        let nested: Term = Triple::new(
            NamedNode::new_unchecked("http://e/s"),
            NamedNode::new_unchecked("http://e/q"),
            inner,
        )
        .into();
        assert_eq!(
            nested.to_string(),
            "<<( <http://e/s> <http://e/q> <<( _:b <http://e/p> \"x\"@ar--rtl )>> )>>"
        );
        assert!(nested.is_triple());
        assert_eq!(nested.as_ref().into_owned(), nested);
        assert!(NamedOrBlankNode::try_from(nested).is_err());
        let (value, datatype, language, direction) = rtl.destruct();
        assert_eq!(
            (value.as_str(), datatype, language.as_deref(), direction),
            ("x", None, Some("ar"), Some(BaseDirection::Rtl))
        );
    }

    #[test]
    fn a_string_typed_literal_is_simple() {
        let l = Literal::new_typed_literal("a", xsd::STRING);
        assert_eq!(l, Literal::new_simple_literal("a"));
        assert_eq!(l.datatype(), xsd::STRING);
        assert!(l.is_plain());
        let tagged = Literal::new_language_tagged_literal_unchecked("a", "de");
        assert_eq!(tagged.datatype(), rdf::LANG_STRING);
    }

    #[test]
    fn identifiers_and_tags_are_checked() {
        assert!(BlankNode::new("a1").is_ok());
        assert!(BlankNode::new("1a").is_ok());
        assert!(BlankNode::new("").is_err());
        assert!(BlankNode::new("a.").is_err());
        assert!(BlankNode::new("a b").is_err());
        assert!(BlankNode::default().as_str().as_bytes()[0].is_ascii_alphabetic());
        assert!(Literal::new_language_tagged_literal("x", "en-").is_err());
        assert!(Literal::new_language_tagged_literal("x", "123").is_err());
        assert!(Literal::new_language_tagged_literal("x", "zh-Hant-TW").is_ok());
        assert!(Literal::new_language_tagged_literal("x", "en-a").is_err());
        assert!(Variable::new("x_1").is_ok());
        assert!(Variable::new("x y").is_err());
        assert!(NamedNode::new("not an iri").is_err());
    }

    #[test]
    fn owned_and_borrowed_convert() {
        let t: Term = Literal::new_simple_literal("v").into();
        let r = t.as_ref();
        assert_eq!(r.into_owned(), t);
        let n: NamedOrBlankNode = BlankNode::new_unchecked("x").into();
        assert!(NamedOrBlankNode::try_from(Term::from(n.clone())).is_ok());
        assert!(NamedOrBlankNode::try_from(t).is_err());
        assert_eq!(GraphName::from(n).as_ref().into_owned().to_string(), "_:x");
    }
}
