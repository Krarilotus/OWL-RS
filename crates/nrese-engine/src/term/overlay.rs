//! A transaction's private vocabulary: the lifecycle contract's overlay
//! (`notes/2026-11-08-dictionary-lifecycle-contract.md` P2, §4.1, §4.3; package 2a's design note
//! `notes/2026-11-08-p2-bounded-vocabulary-design.md`).
//!
//! An [`Overlay`] resolves terms through the dictionary below its `start` (the length of the
//! snapshot it opened over: append-only, so those ids never change) and then through its own
//! [`Suffix`], where it adds what the dictionary didn't have at `start`. A term has one id in one
//! overlay: shared if the dictionary had it at `start`, else its own, numbered from `start`. A
//! triple term's components resolve first. What it adds is charged to its budget, and leaves
//! nothing in the dictionary.
//!
//! [`Overlay::freeze`] makes an [`OverlayView`]: the entries there were then, for a snapshot to
//! hold. Lookups through a view still read the overlay's table, accepting only its own entries.

use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, Reservation};
use nrese_rdf::{NamedOrBlankNode, Term, TermRef, Triple};
use parking_lot::Mutex;

use super::charged::ChargedArc;
use super::dictionary::{
    Dictionary, TRIPLE_KEY_LEN, decode_key, encode_key, encoded_len, key_lookup_bytes, triple_ids,
    triple_key, view_key,
};

/// The most bytes the dictionary's probe of a key of `encoded` bytes allocates, on a plain or a
/// compressed base, whichever is more ([`key_lookup_bytes`]).
fn probe_bytes(encoded: usize) -> usize {
    key_lookup_bytes(encoded, false).max(key_lookup_bytes(encoded, true))
}

/// The most bytes a lookup of `term` through an overlay or a view holds at once: a probe and
/// then an own key, one after the other; for a triple term, its components' lookups one after
/// another, then its own probe and key.
fn lookup_scratch(term: TermRef<'_>) -> usize {
    match term {
        TermRef::Triple(triple) => [
            lookup_scratch(triple.subject.as_ref().into()),
            lookup_scratch(triple.predicate.as_ref().into()),
            lookup_scratch(triple.object.as_ref()),
            probe_bytes(TRIPLE_KEY_LEN) + TRIPLE_KEY_LEN,
        ]
        .into_iter()
        .max()
        .unwrap_or(0),
        _ => probe_bytes(encoded_len(term)) + encoded_len(term),
    }
}
/// The buffer interning makes its keys in, its capacity charged: grown as a key needs, the new
/// capacity charged before it is allocated and the old released after it is freed. The bytes
/// are freed before the charge is released (the fields' order).
struct KeyBuffer<'b> {
    bytes: Vec<u8>,
    held: Reservation<&'b Budget>,
}

impl<'b> KeyBuffer<'b> {
    fn new(budget: &'b Budget) -> Self {
        Self {
            bytes: Vec::new(),
            held: Reservation::new(budget),
        }
    }

    /// The buffer, cleared, with room for a key of `len` bytes: grown, if it must be, to at
    /// least 64 bytes (as the dictionary's own key buffer starts) or twice what it had.
    fn room(&mut self, len: usize) -> Result<&mut Vec<u8>, BudgetExceeded> {
        self.bytes.clear();
        let had = self.bytes.capacity();
        if len > had {
            let target = len.max(64).max(2 * had);
            self.held.charge(target)?;
            self.bytes = Vec::with_capacity(target);
            self.held.release(had);
        }
        Ok(&mut self.bytes)
    }
}
use super::hash::key_hash;
use super::suffix::{Frozen, Suffix};
use super::{TermId, TermKind, TermView};

/// A transaction's private vocabulary over the dictionary's entries below `start`.
pub(crate) struct Overlay {
    dictionary: Arc<Dictionary>,
    start: u64,
    budget: Arc<Budget>,
    /// Its own entries. A leaf lock: held for one probe and push, never while taking another
    /// lock or running parallel work.
    suffix: Mutex<Suffix>,
}

impl Overlay {
    /// An overlay over `dictionary`'s entries below `start` (the snapshot's length it opens
    /// over), its own entries charged to `budget`.
    pub(crate) fn over(dictionary: Arc<Dictionary>, start: u64, budget: Arc<Budget>) -> Arc<Self> {
        let suffix = Mutex::new(Suffix::at(start, Arc::clone(&budget)));
        Arc::new(Self {
            dictionary,
            start,
            budget,
            suffix,
        })
    }

