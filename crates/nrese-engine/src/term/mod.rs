//! Term encoding: every RDF term is represented by a 64-bit [`TermId`].
//!
//! Layout: the top 4 bits are the [`TermKind`] tag, the low 60 bits the payload.
//! Dictionary-backed kinds (IRI, blank node, literal) carry an index into the
//! [`Dictionary`](dictionary::Dictionary); inline kinds carry the value itself.
//!
//! Inline encoding is only used when the lexical form is the canonical form of the value,
//! so RDF term identity is preserved (`"01"^^xsd:integer` and `"1"^^xsd:integer` are
//! different terms and get different ids). See ADR-0002.

pub(crate) mod derived;
pub(crate) mod dictionary;
pub(crate) mod hash;
mod inline;
pub(crate) mod offsets;
pub(crate) mod order;
mod strings;
pub mod text;
pub mod vectors;
pub(crate) mod vocabulary;
#[cfg(test)]
mod vocabulary_study;

pub use dictionary::{Dictionary, DictionaryStats, TermView};
pub use strings::{Placement, StringTest};
pub use text::{TextMatch, TextQuery};
pub use vectors::{VectorQuery, VectorSearchReport, VectorStrategy};

/// Number of bits used by the payload of a [`TermId`].
pub const PAYLOAD_BITS: u32 = 60;
const PAYLOAD_MASK: u64 = (1 << PAYLOAD_BITS) - 1;

/// Kind tag stored in the top 4 bits of a [`TermId`].
///
/// Dictionary literals are split by datatype class, so the id alone tells an executor
/// whether a term can be numeric: a numeric FILTER over a predicate reads its inline
/// numeric ranges and its [`TypedLiteral`](Self::TypedLiteral) range, and skips strings and
/// language-tagged strings without decoding them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum TermKind {
    /// Reserved: only the default graph marker uses this tag.
    DefaultGraph = 0,
    Iri = 1,
    BlankNode = 2,
    /// A simple literal (`xsd:string`).
    String = 3,
    /// Inline canonical `xsd:integer` (60-bit, offset binary, so ids sort by value).
    Integer = 4,
    /// Inline canonical `xsd:boolean`.
    Boolean = 5,
    /// Inline canonical `xsd:decimal` (see `inline` for ranges).
    Decimal = 6,
    /// Inline canonical `xsd:date`.
    Date = 7,
    /// Inline canonical `xsd:dateTime`.
    DateTime = 8,
    /// A language-tagged string (`rdf:langString`).
    LangString = 9,
    /// A literal of any other datatype, including non-canonical forms of the inline ones.
    TypedLiteral = 10,
    /// A triple term (RDF 1.2); after the literals, as SPARQL 1.2 orders terms.
    Triple = 11,
    /// Inline canonical literals of the datatypes derived from `xsd:integer` (`xsd:int`,
    /// `xsd:long`, `xsd:nonNegativeInteger`, ...): the value (56-bit, offset binary) and a
    /// datatype code below it, so ids sort by value. A store created before this kind
    /// keeps such literals in the dictionary ([`TypedLiteral`](Self::TypedLiteral)) for
    /// its whole life (`Dictionary::integers_in_dictionary`). The tag comes after the
    /// others, which older stores already use.
    DerivedInteger = 12,
}

impl TermKind {
    const fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::DefaultGraph,
            1 => Self::Iri,
            2 => Self::BlankNode,
            3 => Self::String,
            4 => Self::Integer,
            5 => Self::Boolean,
            6 => Self::Decimal,
            7 => Self::Date,
            8 => Self::DateTime,
            9 => Self::LangString,
            10 => Self::TypedLiteral,
            11 => Self::Triple,
            12 => Self::DerivedInteger,
            _ => return None,
        })
    }

    /// True for kinds whose payload is a dictionary index.
    pub const fn is_dictionary(self) -> bool {
        matches!(
            self,
            Self::Iri
                | Self::BlankNode
                | Self::String
                | Self::LangString
                | Self::TypedLiteral
                | Self::Triple
        )
    }

    /// True for dictionary-backed literal kinds.
    pub const fn is_dictionary_literal(self) -> bool {
        matches!(self, Self::String | Self::LangString | Self::TypedLiteral)
    }

    /// True for kinds whose payload is the value itself.
    pub const fn is_inline(self) -> bool {
        matches!(
            self,
            Self::Integer
                | Self::Boolean
                | Self::Decimal
                | Self::Date
                | Self::DateTime
                | Self::DerivedInteger
        )
    }
}

