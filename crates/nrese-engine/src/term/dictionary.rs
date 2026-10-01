//! Append-only term dictionary.
//!
//! All dictionary-backed terms are stored once in a single byte arena (`bytes` + `ends`)
//! and indexed by a SwissTable (`hashbrown::HashTable`) that stores only the arena index.
//! Memory per term is the key length plus 8 bytes (end offset) plus ~9 bytes of table
//! slot, with no per-term heap allocation.
//!
//! Concurrency: writers intern under an exclusive lock; readers look up and decode under a
//! shared lock. Bulk loads intern from many threads through
//! [`intern_quads`](Dictionary::intern_quads), which encodes, hashes and deduplicates a batch
//! before taking the lock, so the critical section is only table probes and arena appends. Ids are dense and assigned in insertion order, which is what the
//! write-ahead log relies on to replay dictionary growth deterministically.

use std::hash::BuildHasher;

use hashbrown::HashTable;
use nrese_rdf::{
    BaseDirection, BlankNode, GraphName, GraphNameRef, Literal, NamedNode, NamedOrBlankNode, Quad,
    QuadRef, Term, TermRef,
};
use parking_lot::RwLock;

use super::{TermId, TermKind, inline_to_literal, try_inline_literal};
use crate::error::{EngineError, EngineResult};
use crate::quad::EncodedQuad;

const TAG_IRI: u8 = b'I';
const TAG_BNODE: u8 = b'B';
const TAG_STRING: u8 = b'S';
const TAG_LANG: u8 = b'L';
const TAG_TYPED: u8 = b'T';
/// A language-tagged string with a base direction (RDF 1.2): tag, direction, value.
const TAG_DIR_LANG: u8 = b'D';
/// A triple term (RDF 1.2), keyed by its N-Triples text for now; the roadmap's R7 keys it
/// by its components' ids instead.
const TAG_TRIPLE: u8 = b'R';
const SEP: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DictionaryStats {
    pub terms: u64,
    pub arena_bytes: u64,
}

#[derive(Default)]
struct Inner {
    bytes: Vec<u8>,
    ends: Vec<u64>,
    table: HashTable<u64>,
}

impl Inner {
    fn key(&self, index: u64) -> &[u8] {
        let index = index as usize;
        let start = if index == 0 {
            0
        } else {
            self.ends[index - 1] as usize
        };
        &self.bytes[start..self.ends[index] as usize]
    }
}

pub struct Dictionary {
    inner: RwLock<Inner>,
    hasher: foldhash::fast::FixedState,
    /// Built at the first search, extended at later ones ([`super::text`]).
    text: RwLock<super::text::TextIndex>,
}

impl Default for Dictionary {
    fn default() -> Self {
        Self {
            inner: RwLock::default(),
            hasher: foldhash::fast::FixedState::with_seed(0x6e72_6573_655f_6474),
            text: RwLock::default(),
        }
    }
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary")
            .field("stats", &self.stats())
            .finish()
    }
}

impl Dictionary {
    /// Number of dictionary entries (inline terms are not counted).
    pub fn len(&self) -> u64 {
        self.inner.read().ends.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> DictionaryStats {
        let inner = self.inner.read();
        DictionaryStats {
            terms: inner.ends.len() as u64,
            arena_bytes: inner.bytes.len() as u64,
        }
    }

    /// Looks a term up without interning it. O(len(term)).
    pub fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.lookup_bounded(term, u64::MAX)
    }

    /// Like [`lookup`](Self::lookup) but treats entries with index `>= limit` as absent.
    /// Snapshots use this so that terms interned after the snapshot was taken are never
    /// observed, which keeps term identity stable for the lifetime of a query.
    pub fn lookup_bounded(&self, term: TermRef<'_>, limit: u64) -> Option<TermId> {
        if let Some(id) = inline_id(term) {
            return Some(id);
        }
        let mut key = Vec::with_capacity(64);
        let kind = encode_key(term, &mut key);
        let hash = self.hasher.hash_one(&key[..]);
        let inner = self.inner.read();
        inner
            .table
            .find(hash, |&index| inner.key(index) == &key[..])
            .filter(|&&index| index < limit)
            .map(|&index| TermId::new(kind, index))
    }

    /// Returns the id of `term`, adding it to the dictionary if needed. O(len(term)) amortised.
    pub fn intern(&self, term: TermRef<'_>) -> TermId {
        if let Some(id) = inline_id(term) {
            return id; // no lock needed
        }
        self.intern_locked(&mut self.inner.write(), &mut Vec::with_capacity(64), term)
    }

