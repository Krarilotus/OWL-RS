//! Append-only term dictionary.
//!
//! All dictionary-backed terms are stored once in a byte arena (`bytes` + `ends`) and
//! indexed by a SwissTable (`hashbrown::HashTable`) that stores only the arena index.
//! Memory per term is the key length plus 8 bytes (end offset) plus ~9 bytes of table
//! slot, with no per-term heap allocation.
//!
//! A dictionary restored from a checkpoint keeps the checkpoint's entries where they are:
//! the [`Base`], the arena, end offsets and an open-addressing hash table of a mapped file
//! ([`crate::mapped`]). Only the entries interned since are on the heap. Lookups probe the
//! base's table, then the heap's. Both hash keys with the fixed [`key_hash`], which the
//! checkpoint's table was built with.
//!
//! Concurrency: writers intern under an exclusive lock; readers look up and decode under a
//! shared lock. Bulk loads intern from many threads through
//! [`intern_quads`](Dictionary::intern_quads), which encodes, hashes and deduplicates a batch
//! before taking the lock, so the critical section is only table probes and arena appends. Ids are dense and assigned in insertion order, which is what the
//! write-ahead log relies on to replay dictionary growth deterministically.

use hashbrown::HashTable;
use nrese_rdf::{
    BaseDirection, BlankNode, GraphName, GraphNameRef, Literal, NamedNode, NamedOrBlankNode, Quad,
    QuadRef, Term, TermRef,
};
use parking_lot::RwLock;

use super::hash::key_hash;
use super::{TermId, TermKind, inline_to_literal, try_inline_literal};
use crate::error::{EngineError, EngineResult};
use crate::mapped::Mapped;
use crate::quad::EncodedQuad;

const TAG_IRI: u8 = b'I';
const TAG_BNODE: u8 = b'B';
const TAG_STRING: u8 = b'S';
const TAG_LANG: u8 = b'L';
const TAG_TYPED: u8 = b'T';
/// A language-tagged string with a base direction (RDF 1.2): tag, direction, value.
const TAG_DIR_LANG: u8 = b'D';
/// A triple term (RDF 1.2) keyed by its N-Triples text: written by the first RDF 1.2
/// version (step 5 of the migration), still read.
const TAG_TRIPLE: u8 = b'R';
/// A triple term (RDF 1.2) keyed by its components' ids (roadmap R7): the tag, then the
/// subject, predicate and object ids, 8 bytes each, big-endian. Fixed 25 bytes whatever the
/// terms' lengths; equal triple terms share an id however they are written. The components
/// are interned first, so their ids are lower: the log and checkpoints replay keys in id
/// order and find them.
const TAG_TRIPLE_IDS: u8 = b'Q';
const TRIPLE_KEY_LEN: usize = 25;
const SEP: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DictionaryStats {
    pub terms: u64,
    /// The terms' keys, one after another (mapped and on the heap).
    pub arena_bytes: u64,
    /// What finds them on the heap: the end offsets and the hash table, as allocated.
    pub index_bytes: u64,
    /// Bytes used in place from a mapped checkpoint: keys, end offsets and hash table.
    pub mapped_bytes: u64,
}

/// Dictionary entries `0..len` in a mapped checkpoint: their keys one after another, the
/// end offsets, and a hash table of them.
pub(crate) struct Base {
    pub(crate) len: u64,
    pub(crate) arena: Mapped<u8>,
    pub(crate) ends: Mapped<u64>,
    /// Open addressing with linear probing by [`key_hash`]: entry index + 1, or 0 for an
    /// empty slot; a power of two long, with at least one empty slot.
    pub(crate) slots: Mapped<u32>,
    /// The entries with a text, sorted by it ([`super::order`]); `None` in checkpoints
    /// written before format 7.
    pub(crate) order: Option<Mapped<u32>>,
}