    /// Its `start`, which its views report ([`OverlayView::start`]): where its own ids begin.
    pub(crate) fn start(&self) -> u64 {
        self.start
    }

    /// What its own entries are charged to.
    pub(crate) fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    /// The end of its ids now: `start` plus its own entries so far.
    pub(crate) fn end(&self) -> u64 {
        self.start + self.suffix.lock().len()
    }

    /// The id of `term`: inline, the dictionary's below `start`, or this overlay's own, added
    /// if it is new. What it allocates is charged first. Refused, `term` is not added; a triple
    /// term's components interned before the refusal stay, each its own entry, charged (a later
    /// intern finds them; one id per term holds).
    pub(crate) fn intern(&self, term: TermRef<'_>) -> Result<TermId, BudgetExceeded> {
        if let Some(id) = self.dictionary.inline_id(term) {
            return Ok(id);
        }
        if let TermRef::Triple(_) = term {
            return self.intern_with(term, &mut KeyBuffer::new(&self.budget));
        }
        // One term: its key charged first, and freed before the charge is released (declared
        // after it); no buffer is kept across terms.
        let len = encoded_len(term);
        let mut held = Reservation::new(&*self.budget);
        held.charge(len)?;
        let mut key = Vec::with_capacity(len);
        let kind = encode_key(term, &mut key);
        self.probe_or_own(&key, kind)
    }

    /// `key`'s id: the dictionary's below `start`, else this overlay's own, added if absent.
    /// Encoded and hashed once for both. The dictionary's probe is charged as the base it reads
    /// needs ([`Dictionary::lookup_key_charged`]: nothing on a plain one), decided under the
    /// probe's own guard, so a rebase between bound and probe can't escape it.
    fn probe_or_own(&self, key: &[u8], kind: TermKind) -> Result<TermId, BudgetExceeded> {
        let hash = key_hash(key);
        let mut scratch = Reservation::new(&*self.budget);
        let shared = self
            .dictionary
            .lookup_key_charged(key, hash, kind, self.start, |bytes| scratch.charge(bytes))?;
        drop(scratch);
        if let Some(id) = shared {
            return Ok(id);
        }
        self.own(key, hash, kind)
    }

    /// [`Self::intern`], making each key in `key`: one buffer for a quad's terms, as the
    /// dictionary's own `intern_quad` keeps (dictionary lifecycle V1, count 1: a buffer per term
    /// and a second encoding of an absent one cost the session read and the SHACL preview
    /// allocations the shared path doesn't make). The key is encoded and hashed once, for the
    /// dictionary's probe and, absent there, as an own entry's.
    fn intern_with(
        &self,
        term: TermRef<'_>,
        key: &mut KeyBuffer<'_>,
    ) -> Result<TermId, BudgetExceeded> {
        if let Some(id) = self.dictionary.inline_id(term) {
            return Ok(id);
        }
        if let TermRef::Triple(triple) = term {
            // The components first: their ids make the key, and come before its own.
            let ids = [
                self.intern_with(triple.subject.as_ref().into(), key)?,
                self.intern_with(triple.predicate.as_ref().into(), key)?,
                self.intern_with(triple.object.as_ref(), key)?,
            ];
            // The dictionary's probe of the key (on a compressed base, its compressed form too),
            // then this one's key: charged first, at the larger of the two bases' bounds, since
            // the probe takes its own guard and a rebase may change the base meanwhile.
            let mut scratch = Reservation::new(&*self.budget);
            scratch.charge(probe_bytes(TRIPLE_KEY_LEN))?;
            if let Some(id) = self.dictionary.triple_term_bounded(ids, self.start) {
                return Ok(id);
            }
            let bytes = key.room(TRIPLE_KEY_LEN)?;
            triple_key(ids, bytes);
            return self.own(bytes, key_hash(bytes), TermKind::Triple);
        }
        // The key made in the buffer (its growth charged there); the probe of a key already made
        // charged as the base it reads needs ([`Self::probe_or_own`]).
        let bytes = key.room(encoded_len(term))?;
        let kind = encode_key(term, bytes);
        self.probe_or_own(bytes, kind)
    }

    /// `key`'s id among this overlay's own entries, added if absent; `hash` is its [`key_hash`].
    fn own(&self, key: &[u8], hash: u64, kind: TermKind) -> Result<TermId, BudgetExceeded> {
        let mut suffix = self.suffix.lock();
        let id = match suffix.find(hash, key) {
            Some(id) => id,
            None => suffix.push(key, hash)?,
        };
        Ok(TermId::new(kind, id))
    }