/// A 64-bit encoded RDF term. Ordering is by kind, then payload; it is an internal
/// storage order, not SPARQL `ORDER BY` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct TermId(u64);

impl TermId {
    /// The default graph. Also the smallest id, so default-graph quads sort first.
    pub const DEFAULT_GRAPH: Self = Self(0);

    pub(crate) const fn new(kind: TermKind, payload: u64) -> Self {
        debug_assert!(payload <= PAYLOAD_MASK);
        Self(((kind as u64) << PAYLOAD_BITS) | (payload & PAYLOAD_MASK))
    }

    /// Reconstructs an id from its raw representation (storage, WAL).
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub fn kind(self) -> TermKind {
        TermKind::from_tag((self.0 >> PAYLOAD_BITS) as u8).unwrap_or(TermKind::DefaultGraph)
    }

    pub const fn payload(self) -> u64 {
        self.0 & PAYLOAD_MASK
    }

    pub const fn is_default_graph(self) -> bool {
        self.0 == 0
    }

    /// Value of an inline integer term (`xsd:integer` or a datatype derived from it), if
    /// this is one.
    pub fn as_inline_integer(self) -> Option<i64> {
        match self.kind() {
            TermKind::Integer => Some(inline::decode_integer(self.payload())),
            TermKind::DerivedInteger => Some(inline::decode_derived(self.payload()).0),
            _ => None,
        }
    }

    /// The ids of inline integer-derived literals ([`TermKind::DerivedInteger`]) with a
    /// value in `low..=high`, as one range (the value leads the payload); values beyond
    /// the inline range are clamped to it.
    pub fn derived_integer_range(low: i64, high: i64) -> Option<(Self, Self)> {
        inline::derived_range(low, high)
    }

    /// The inline id of the canonical integer `value`, if it is in the inline range. Inline
    /// integer ids sort by value, so numeric bounds become id bounds.
    pub fn inline_integer(value: i64) -> Option<Self> {
        inline::integer_id(value)
    }

    /// For an inline date, an id one local day earlier (`later == false`) or later, with the
    /// smallest or largest timezone code: widened FILTER range bounds (see
    /// `inline::date_widened`). `None` for other ids.
    pub fn date_widened(self, later: bool) -> Option<Self> {
        inline::date_widened(self, later)
    }

    /// The smallest and largest inline ids of `kind` (`Date` or `DateTime`) whose local
    /// year is in `low..=high` (0000-9999): the ids for which `YEAR` gives those years, as
    /// one range, since the year leads the payload. `None` for other kinds or years.
    pub fn year_range(kind: TermKind, low: u32, high: u32) -> Option<(Self, Self)> {
        inline::year_range(kind, low, high)
    }

    /// The smallest and largest ids of `kind`: the id range a scan restricted to one kind
    /// covers (for example all language-tagged strings of a predicate).
    pub const fn kind_range(kind: TermKind) -> (Self, Self) {
        (Self::new(kind, 0), Self::new(kind, PAYLOAD_MASK))
    }

    /// The timezone code of an inline date or dateTime (its lowest payload bits): two such
    /// ids of one kind with the same code compare by id as by value.
    pub fn date_timezone(self) -> Option<u64> {
        matches!(self.kind(), TermKind::Date | TermKind::DateTime)
            .then(|| inline::timezone_code(self.payload()))
    }

    /// Value of an inline boolean term, if this is one.
    pub fn as_inline_boolean(self) -> Option<bool> {
        (self.kind() == TermKind::Boolean).then(|| self.payload() != 0)
    }
}

pub(crate) use inline::{inline_to_literal, try_inline_literal};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_payload_roundtrip() {
        let id = TermId::new(TermKind::TypedLiteral, 42);
        assert_eq!(id.kind(), TermKind::TypedLiteral);
        assert_eq!(id.payload(), 42);
        assert_eq!(TermId::from_raw(id.raw()), id);
    }

    #[test]
    fn default_graph_sorts_first() {
        assert!(TermId::DEFAULT_GRAPH < TermId::new(TermKind::Iri, 0));
        assert!(TermId::DEFAULT_GRAPH.is_default_graph());
    }
}
