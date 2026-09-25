//! Term encoding: every RDF term is represented by a 64-bit [`TermId`].
//!
//! Layout: the top 4 bits are the [`TermKind`] tag, the low 60 bits the payload.
//! Dictionary-backed kinds (IRI, blank node, literal) carry an index into the
//! [`Dictionary`](dictionary::Dictionary); inline kinds carry the value itself.
//!
//! Inline encoding is only used when the lexical form is the canonical form of the value,
//! so RDF term identity is preserved (`"01"^^xsd:integer` and `"1"^^xsd:integer` are
//! different terms and get different ids). See ADR-0002.

pub(crate) mod dictionary;
mod inline;

pub use dictionary::{Dictionary, DictionaryStats};

/// Number of bits used by the payload of a [`TermId`].
pub const PAYLOAD_BITS: u32 = 60;
const PAYLOAD_MASK: u64 = (1 << PAYLOAD_BITS) - 1;

/// Kind tag stored in the top 4 bits of a [`TermId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum TermKind {
    /// Reserved: only the default graph marker uses this tag.
    DefaultGraph = 0,
    Iri = 1,
    BlankNode = 2,
    Literal = 3,
    /// Inline canonical `xsd:integer` (60-bit two's complement).
    Integer = 4,
    /// Inline canonical `xsd:boolean`.
    Boolean = 5,
}

impl TermKind {
    const fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::DefaultGraph,
            1 => Self::Iri,
            2 => Self::BlankNode,
            3 => Self::Literal,
            4 => Self::Integer,
            5 => Self::Boolean,
            _ => return None,
        })
    }

    /// True for kinds whose payload is a dictionary index.
    pub const fn is_dictionary(self) -> bool {
        matches!(self, Self::Iri | Self::BlankNode | Self::Literal)
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

    /// Value of an inline integer term, if this is one.
    pub fn as_inline_integer(self) -> Option<i64> {
        (self.kind() == TermKind::Integer).then(|| inline::decode_integer(self.payload()))
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
        let id = TermId::new(TermKind::Literal, 42);
        assert_eq!(id.kind(), TermKind::Literal);
        assert_eq!(id.payload(), 42);
        assert_eq!(TermId::from_raw(id.raw()), id);
    }

    #[test]
    fn default_graph_sorts_first() {
        assert!(TermId::DEFAULT_GRAPH < TermId::new(TermKind::Iri, 0));
        assert!(TermId::DEFAULT_GRAPH.is_default_graph());
    }
}