    /// Interns a batch of terms under one lock acquisition.
    pub fn intern_all<'a>(&self, terms: impl IntoIterator<Item = TermRef<'a>>) -> Vec<TermId> {
        let mut key = Vec::with_capacity(64);
        let mut inner = self.inner.write();
        terms
            .into_iter()
            .map(|term| self.intern_locked(&mut inner, &mut key, term))
            .collect()
    }

    /// The one interning path: inline value, or arena entry (reusing `key` as scratch).
    fn intern_locked(&self, inner: &mut Inner, key: &mut Vec<u8>, term: TermRef<'_>) -> TermId {
        if let Some(id) = inline_id(term) {
            return id;
        }
        key.clear();
        let kind = encode_key(term, key);
        TermId::new(kind, self.intern_key_locked(inner, key))
    }

    fn intern_key_locked(&self, inner: &mut Inner, key: &[u8]) -> u64 {
        self.intern_hashed_locked(inner, key, self.hasher.hash_one(key))
    }

    /// Interns `key` whose hash (with this dictionary's hasher) is `hash`.
    fn intern_hashed_locked(&self, inner: &mut Inner, key: &[u8], hash: u64) -> u64 {
        if let Some(&index) = inner.table.find(hash, |&index| inner.key(index) == key) {
            return index;
        }
        let index = inner.ends.len() as u64;
        inner.bytes.extend_from_slice(key);
        let end = inner.bytes.len() as u64;
        inner.ends.push(end);
        let Inner { table, bytes, ends } = inner;
        table.insert_unique(hash, index, |&i| {
            let i = i as usize;
            let start = if i == 0 { 0 } else { ends[i - 1] as usize };
            self.hasher.hash_one(&bytes[start..ends[i] as usize])
        });
        index
    }

    /// The string literals matching `query`, best first; the text index first takes in the
    /// terms interned since the last search.
    pub fn text_search(&self, query: &super::TextQuery) -> Vec<super::TextMatch> {
        if self.text.read().covered() < self.len() {
            let mut text = self.text.write();
            let inner = self.inner.read();
            let end = inner.ends.len() as u64;
            for index in text.covered()..end {
                match view_key(inner.key(index)) {
                    TermView::String(value) => {
                        text.add(TermId::new(TermKind::String, index).raw(), value);
                    }
                    TermView::LangString { value, .. } => {
                        text.add(TermId::new(TermKind::LangString, index).raw(), value);
                    }
                    _ => {}
                }
            }
            text.cover(end);
        }
        self.text.read().search(query)
    }

    /// Decodes an id back into a term. Returns `None` for ids that do not belong to this
    /// dictionary (or the default-graph marker).
    pub fn decode(&self, id: TermId) -> Option<Term> {
        match id.kind() {
            TermKind::Integer
            | TermKind::Boolean
            | TermKind::Decimal
            | TermKind::Date
            | TermKind::DateTime => inline_to_literal(id).map(Term::from),
            TermKind::DefaultGraph => None,
            TermKind::Iri
            | TermKind::BlankNode
            | TermKind::String
            | TermKind::LangString
            | TermKind::TypedLiteral
            | TermKind::Triple => {
                let inner = self.inner.read();
                if id.payload() >= inner.ends.len() as u64 {
                    return None;
                }
                Some(decode_key(inner.key(id.payload())))
            }
        }
    }

    /// Calls `f` with a borrowed view of the dictionary term `id`, without allocating: the
    /// view points into the arena under the read lock, so `f` must be short. `None` for
    /// inline ids (which carry their value, see [`TermId`]) and unknown ids.
    pub fn with_view<R>(&self, id: TermId, f: impl FnOnce(TermView<'_>) -> R) -> Option<R> {
        if !id.kind().is_dictionary() {
            return None;
        }
        let inner = self.inner.read();
        if id.payload() >= inner.ends.len() as u64 {
            return None;
        }
        Some(f(view_key(inner.key(id.payload()))))
    }

    /// Interns all four components of `quad`. Only the engine's writer calls this.
    pub(crate) fn intern_quad(&self, quad: QuadRef<'_>) -> EncodedQuad {
        let mut inner = self.inner.write();
        let mut key = Vec::with_capacity(64);
        let mut intern = |term| self.intern_locked(&mut inner, &mut key, term);
        let subject = intern(quad.subject.into());
        let predicate = intern(quad.predicate.into());
        let object = intern(quad.object);
        let graph = match graph_term(quad.graph_name) {
            Some(term) => intern(term),
            None => TermId::DEFAULT_GRAPH,
        };
        EncodedQuad::new(subject, predicate, object, graph)
    }

    /// Interns every term of a batch of quads. Safe to call from many threads at once: the
    /// keys are encoded, hashed and deduplicated per batch without the lock, which is then
    /// held once, for the distinct keys only. Ids depend on the interleaving of concurrent
    /// batches, which is fine: they are logged by key, never recomputed.
    pub(crate) fn intern_quads(&self, quads: &[Quad]) -> Vec<EncodedQuad> {
        let mut batch = KeyBatch::with_capacity(quads.len());
        let slots: Vec<[Slot; 4]> = quads
            .iter()
            .map(|quad| {
                let quad = quad.as_ref();
                let graph = match graph_term(quad.graph_name) {
                    Some(term) => batch.slot(self, term),
                    None => Slot::Id(TermId::DEFAULT_GRAPH),
                };
                [
                    batch.slot(self, quad.subject.into()),
                    batch.slot(self, quad.predicate.into()),
                    batch.slot(self, quad.object),
                    graph,
                ]
            })
            .collect();
        let indexes: Vec<u64> = {
            let mut inner = self.inner.write();
            batch
                .keys
                .iter()
                .map(|key| {
                    let bytes = &batch.arena[key.start..key.end];
                    self.intern_hashed_locked(&mut inner, bytes, key.hash)
                })
                .collect()
        };
        let resolve = |slot: Slot| match slot {
            Slot::Id(id) => id,
            Slot::Key(key) => {
                let key = key as usize;
                TermId::new(batch.keys[key].kind, indexes[key])
            }
        };
        slots
            .into_iter()
            .map(|[s, p, o, g]| EncodedQuad::new(resolve(s), resolve(p), resolve(o), resolve(g)))
            .collect()
    }

    /// Encodes `quad` without interning; `None` if any term is unknown (below `limit`).
    pub(crate) fn lookup_quad_bounded(&self, quad: QuadRef<'_>, limit: u64) -> Option<EncodedQuad> {
        let graph = match graph_term(quad.graph_name) {
            Some(term) => self.lookup_bounded(term, limit)?,
            None => TermId::DEFAULT_GRAPH,
        };
        Some(EncodedQuad::new(
            self.lookup_bounded(quad.subject.into(), limit)?,
            self.lookup_bounded(quad.predicate.into(), limit)?,
            self.lookup_bounded(quad.object, limit)?,
            graph,
        ))
    }

    /// Decodes an encoded quad; `None` if an id is unknown or in an impossible position.
    pub fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        let graph_name = if quad.graph.is_default_graph() {
            GraphName::DefaultGraph
        } else {
            NamedOrBlankNode::try_from(self.decode(quad.graph)?)
                .ok()?
                .into()
        };
        let Term::NamedNode(predicate) = self.decode(quad.predicate)? else {
            return None;
        };
        Some(Quad::new(
            NamedOrBlankNode::try_from(self.decode(quad.subject)?).ok()?,
            predicate,
            self.decode(quad.object)?,
            graph_name,
        ))
    }

    /// Raw key bytes of entries `[from, to)`, in id order. Used by the WAL and checkpoints.
    pub(crate) fn export_keys(&self, from: u64, to: u64) -> Vec<Vec<u8>> {
        let inner = self.inner.read();
        (from..to.min(inner.ends.len() as u64))
            .map(|index| inner.key(index).to_vec())
            .collect()
    }

    /// Restores a checkpoint's dictionary into this empty one: `keys` in id order. Keys are
    /// validated and hashed in parallel, appended to the arena at once and indexed in a
    /// table sized up front; a duplicate key is corruption.
    pub(crate) fn restore_keys(&self, keys: &[&[u8]]) -> EngineResult<()> {
        use rayon::prelude::*;
        let hashes: Vec<u64> = keys
            .par_iter()
            .map(|key| validate_key(key).map(|()| self.hasher.hash_one(key)))
            .collect::<EngineResult<_>>()?;
        let mut inner = self.inner.write();
        if !inner.ends.is_empty() {
            return Err(EngineError::Corruption(
                "checkpoint dictionary restored into a non-empty dictionary".to_owned(),
            ));
        }
        let Inner { table, bytes, ends } = &mut *inner;
        bytes.reserve_exact(keys.iter().map(|key| key.len()).sum());
        ends.reserve_exact(keys.len());
        for key in keys {
            bytes.extend_from_slice(key);
            ends.push(bytes.len() as u64);
        }
        let key = |i: u64| {
            let i = i as usize;
            let start = if i == 0 { 0 } else { ends[i - 1] as usize };
            &bytes[start..ends[i] as usize]
        };
        *table = HashTable::with_capacity(keys.len());
        for (index, &hash) in hashes.iter().enumerate() {
            let index = index as u64;
            if table
                .find(hash, |&other| key(other) == key(index))
                .is_some()
            {
                return Err(EngineError::Corruption(format!(
                    "duplicate dictionary key at {index}"
                )));
            }
            table.insert_unique(hash, index, |&i| self.hasher.hash_one(key(i)));
        }
        Ok(())
    }

    /// Re-adds a raw key during recovery. The key must land exactly at `expected_index`.
    pub(crate) fn restore_key(&self, expected_index: u64, key: &[u8]) -> EngineResult<()> {
        validate_key(key)?;
        let mut inner = self.inner.write();
        let len = inner.ends.len() as u64;
        if expected_index < len {
            return if inner.key(expected_index) == key {
                Ok(())
            } else {
                Err(EngineError::Corruption(format!(
                    "dictionary entry {expected_index} differs from the logged key"
                )))
            };
        }
        if expected_index != len {
            return Err(EngineError::Corruption(format!(
                "dictionary gap: expected entry {expected_index}, dictionary has {len}"
            )));
        }
        let index = self.intern_key_locked(&mut inner, key);
        if index != expected_index {
            return Err(EngineError::Corruption(format!(
                "duplicate dictionary key logged at {expected_index} (already {index})"
            )));
        }
        Ok(())
    }
}