impl Base {
    /// Entry `index`'s key; empty where a damaged file's offsets point nowhere.
    fn key(&self, index: u64) -> &[u8] {
        let i = index as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        self.arena
            .get(start..self.ends[i] as usize)
            .unwrap_or_default()
    }

    fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        let mask = self.slots.len() - 1;
        let mut slot = hash as usize & mask;
        // At most one round (a damaged, full table must not loop forever).
        for _ in 0..self.slots.len() {
            let entry = u64::from(self.slots[slot]);
            if entry == 0 {
                return None;
            }
            if entry <= self.len && self.key(entry - 1) == key {
                return Some(entry - 1);
            }
            slot = (slot + 1) & mask;
        }
        None
    }

    fn bytes(&self) -> u64 {
        let order = self.order.as_ref().map_or(0, |order| order.len() * 4);
        (self.arena.len() + self.ends.len() * 8 + self.slots.len() * 4 + order) as u64
    }

    /// Checks every key and that the hash table finds each entry: reads all of the base.
    pub(crate) fn verify(&self) -> Result<(), String> {
        if !self.ends.is_sorted() {
            return Err("dictionary end offsets are out of order".into());
        }
        for index in 0..self.len {
            let key = self.key(index);
            validate_key(key).map_err(|error| format!("dictionary entry {index}: {error}"))?;
            if self.find(key_hash(key), key) != Some(index) {
                return Err(format!("the dictionary's hash table misses entry {index}"));
            }
        }
        if let Some(order) = &self.order {
            let text = |index: u32| {
                (u64::from(index) < self.len)
                    .then(|| super::order::text_of(self.key(u64::from(index))))
                    .flatten()
            };
            for pair in order.windows(2) {
                match (text(pair[0]), text(pair[1])) {
                    (Some(a), Some(b)) if (a, pair[0]) < (b, pair[1]) => {}
                    _ => return Err("the dictionary's text order is out of order".into()),
                }
            }
        }
        Ok(())
    }
}

/// The number of slots of a base's hash table for `len` entries: a power of two, at most
/// three quarters full.
pub(crate) fn base_slots(len: u64) -> u64 {
    (len + len / 3 + 1).next_power_of_two()
}

#[derive(Default)]
struct Inner {
    /// Entries `0..base.len` from a mapped checkpoint.
    base: Option<Base>,
    /// The keys of the entries after the base, one after another.
    bytes: Vec<u8>,
    /// Their end offsets in `bytes`.
    ends: Vec<u64>,
    /// Their indexes, by [`key_hash`].
    table: HashTable<u64>,
}

impl Inner {
    fn base_len(&self) -> u64 {
        self.base.as_ref().map_or(0, |base| base.len)
    }

    fn len(&self) -> u64 {
        self.base_len() + self.ends.len() as u64
    }

    fn key(&self, index: u64) -> &[u8] {
        let base_len = self.base_len();
        if index < base_len {
            return self
                .base
                .as_ref()
                .expect("an index below the base")
                .key(index);
        }
        heap_key(&self.bytes, &self.ends, (index - base_len) as usize)
    }

    /// The index of `key`, whose hash is `hash`, if it is an entry.
    fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        if let Some(index) = self.base.as_ref().and_then(|base| base.find(hash, key)) {
            return Some(index);
        }
        self.table
            .find(hash, |&index| self.key(index) == key)
            .copied()
    }

    /// Appends `key` (not an entry yet), whose hash is `hash`, and returns its index.
    fn push(&mut self, key: &[u8], hash: u64) -> u64 {
        let index = self.len();
        let base_len = self.base_len();
        self.bytes.extend_from_slice(key);
        self.ends.push(self.bytes.len() as u64);
        let Inner {
            table, bytes, ends, ..
        } = self;
        table.insert_unique(hash, index, |&i| {
            key_hash(heap_key(bytes, ends, (i - base_len) as usize))
        });
        index
    }
}