    /// [`Self::intern`] of each of `quad`'s terms (the default graph as itself). Refused, the
    /// terms interned before the refusal stay, as [`Self::intern`]'s components do.
    pub(crate) fn intern_quad(
        &self,
        quad: nrese_rdf::QuadRef<'_>,
    ) -> Result<crate::EncodedQuad, BudgetExceeded> {
        // One key buffer for the four terms ([`Self::intern_with`]).
        let mut key = KeyBuffer::new(&self.budget);
        let graph = match super::dictionary::graph_term(quad.graph_name) {
            Some(term) => self.intern_with(term, &mut key)?,
            None => TermId::DEFAULT_GRAPH,
        };
        Ok(crate::EncodedQuad::new(
            self.intern_with(quad.subject.into(), &mut key)?,
            self.intern_with(quad.predicate.into(), &mut key)?,
            self.intern_with(quad.object, &mut key)?,
            graph,
        ))
    }

    /// The id of `term` if it is known: inline, shared below `start`, or this overlay's own.
    pub(crate) fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.lookup_below(term, u64::MAX)
    }

    /// [`Self::lookup`] of each of `quad`'s terms (the default graph as itself); `None` if one
    /// is unknown. Interns nothing.
    pub(crate) fn lookup_quad(&self, quad: nrese_rdf::QuadRef<'_>) -> Option<crate::EncodedQuad> {
        let graph = match super::dictionary::graph_term(quad.graph_name) {
            Some(term) => self.lookup(term)?,
            None => TermId::DEFAULT_GRAPH,
        };
        Some(crate::EncodedQuad::new(
            self.lookup(quad.subject.into())?,
            self.lookup(quad.predicate.into())?,
            self.lookup(quad.object)?,
            graph,
        ))
    }

    /// [`Self::decode`] of each of `quad`'s terms, as a view of this overlay decodes a quad.
    pub(crate) fn decode_quad(&self, quad: crate::EncodedQuad) -> Option<nrese_rdf::Quad> {
        let graph_name = if quad.graph.is_default_graph() {
            nrese_rdf::GraphName::DefaultGraph
        } else {
            NamedOrBlankNode::try_from(self.decode(quad.graph)?)
                .ok()?
                .into()
        };
        let Term::NamedNode(predicate) = self.decode(quad.predicate)? else {
            return None;
        };
        Some(nrese_rdf::Quad::new(
            NamedOrBlankNode::try_from(self.decode(quad.subject)?).ok()?,
            predicate,
            self.decode(quad.object)?,
            graph_name,
        ))
    }

    /// The term of `id`: inline or the dictionary's below `start`, else this overlay's own,
    /// as a view of it decodes. An own key is copied out of the leaf lock first, so a triple
    /// term's components are decoded with no lock held under it. Not charged, as
    /// `Snapshot::decode` isn't.
    pub(crate) fn decode(&self, id: TermId) -> Option<Term> {
        if !id.kind().is_dictionary() || id.payload() < self.start {
            return self.dictionary.decode(id);
        }
        let key = self.suffix.lock().key(id.payload())?.to_vec();
        let Some([s, p, o]) = triple_ids(&key) else {
            return Some(decode_key(&key));
        };
        let Term::NamedNode(predicate) = self.decode(p)? else {
            return None;
        };
        Some(
            Triple::new(
                NamedOrBlankNode::try_from(self.decode(s)?).ok()?,
                predicate,
                self.decode(o)?,
            )
            .into(),
        )
    }

    /// [`Self::lookup`], counting only own entries below `end`.
    fn lookup_below(&self, term: TermRef<'_>, end: u64) -> Option<TermId> {
        if let Some(id) = self.dictionary.inline_id(term) {
            return Some(id);
        }
        if let TermRef::Triple(triple) = term {
            let ids = [
                self.lookup_below(triple.subject.as_ref().into(), end)?,
                self.lookup_below(triple.predicate.as_ref().into(), end)?,
                self.lookup_below(triple.object.as_ref(), end)?,
            ];
            return self.triple_term_below(ids, end);
        }
        // One key and one hash for both probes, the dictionary's and the own entries'
        // (dictionary lifecycle V1, count 2: a second encoding, hash and allocation made an
        // absent term's private lookup ×1.36 the shared one's).
        let mut key = Vec::with_capacity(encoded_len(term));
        let kind = encode_key(term, &mut key);
        self.probe_below(&key, kind, end)
    }

    /// The triple term of `components` below `end`: the dictionary's below `start`, else own.
    fn triple_term_below(&self, components: [TermId; 3], end: u64) -> Option<TermId> {
        let mut key = Vec::with_capacity(TRIPLE_KEY_LEN);
        triple_key(components, &mut key);
        self.probe_below(&key, TermKind::Triple, end)
    }

    /// `key`'s id below `end`: the dictionary's below `start`, else own. No own probe where
    /// there can be no own entry below `end`.
    fn probe_below(&self, key: &[u8], kind: TermKind, end: u64) -> Option<TermId> {
        let hash = key_hash(key);
        if let Some(id) = self
            .dictionary
            .lookup_key_bounded(key, hash, kind, self.start)
        {
            return Some(id);
        }
        if end <= self.start {
            return None;
        }
        self.suffix
            .lock()
            .find(hash, key)
            .filter(|&id| id < end)
            .map(|id| TermId::new(kind, id))
    }

    /// The entries now, for a snapshot to hold: later interns don't change it. Its lists and
    /// its block are charged to this overlay's budget.
    pub(crate) fn freeze(self: &Arc<Self>) -> Result<OverlayView, BudgetExceeded> {
        let frozen = self.suffix.lock().freeze()?;
        let state = ViewState {
            overlay: Arc::clone(self),
            frozen,
        };
        Ok(OverlayView(ChargedArc::new(state, &self.budget)?))
    }
}