/// The graph name as a term; `None` for the default graph.
/// A term position in a batch being interned: an id known up front (inline values, the
/// default graph) or an index into the batch's distinct keys.
#[derive(Clone, Copy)]
enum Slot {
    Id(TermId),
    Key(u32),
}

struct PreparedKey {
    start: usize,
    end: usize,
    hash: u64,
    kind: TermKind,
}

/// The distinct keys of one batch, encoded into one arena, with their hashes.
struct KeyBatch {
    arena: Vec<u8>,
    keys: Vec<PreparedKey>,
    /// Indexes into `keys`, for deduplication within the batch.
    table: HashTable<u32>,
}

impl KeyBatch {
    fn with_capacity(quads: usize) -> Self {
        Self {
            arena: Vec::with_capacity(quads * 64),
            keys: Vec::with_capacity(quads),
            table: HashTable::with_capacity(quads),
        }
    }

    fn slot(&mut self, dictionary: &Dictionary, term: TermRef<'_>) -> Slot {
        if let Some(id) = inline_id(term) {
            return Slot::Id(id);
        }
        let start = self.arena.len();
        let kind = encode_key(term, &mut self.arena);
        let hash = dictionary.hasher.hash_one(&self.arena[start..]);
        let Self { arena, keys, table } = self;
        let key = &arena[start..];
        if let Some(&index) = table.find(hash, |&i| {
            let other = &keys[i as usize];
            &arena[other.start..other.end] == key
        }) {
            arena.truncate(start);
            return Slot::Key(index);
        }
        let index = u32::try_from(keys.len()).expect("batch has fewer than 2^32 distinct terms");
        keys.push(PreparedKey {
            start,
            end: arena.len(),
            hash,
            kind,
        });
        table.insert_unique(hash, index, |&i| keys[i as usize].hash);
        Slot::Key(index)
    }
}