/// Heap entry `local` (counted from the end of the base).
fn heap_key<'a>(bytes: &'a [u8], ends: &[u64], local: usize) -> &'a [u8] {
    let start = if local == 0 {
        0
    } else {
        ends[local - 1] as usize
    };
    &bytes[start..ends[local] as usize]
}

/// The triple terms of the dictionary by their components, built lazily from the arena (the
/// dictionary only grows): what matching `<<( ?s :p ?o )>>` scans.
#[derive(Default)]
struct TripleIndex {
    /// Dictionary entries up to here are in `rows`.
    covered: u64,
    /// `[subject, predicate, object, triple term]` ids.
    rows: Vec<[TermId; 4]>,
}

#[derive(Default)]
pub struct Dictionary {
    inner: RwLock<Inner>,
    /// Built at the first search, extended at later ones ([`super::text`]).
    text: RwLock<super::text::TextIndex>,
    /// Built at the first triple-term match, extended at later ones.
    triples: RwLock<TripleIndex>,
    /// The text order of the entries the mapped base doesn't order ([`super::order`]).
    order: RwLock<super::order::TextOrder>,
    /// Integer-derived literals (`xsd:int`, ...) are dictionary entries, not inline
    /// ([`TermKind::DerivedInteger`]): stores created before that kind keep them so.
    integers_in_dictionary: std::sync::atomic::AtomicBool,
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
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether integer-derived literals are dictionary entries (a store created before
    /// [`TermKind::DerivedInteger`]); else they are inline.
    pub fn integers_in_dictionary(&self) -> bool {
        self.integers_in_dictionary
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Sets how integer-derived literals are encoded: when a store is opened, before any
    /// term is interned or looked up.
    pub(crate) fn set_integers_in_dictionary(&self, yes: bool) {
        self.integers_in_dictionary
            .store(yes, std::sync::atomic::Ordering::Relaxed);
    }

    /// The inline id of `term`, if it has one in this dictionary's encoding.
    fn inline_id(&self, term: TermRef<'_>) -> Option<TermId> {
        match term {
            TermRef::Literal(literal) => {
                try_inline_literal(literal, !self.integers_in_dictionary())
            }
            _ => None,
        }
    }

    pub fn stats(&self) -> DictionaryStats {
        let inner = self.inner.read();
        let base = inner.base.as_ref();
        DictionaryStats {
            terms: inner.len(),
            arena_bytes: inner.bytes.len() as u64 + base.map_or(0, |b| b.arena.len() as u64),
            // A slot is the stored index plus one control byte.
            index_bytes: (inner.ends.capacity() * 8 + inner.table.capacity() * 9) as u64,
            mapped_bytes: base.map_or(0, Base::bytes),
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
        if let Some(id) = self.inline_id(term) {
            return Some(id);
        }
        let mut key = Vec::with_capacity(64);
        let kind = match term {
            TermRef::Triple(triple) => {
                let ids = [
                    self.lookup_bounded(triple.subject.as_ref().into(), limit)?,
                    self.lookup_bounded(triple.predicate.as_ref().into(), limit)?,
                    self.lookup_bounded(triple.object.as_ref(), limit)?,
                ];
                triple_key(ids, &mut key);
                TermKind::Triple
            }
            _ => encode_key(term, &mut key),
        };
        let hash = key_hash(&key);
        self.inner
            .read()
            .find(hash, &key)
            .filter(|&index| index < limit)
            .map(|index| TermId::new(kind, index))
    }

    /// Returns the id of `term`, adding it to the dictionary if needed. O(len(term)) amortised.
    pub fn intern(&self, term: TermRef<'_>) -> TermId {
        if let Some(id) = self.inline_id(term) {
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
        if let Some(id) = self.inline_id(term) {
            return id;
        }
        if let TermRef::Triple(triple) = term {
            // The components first: their ids are part of the key, and lower than its own.
            let ids = [
                self.intern_locked(inner, key, triple.subject.as_ref().into()),
                self.intern_locked(inner, key, triple.predicate.as_ref().into()),
                self.intern_locked(inner, key, triple.object.as_ref()),
            ];
            key.clear();
            triple_key(ids, key);
            return TermId::new(TermKind::Triple, self.intern_key_locked(inner, key));
        }
        key.clear();
        let kind = encode_key(term, key);
        TermId::new(kind, self.intern_key_locked(inner, key))
    }

    /// The triple terms matching the given components (`None`: any) among the entries below
    /// `limit` (a snapshot's dictionary length): `[subject, predicate, object, triple term]`.
    pub fn triple_terms(
        &self,
        subject: Option<TermId>,
        predicate: Option<TermId>,
        object: Option<TermId>,
        limit: u64,
    ) -> Vec<[TermId; 4]> {
        let len = self.len();
        if self.triples.read().covered < len {
            let mut index = self.triples.write();
            let inner = self.inner.read();
            let end = inner.len();
            for entry in index.covered..end {
                if let Some(ids) = triple_ids(inner.key(entry)) {
                    let [s, p, o] = ids;
                    index
                        .rows
                        .push([s, p, o, TermId::new(TermKind::Triple, entry)]);
                }
            }
            index.covered = end;
        }
        let matches = |want: Option<TermId>, have: TermId| want.is_none_or(|w| w == have);
        self.triples
            .read()
            .rows
            .iter()
            .filter(|[s, p, o, id]| {
                id.payload() < limit
                    && matches(subject, *s)
                    && matches(predicate, *p)
                    && matches(object, *o)
            })
            .copied()
            .collect()
    }

    fn intern_key_locked(&self, inner: &mut Inner, key: &[u8]) -> u64 {
        self.intern_hashed_locked(inner, key, key_hash(key))
    }

    /// Interns `key` whose hash ([`key_hash`]) is `hash`.
    fn intern_hashed_locked(&self, inner: &mut Inner, key: &[u8], hash: u64) -> u64 {
        match inner.find(hash, key) {
            Some(index) => index,
            None => inner.push(key, hash),
        }
    }

    /// The string literals matching `query`, best first; the text index first takes in the
    /// terms interned since the last search.
    pub fn text_search(&self, query: &super::TextQuery) -> Vec<super::TextMatch> {
        if self.text.read().covered() < self.len() {
            let mut text = self.text.write();
            let inner = self.inner.read();
            let end = inner.len();
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
        let inner = self.inner.read();
        let text_of = |id: u64| match view_key(inner.key(TermId::from_raw(id).payload())) {
            TermView::String(value) | TermView::LangString { value, .. } => Some(value.to_owned()),
            _ => None,
        };
        self.text.read().search(query, &text_of)
    }

    /// The ids of the entries `0..limit` whose text passes `test`, sorted: one parallel
    /// substring search over the arena ([`super::strings`]).
    pub fn matching_strings(&self, test: &super::StringTest<'_>, limit: u64) -> Vec<TermId> {
        // A prefix: a range of the text order, where it is at hand.
        if matches!(
            test.placement,
            super::Placement::Start | super::Placement::Whole
        ) && !test.needle.is_empty()
            && self.text_order_ready()
        {
            return self.prefix_matches(test, limit);
        }
        let inner = self.inner.read();
        let base_len = inner.base_len();
        let mut ids = match &inner.base {
            Some(base) => super::strings::matching(&base.arena, &base.ends, 0, limit, test),
            None => Vec::new(),
        };
        if limit > base_len {
            ids.extend(super::strings::matching(
                &inner.bytes,
                &inner.ends,
                base_len,
                limit - base_len,
                test,
            ));
        }
        ids.sort_unstable();
        ids
    }

    /// Whether a prefix search is a few binary searches: the entries are in text order
    /// (the mapped base's, and the others' in memory), or few enough to sort on the way.
    pub fn text_order_ready(&self) -> bool {
        const SORTED_ON_THE_WAY: u64 = 1 << 20;
        let inner = self.inner.read();
        let first = match &inner.base {
            Some(base) if base.order.is_some() => base.len,
            Some(_) => 0,
            None => 0,
        };
        let order = self.order.read();
        let covered = if order.first == first {
            order.covered
        } else {
            first
        };
        inner.len().saturating_sub(covered) <= SORTED_ON_THE_WAY
    }

    /// The entries below `limit` that pass `test`, a prefix test, from the text order.
    fn prefix_matches(&self, test: &super::StringTest<'_>, limit: u64) -> Vec<TermId> {
        let inner = self.inner.read();
        let key = |index: u64| inner.key(index);
        let needle = test.needle.as_bytes();
        let mut candidates: Vec<u64> = Vec::new();
        let first = match inner.base.as_ref().and_then(|base| base.order.as_ref()) {
            Some(order) => {
                let (start, end) = super::order::prefix_range(order, &key, needle);
                candidates.extend(order[start..end].iter().map(|&i| u64::from(i)));
                inner.base_len()
            }
            None => 0,
        };
        {
            let mut order = self.order.write();
            order.extend(first, inner.len(), &key);
            let (start, end) = super::order::prefix_range(&order.order, &key, needle);
            candidates.extend_from_slice(&order.order[start..end]);
        }
        let mut ids: Vec<TermId> = candidates
            .into_iter()
            .filter(|&index| index < limit)
            .filter_map(|index| super::strings::passes(key(index), index, 0, test))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// The text order of the entries `0..len` as checkpoint indices: the mapped base's
    /// order merged with that of the others, from the in-memory order where it covers them,
    /// else sorted here (and not kept: the checkpoint's base takes them over).
    pub(crate) fn text_order(&self, len: u64) -> Vec<u32> {
        let inner = self.inner.read();
        let key = |index: u64| inner.key(index);
        let text = |index: u64| super::order::text_of(key(index)).unwrap_or_default();
        let (base, first): (&[u32], u64) =
            match inner.base.as_ref().and_then(|base| base.order.as_ref()) {
                Some(order) => (order, inner.base_len()),
                None => (&[], 0),
            };
        let order = self.order.read();
        let sorted: Vec<u32>;
        let rest: Box<dyn Iterator<Item = u64>> =
            if order.first == first && order.covered >= len.max(first) {
                Box::new(order.order.iter().copied().filter(|&i| i < len))
            } else {
                sorted = super::order::sorted(first.min(len)..len, &key);
                Box::new(sorted.iter().map(|&i| u64::from(i)))
            };
        // Merged straight from both orders: no copies of them.
        let capacity = base.len() + len.saturating_sub(first) as usize;
        let mut base = base
            .iter()
            .map(|&i| u64::from(i))
            .filter(|&i| i < len)
            .peekable();
        let mut rest = rest.peekable();
        let mut merged = Vec::with_capacity(capacity);
        loop {
            let next = match (base.peek(), rest.peek()) {
                (Some(&b), Some(&r)) if (text(b), b) <= (text(r), r) => base.next(),
                (Some(_), Some(_)) => rest.next(),
                (Some(_), None) => base.next(),
                (None, Some(_)) => rest.next(),
                (None, None) => break,
            };
            merged.push(next.expect("peeked") as u32);
        }
        merged
    }

    /// Decodes an id back into a term. Returns `None` for ids that do not belong to this
    /// dictionary (or the default-graph marker).
    pub fn decode(&self, id: TermId) -> Option<Term> {
        match id.kind() {
            TermKind::Integer
            | TermKind::Boolean
            | TermKind::Decimal
            | TermKind::Date
            | TermKind::DateTime
            | TermKind::DerivedInteger => inline_to_literal(id).map(Term::from),
            TermKind::DefaultGraph => None,
            TermKind::Iri
            | TermKind::BlankNode
            | TermKind::String
            | TermKind::LangString
            | TermKind::TypedLiteral
            | TermKind::Triple => {
                let ids = {
                    // Recursive: callers may hold a read lock already (`with_views`), and a
                    // plain read would queue behind a waiting writer, which waits for them.
                    let inner = self.inner.read_recursive();
                    if id.payload() >= inner.len() {
                        return None;
                    }
                    let key = inner.key(id.payload());
                    match triple_ids(key) {
                        Some(ids) => ids,
                        None => return Some(decode_key(key)),
                    }
                };
                let [s, p, o] = ids;
                let Term::NamedNode(predicate) = self.decode(p)? else {
                    return None;
                };
                Some(
                    nrese_rdf::Triple::new(
                        NamedOrBlankNode::try_from(self.decode(s)?).ok()?,
                        predicate,
                        self.decode(o)?,
                    )
                    .into(),
                )
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
        if id.payload() >= inner.len() {
            return None;
        }
        Some(f(view_key(inner.key(id.payload()))))
    }

    /// Calls `f` with a lookup of entry views below `limit`, all under one read lock: for
    /// writing many terms (result serialisation) without a lock per term. `f` may decode
    /// ([`Self::decode`] reads recursively) but not intern.
    pub fn with_views<R>(
        &self,
        limit: u64,
        f: impl for<'v> FnOnce(&'v dyn Fn(TermId) -> Option<TermView<'v>>) -> R,
    ) -> R {
        let inner = self.inner.read();
        let len = inner.len().min(limit);
        let view = |id: TermId| {
            (id.kind().is_dictionary() && id.payload() < len)
                .then(|| view_key(inner.key(id.payload())))
        };
        f(&view)
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
        // A new array of the quads' size: `collect` from `slots` would keep its allocation,
        // 64 bytes per quad for 32 (twice the memory of every batch a bulk load holds).
        let mut quads = Vec::with_capacity(slots.len());
        quads.extend(
            slots.iter().map(|&[s, p, o, g]| {
                EncodedQuad::new(resolve(s), resolve(p), resolve(o), resolve(g))
            }),
        );
        quads
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
        (from..to.min(inner.len()))
            .map(|index| inner.key(index).to_vec())
            .collect()
    }

    /// Calls `f` with the key of each entry in `from..to`, in id order, holding the read
    /// lock for the range: checkpoints read the dictionary in chunks with this.
    pub(crate) fn for_each_key(&self, from: u64, to: u64, mut f: impl FnMut(&[u8])) {
        let inner = self.inner.read();
        for index in from..to.min(inner.len()) {
            f(inner.key(index));
        }
    }

    /// Restores a checkpoint's mapped entries into this empty dictionary.
    pub(crate) fn restore_base(&self, base: Base) -> EngineResult<()> {
        let mut inner = self.inner.write();
        if inner.len() != 0 {
            return Err(EngineError::Corruption(
                "checkpoint dictionary restored into a non-empty dictionary".to_owned(),
            ));
        }
        inner.base = Some(base);
        Ok(())
    }

    /// Serves the entries `0..base.len` from `base`, a checkpoint of them that this engine
    /// has written, instead of from memory or an older checkpoint; later entries stay in
    /// memory. `false` (and nothing changes) if the dictionary already uses a longer base.
    pub(crate) fn rebase(&self, base: Base) -> EngineResult<bool> {
        let mut inner = self.inner.write();
        let len = base.len;
        if len <= inner.base_len() {
            return Ok(false);
        }
        if len > inner.len() || base.key(len - 1) != inner.key(len - 1) {
            return Err(EngineError::Corruption(
                "a checkpoint's dictionary doesn't match the dictionary".to_owned(),
            ));
        }
        let mut next = Inner {
            base: Some(base),
            ..Inner::default()
        };
        for index in len..inner.len() {
            let key = inner.key(index);
            next.push(key, key_hash(key));
        }
        *inner = next;
        // The in-memory text order covered entries the base orders now.
        *self.order.write() = super::order::TextOrder::default();
        Ok(true)
    }

    /// Restores a checkpoint's dictionary into this empty one: `keys` in id order. Keys are
    /// validated and hashed in parallel, appended to the arena at once and indexed in a
    /// table sized up front; a duplicate key is corruption.
    pub(crate) fn restore_keys(&self, keys: &[&[u8]]) -> EngineResult<()> {
        use rayon::prelude::*;
        let hashes: Vec<u64> = keys
            .par_iter()
            .map(|key| validate_key(key).map(|()| key_hash(key)))
            .collect::<EngineResult<_>>()?;
        let mut inner = self.inner.write();
        if inner.len() != 0 {
            return Err(EngineError::Corruption(
                "checkpoint dictionary restored into a non-empty dictionary".to_owned(),
            ));
        }
        let Inner {
            table, bytes, ends, ..
        } = &mut *inner;
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
            table.insert_unique(hash, index, |&i| key_hash(key(i)));
        }
        Ok(())
    }

    /// Re-adds a raw key during recovery. The key must land exactly at `expected_index`.
    pub(crate) fn restore_key(&self, expected_index: u64, key: &[u8]) -> EngineResult<()> {
        validate_key(key)?;
        let mut inner = self.inner.write();
        let len = inner.len();
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
        if let Some(id) = dictionary.inline_id(term) {
            return Slot::Id(id);
        }
        // Rare, and its key needs its components' ids: interned at once, under the lock.
        if let TermRef::Triple(_) = term {
            return Slot::Id(dictionary.intern(term));
        }
        let start = self.arena.len();
        let kind = encode_key(term, &mut self.arena);
        let hash = key_hash(&self.arena[start..]);
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
    /// A triple term (RDF 1.2): decode it ([`Dictionary::decode`]) for its parts.
    Triple,
}

impl<'a> TermView<'a> {
    /// The lexical form of a literal, or the IRI string: what SPARQL `STR` returns (`None`
    /// for blank nodes).
    pub fn str(self) -> Option<&'a str> {
        match self {
            Self::Iri(s) | Self::String(s) => Some(s),
            Self::LangString { value, .. } | Self::Typed { value, .. } => Some(value),
            Self::BlankNode(_) | Self::Triple => None,
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
        TAG_TRIPLE | TAG_TRIPLE_IDS => TermView::Triple,
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

/// The key of a triple term with these component ids.
fn triple_key(ids: [TermId; 3], out: &mut Vec<u8>) {
    out.push(TAG_TRIPLE_IDS);
    for id in ids {
        out.extend_from_slice(&id.raw().to_be_bytes());
    }
}

/// The component ids of a triple term's key, if `key` is one.
fn triple_ids(key: &[u8]) -> Option<[TermId; 3]> {
    if key.len() != TRIPLE_KEY_LEN || key[0] != TAG_TRIPLE_IDS {
        return None;
    }
    let id = |i: usize| {
        let bytes: [u8; 8] = key[1 + 8 * i..9 + 8 * i].try_into().unwrap_or_default();
        TermId::from_raw(u64::from_be_bytes(bytes))
    };
    Some([id(0), id(1), id(2)])
}

/// A triple term from the N-Triples text of a key the first RDF 1.2 version wrote.
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
        && std::str::from_utf8(&key[1..]).is_ok()
        || (key.len() == TRIPLE_KEY_LEN && key[0] == TAG_TRIPLE_IDS);
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
        // Found by its predicate among the triple terms.
        assert_eq!(
            dict.triple_terms(
                None,
                Some(
                    dict.lookup(NamedNodeRef::new_unchecked("http://e/p").into())
                        .unwrap()
                ),
                None,
                u64::MAX
            )
            .len(),
            1
        );
        assert_eq!(
            dict.with_view(ids[2], |v| format!("{v:?}")).unwrap(),
            "Triple"
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