/// An overlay's entries at one moment ([`Overlay::freeze`]): the dictionary's below `start`, its
/// own `start..start + n`. Its identity is its overlay's. Cloned by every snapshot made from the
/// one holding it; its block is charged and released after it is freed ([`ChargedArc`]).
#[derive(Clone)]
pub(crate) struct OverlayView(ChargedArc<ViewState>);

pub(crate) struct ViewState {
    overlay: Arc<Overlay>,
    frozen: Frozen,
}

impl std::ops::Deref for OverlayView {
    type Target = ViewState;

    fn deref(&self) -> &ViewState {
        &self.0
    }
}

impl OverlayView {
    pub(crate) fn dictionary(&self) -> &Arc<Dictionary> {
        &self.overlay.dictionary
    }

    pub(crate) fn start(&self) -> u64 {
        self.overlay.start
    }

    /// The end of its ids: `start + n`.
    pub(crate) fn end(&self) -> u64 {
        self.overlay.start + self.frozen.len()
    }

    /// Test support: its own entries' count.
    #[cfg(test)]
    pub(crate) fn len(&self) -> u64 {
        self.frozen.len()
    }

    /// Whether `other` is a view of the same overlay.
    pub(crate) fn same_overlay(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.overlay, &other.overlay)
    }

    /// Own entry `id`'s key (a dictionary-kind id at or past `start`).
    fn own_key(&self, id: TermId) -> Option<&[u8]> {
        (id.kind().is_dictionary() && id.payload() >= self.start())
            .then(|| self.frozen.key(id.payload()))
            .flatten()
    }

    fn is_own(&self, id: TermId) -> bool {
        id.kind().is_dictionary() && id.payload() >= self.start()
    }

    pub(crate) fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.overlay.lookup_below(term, self.end())
    }

    /// [`Self::lookup`], what it allocates charged first through `charge` ([`lookup_scratch`]):
    /// at the larger of a plain and a compressed base's bounds, since its probes take their own
    /// guards and a rebase may change the base between them.
    pub(crate) fn lookup_charged<E>(
        &self,
        term: TermRef<'_>,
        charge: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Option<TermId>, E> {
        charge(lookup_scratch(term))?;
        Ok(self.lookup(term))
    }

    pub(crate) fn lookup_quad(&self, quad: nrese_rdf::QuadRef<'_>) -> Option<crate::EncodedQuad> {
        let graph = match super::dictionary::graph_term(quad.graph_name) {
            Some(term) => self.lookup(term)?,
            None => TermId::DEFAULT_GRAPH,
        };
        Some(crate::EncodedQuad::new(
            self.lookup(quad.subject.into())?,
            self.lookup(quad.predicate.into())?,
            self.lookup(quad.object)?,
            graph,
        ))
    }

    pub(crate) fn triple_term(&self, components: [TermId; 3]) -> Option<TermId> {
        self.overlay.triple_term_below(components, self.end())
    }

    pub(crate) fn components(&self, id: TermId) -> Option<[TermId; 3]> {
        if id.kind() != TermKind::Triple {
            return None;
        }
        if id.payload() < self.start() {
            return self.dictionary().components(id);
        }
        triple_ids(self.own_key(id)?)
    }

    pub(crate) fn decode(&self, id: TermId) -> Option<Term> {
        if !self.is_own(id) {
            return self.dictionary().decode(id);
        }
        let key = self.own_key(id)?;
        let Some([s, p, o]) = triple_ids(key) else {
            return Some(decode_key(key));
        };
        let Term::NamedNode(predicate) = self.decode(p)? else {
            return None;
        };
        Some(
            Triple::new(
                NamedOrBlankNode::try_from(self.decode(s)?).ok()?,
                predicate,
                self.decode(o)?,
            )
            .into(),
        )
    }

    pub(crate) fn decode_charged<E>(
        &self,
        id: TermId,
        charge: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Option<Term>, E> {
        if !self.is_own(id) {
            return self.dictionary().decode_charged(id, charge);
        }
        // As the dictionary's: a triple term's decode is its components' (`None` here).
        let Some(key) = self.own_key(id).filter(|_| id.kind() != TermKind::Triple) else {
            return Ok(None);
        };
        charge(key.len())?;
        Ok(Some(decode_key(key)))
    }

    pub(crate) fn decode_bound(&self, id: TermId) -> Option<usize> {
        if !self.is_own(id) {
            return self.dictionary().decode_bound(id);
        }
        self.own_key(id)
            .filter(|_| id.kind() != TermKind::Triple)
            .map(<[u8]>::len)
    }

    pub(crate) fn with_view<R>(&self, id: TermId, f: impl FnOnce(TermView<'_>) -> R) -> Option<R> {
        if !self.is_own(id) {
            return self.dictionary().with_view(id, f);
        }
        self.own_key(id).map(|key| f(view_key(key)))
    }

    /// Own entry `id`'s view, from the frozen entries (no lock); `None` for an id below
    /// `start`, which the dictionary answers (`Vocabulary::with_views` asks it under one of
    /// its read guards).
    #[inline]
    pub(crate) fn own_view(&self, id: TermId) -> Option<Option<TermView<'_>>> {
        self.is_own(id).then(|| self.own_entry_view(id))
    }

    /// Out of line, so a caller's loop over [`Vocabulary::with_views`] inlines one key decode
    /// (the dictionary's), not two; with two, the decode itself moved out of the loop on the
    /// shared path too (package 2b's B17 count).
    ///
    /// [`Vocabulary::with_views`]: super::resolve::Vocabulary::with_views
    #[inline(never)]
    fn own_entry_view(&self, id: TermId) -> Option<TermView<'_>> {
        self.own_key(id).map(view_key)
    }

    pub(crate) fn decode_quad(&self, quad: crate::EncodedQuad) -> Option<nrese_rdf::Quad> {
        let graph_name = if quad.graph.is_default_graph() {
            nrese_rdf::GraphName::DefaultGraph
        } else {
            NamedOrBlankNode::try_from(self.decode(quad.graph)?)
                .ok()?
                .into()
        };
        let Term::NamedNode(predicate) = self.decode(quad.predicate)? else {
            return None;
        };
        Some(nrese_rdf::Quad::new(
            NamedOrBlankNode::try_from(self.decode(quad.subject)?).ok()?,
            predicate,
            self.decode(quad.object)?,
            graph_name,
        ))
    }

    /// The dictionary's matches below `start`, then its own, each sorted: every own id is above
    /// every shared one.
    pub(crate) fn matching_strings_scan(
        &self,
        test: &super::StringTest<'_>,
    ) -> (Vec<TermId>, super::StringScan) {
        let (mut found, mut scan) = self.dictionary().matching_strings_scan(test, self.start());
        let matcher = super::strings::Matcher::new(test);
        let mut own = Vec::new();
        let mut tested = 0;
        for (id, key) in self.frozen.entries() {
            let ends = [key.len() as u64];
            tested += matcher.slice(key, &ends[..], (0, 1), id, &mut own);
        }
        if self.frozen.len() > 0 {
            scan.arenas += 1;
            scan.slices += 1;
            scan.keys_tested += tested;
        }
        // A key a pass finds is pushed once per pass: sorted, then each once.
        own.sort_unstable();
        own.dedup();
        // Sorted as the dictionary's are: by id, kind first. An own id is above every shared one
        // of its kind, not above every shared one: a shared language-tagged string sorts after
        // an own simple literal.
        found.extend(own);
        found.sort_unstable();
        (found, scan)
    }

    pub(crate) fn matching_strings(&self, test: &super::StringTest<'_>) -> Vec<TermId> {
        self.matching_strings_scan(test).0
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