fn graph_term(graph: GraphNameRef<'_>) -> Option<TermRef<'_>> {
    match graph {
        GraphNameRef::NamedNode(node) => Some(node.into()),
        GraphNameRef::BlankNode(node) => Some(node.into()),
        GraphNameRef::DefaultGraph => None,
    }
}

fn inline_id(term: TermRef<'_>) -> Option<TermId> {
    match term {
        TermRef::Literal(literal) => try_inline_literal(literal),
        _ => None,
    }
}

fn encode_key(term: TermRef<'_>, out: &mut Vec<u8>) -> TermKind {
    match term {
        TermRef::NamedNode(node) => {
            out.push(TAG_IRI);
            out.extend_from_slice(node.as_str().as_bytes());
            TermKind::Iri
        }
        TermRef::BlankNode(node) => {
            out.push(TAG_BNODE);
            out.extend_from_slice(node.as_str().as_bytes());
            TermKind::BlankNode
        }
        TermRef::Literal(literal) => {
            let kind = if let Some(language) = literal.language() {
                match literal.direction() {
                    Some(direction) => {
                        out.push(TAG_DIR_LANG);
                        out.extend_from_slice(language.as_bytes());
                        out.push(SEP);
                        out.extend_from_slice(direction.as_str().as_bytes());
                    }
                    None => {
                        out.push(TAG_LANG);
                        out.extend_from_slice(language.as_bytes());
                    }
                }
                out.push(SEP);
                TermKind::LangString
            } else if literal.datatype() == nrese_rdf::vocab::xsd::STRING {
                out.push(TAG_STRING);
                TermKind::String
            } else {
                out.push(TAG_TYPED);
                out.extend_from_slice(literal.datatype().as_str().as_bytes());
                out.push(SEP);
                TermKind::TypedLiteral
            };
            out.extend_from_slice(literal.value().as_bytes());
            kind
        }
        TermRef::Triple(_) => {
            out.push(TAG_TRIPLE);
            out.extend_from_slice(term.to_string().as_bytes());
            TermKind::Triple
        }
    }
}

fn split_sep(rest: &[u8]) -> (&str, &str) {
    let pos = rest.iter().position(|&b| b == SEP).unwrap_or(rest.len());
    let head = std::str::from_utf8(&rest[..pos]).unwrap_or_default();
    let tail = std::str::from_utf8(rest.get(pos + 1..).unwrap_or_default()).unwrap_or_default();
    (head, tail)
}

/// Keys are only produced by [`encode_key`] or checked by [`validate_key`], so the
/// unchecked constructors are sound here.
/// A dictionary term's text, borrowed from the arena (see [`Dictionary::with_view`]).
/// Executors read strings, language tags and datatypes through it without building terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermView<'a> {
    Iri(&'a str),
    BlankNode(&'a str),
    /// A simple literal (`xsd:string`).
    String(&'a str),
    LangString {
        value: &'a str,
        language: &'a str,
        /// The base direction (RDF 1.2), if the string has one.
        direction: Option<BaseDirection>,
    },
    Typed {
        value: &'a str,
        datatype: &'a str,
    },
    /// A triple term (RDF 1.2), as N-Triples text: `<<( s p o )>>`.
    Triple(&'a str),
}

impl<'a> TermView<'a> {
    /// The lexical form of a literal, or the IRI string: what SPARQL `STR` returns (`None`
    /// for blank nodes).
    pub fn str(self) -> Option<&'a str> {
        match self {
            Self::Iri(s) | Self::String(s) => Some(s),
            Self::LangString { value, .. } | Self::Typed { value, .. } => Some(value),
            Self::BlankNode(_) | Self::Triple(_) => None,
        }
    }
}

fn view_key(key: &[u8]) -> TermView<'_> {
    let rest = &key[1..];
    let text = || std::str::from_utf8(rest).unwrap_or_default();
    match key[0] {
        TAG_IRI => TermView::Iri(text()),
        TAG_BNODE => TermView::BlankNode(text()),
        TAG_STRING => TermView::String(text()),
        TAG_LANG => {
            let (language, value) = split_sep(rest);
            TermView::LangString {
                value,
                language,
                direction: None,
            }
        }
        TAG_DIR_LANG => {
            let (language, rest) = split_sep(rest);
            let (direction, value) = split_sep(rest.as_bytes());
            TermView::LangString {
                value,
                language,
                direction: direction.parse().ok(),
            }
        }
        TAG_TRIPLE => TermView::Triple(text()),
        _ => {
            let (datatype, value) = split_sep(rest);
            TermView::Typed { value, datatype }
        }
    }
}

fn decode_key(key: &[u8]) -> Term {
    let rest = &key[1..];
    let text = || std::str::from_utf8(rest).unwrap_or_default().to_owned();
    match key[0] {
        TAG_IRI => NamedNode::new_unchecked(text()).into(),
        TAG_BNODE => BlankNode::new_unchecked(text()).into(),
        TAG_STRING => Literal::new_simple_literal(text()).into(),
        TAG_LANG => {
            let (language, lexical) = split_sep(rest);
            Literal::new_language_tagged_literal_unchecked(lexical, language).into()
        }
        TAG_DIR_LANG => {
            let (language, rest) = split_sep(rest);
            let (direction, lexical) = split_sep(rest.as_bytes());
            let direction = direction.parse().unwrap_or(BaseDirection::Ltr);
            Literal::new_directional_language_tagged_literal_unchecked(lexical, language, direction)
                .into()
        }
        TAG_TRIPLE => parse_triple_term(&text()),
        _ => {
            let (datatype, lexical) = split_sep(rest);
            Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)).into()
        }
    }
}

/// A triple term from the N-Triples text its key holds (written by [`encode_key`]).
fn parse_triple_term(text: &str) -> Term {
    let line = format!("<urn:x> <urn:x> {text} .");
    nrese_rdf_io::RdfParser::from_format(nrese_rdf_io::RdfFormat::NTriples)
        .for_slice(line.as_bytes())
        .next()
        .and_then(Result::ok)
        .map_or_else(
            || Literal::new_simple_literal(text).into(),
            |quad| quad.object,
        )
}

fn validate_key(key: &[u8]) -> EngineResult<()> {
    let ok = !key.is_empty()
        && matches!(
            key[0],
            TAG_IRI | TAG_BNODE | TAG_STRING | TAG_LANG | TAG_TYPED | TAG_DIR_LANG | TAG_TRIPLE
        )
        && std::str::from_utf8(&key[1..]).is_ok();
    if ok {
        Ok(())
    } else {
        Err(EngineError::Corruption(
            "malformed dictionary key".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use nrese_rdf::vocab::xsd;
    use nrese_rdf::{BlankNodeRef, LiteralRef, NamedNodeRef};

    use super::*;

    #[test]
    fn bulk_restore_matches_interning() {
        let source = Dictionary::default();
        let terms: Vec<NamedNode> = (0..5000)
            .map(|i| NamedNode::new_unchecked(format!("http://example.com/{i}")))
            .collect();
        let ids: Vec<TermId> = terms
            .iter()
            .map(|t| source.intern(t.as_ref().into()))
            .collect();
        let keys = source.export_keys(0, source.len());
        let slices: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
        let restored = Dictionary::default();
        restored.restore_keys(&slices).unwrap();
        for (term, id) in terms.iter().zip(&ids) {
            assert_eq!(restored.lookup(term.as_ref().into()), Some(*id));
            assert_eq!(restored.decode(*id), Some(term.clone().into()));
        }
        let mut duplicated = slices.clone();
        duplicated.push(slices[42]);
        assert!(Dictionary::default().restore_keys(&duplicated).is_err());
        assert!(
            restored.restore_keys(&slices).is_err(),
            "only into an empty dictionary"
        );
    }

    fn roundtrip(term: TermRef<'_>) {
        let dict = Dictionary::default();
        let id = dict.intern(term);
        assert_eq!(dict.intern(term), id, "interning is idempotent");
        assert_eq!(dict.lookup(term), Some(id));
        assert_eq!(dict.decode(id).as_ref().map(Term::as_ref), Some(term));
    }

    #[test]
    fn all_term_shapes_roundtrip() {
        roundtrip(NamedNodeRef::new_unchecked("http://example.com/a").into());
        roundtrip(BlankNodeRef::new_unchecked("b0").into());
        roundtrip(LiteralRef::new_simple_literal("plain").into());
        roundtrip(LiteralRef::new_language_tagged_literal_unchecked("Haus", "de").into());
        roundtrip(LiteralRef::new_typed_literal("1450-01-01", xsd::DATE).into());
        roundtrip(LiteralRef::new_typed_literal("01", xsd::INTEGER).into());
        roundtrip(LiteralRef::new_typed_literal("7", xsd::INTEGER).into());
    }

    #[test]
    fn rdf_1_2_terms_round_trip() {
        let dict = Dictionary::default();
        let directional = Term::from(Literal::new_directional_language_tagged_literal_unchecked(
            "x",
            "en",
            BaseDirection::Ltr,
        ));
        let plain = Term::from(Literal::new_language_tagged_literal_unchecked("x", "en"));
        let triple: Term = nrese_rdf::Triple::new(
            BlankNode::new_unchecked("b"),
            NamedNode::new_unchecked("http://e/p"),
            nrese_rdf::Triple::new(
                NamedNode::new_unchecked("http://e/s"),
                NamedNode::new_unchecked("http://e/q"),
                directional.clone(),
            ),
        )
        .into();
        let ids: Vec<TermId> = [&directional, &plain, &triple]
            .into_iter()
            .map(|t| dict.intern(t.as_ref()))
            .collect();
        // The direction makes a term of its own.
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[2].kind(), TermKind::Triple);
        for (id, term) in ids.iter().zip([&directional, &plain, &triple]) {
            assert_eq!(dict.decode(*id).as_ref(), Some(term));
        }
        assert_eq!(
            dict.with_view(ids[2], |v| format!("{v:?}")).unwrap(),
            "Triple(\"<<( _:b <http://e/p> <<( <http://e/s> <http://e/q> \\\"x\\\"@en--ltr )>> )>>\")"
        );
    }

    #[test]
    fn views_expose_text_kind_language_and_datatype() {
        let dict = Dictionary::default();
        let view = |term: TermRef<'_>| {
            let id = dict.intern(term);
            dict.with_view(id, |v| format!("{v:?}"))
        };
        assert_eq!(
            view(NamedNodeRef::new_unchecked("http://e/a").into()).unwrap(),
            "Iri(\"http://e/a\")"
        );
        assert_eq!(
            view(BlankNodeRef::new_unchecked("b0").into()).unwrap(),
            "BlankNode(\"b0\")"
        );
        assert_eq!(
            view(LiteralRef::new_simple_literal("plain").into()).unwrap(),
            "String(\"plain\")"
        );
        assert_eq!(
            view(LiteralRef::new_language_tagged_literal_unchecked("Haus", "de").into()).unwrap(),
            "LangString { value: \"Haus\", language: \"de\", direction: None }"
        );
        assert_eq!(
            view(
                LiteralRef::new_directional_language_tagged_literal_unchecked(
                    "Haus",
                    "de",
                    nrese_rdf::BaseDirection::Rtl
                )
                .into()
            )
            .unwrap(),
            "LangString { value: \"Haus\", language: \"de\", direction: Some(Rtl) }"
        );
        assert_eq!(
            view(LiteralRef::new_typed_literal("01", xsd::INTEGER).into()).unwrap(),
            "Typed { value: \"01\", datatype: \"http://www.w3.org/2001/XMLSchema#integer\" }"
        );
        // Inline ids carry their value; there is no dictionary text to view.
        assert_eq!(
            view(LiteralRef::new_typed_literal("7", xsd::INTEGER).into()),
            None
        );
        let id =
            dict.intern(LiteralRef::new_language_tagged_literal_unchecked("Haus", "de").into());
        assert_eq!(
            dict.with_view(id, |v| v.str().map(str::to_owned))
                .flatten()
                .as_deref(),
            Some("Haus")
        );
    }

    #[test]
    fn distinct_shapes_with_same_text_get_distinct_ids() {
        let dict = Dictionary::default();
        let ids = dict.intern_all([
            NamedNodeRef::new_unchecked("x:a").into(),
            LiteralRef::new_simple_literal("x:a").into(),
            BlankNodeRef::new_unchecked("a").into(),
            LiteralRef::new_typed_literal("01", xsd::INTEGER).into(),
            LiteralRef::new_typed_literal("1", xsd::INTEGER).into(),
        ]);
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn bounded_lookup_hides_newer_entries() {
        let dict = Dictionary::default();
        let limit = dict.len();
        let term: TermRef<'_> = NamedNodeRef::new_unchecked("http://example.com/late").into();
        dict.intern(term);
        assert!(dict.lookup_bounded(term, limit).is_none());
        assert!(dict.lookup(term).is_some());
    }

    #[test]
    fn restore_keys_replays_in_order_and_detects_divergence() {
        let source = Dictionary::default();
        source.intern(NamedNodeRef::new_unchecked("http://example.com/a").into());
        source.intern(LiteralRef::new_simple_literal("b").into());
        let keys = source.export_keys(0, source.len());

        let target = Dictionary::default();
        for (index, key) in keys.iter().enumerate() {
            target.restore_key(index as u64, key).unwrap();
        }
        target.restore_key(0, &keys[0]).unwrap(); // idempotent overlap
        assert!(target.restore_key(0, &keys[1]).is_err());
        assert!(target.restore_key(5, &keys[0]).is_err());
        assert_eq!(target.len(), 2);
    }
}
