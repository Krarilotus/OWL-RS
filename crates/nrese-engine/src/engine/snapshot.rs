//! Read-only, point-in-time view of the dataset.

use std::sync::Arc;

use oxrdf::{Quad, QuadRef, Term, TermRef};

use super::Version;
use crate::quad::{EncodedQuad, QuadPattern};
use crate::term::{Dictionary, TermId};

/// A consistent view of one committed revision. Cheap to clone; holding it keeps that
/// revision's runs alive but never blocks writers or compaction.
///
/// Term lookups are bounded by the dictionary size at the snapshot's revision, so terms
/// interned later (even by an open transaction) are reported as unknown, which keeps term
/// identity stable for the lifetime of a query.
#[derive(Clone)]
pub struct Snapshot {
    version: Arc<Version>,
    dictionary: Arc<Dictionary>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("revision", &self.revision())
            .field("len", &self.len())
            .finish()
    }
}

impl Snapshot {
    pub(super) fn new(version: Arc<Version>, dictionary: Arc<Dictionary>) -> Self {
        Self {
            version,
            dictionary,
        }
    }

    pub(crate) fn dictionary(&self) -> &Dictionary {
        &self.dictionary
    }

    /// Dictionary entries visible to this snapshot.
    pub(crate) fn dictionary_len(&self) -> u64 {
        self.version.dictionary_len
    }

    pub fn revision(&self) -> u64 {
        self.version.revision
    }

    /// Number of quads. O(1).
    pub fn len(&self) -> u64 {
        self.version.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, quad: &EncodedQuad) -> bool {
        self.version.index.contains(quad)
    }

    /// All quads matching `pattern`. O(r log n + k·r) for k results over r runs.
    /// The iterator borrows only the snapshot, not `pattern`.
    pub fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        self.version.index.scan(pattern)
    }

    /// True if the named graph `graph` contains at least one quad. O(r log n).
    pub fn contains_named_graph(&self, graph: TermId) -> bool {
        !graph.is_default_graph()
            && self
                .quads_for_pattern(&QuadPattern::in_graph(graph))
                .next()
                .is_some()
    }

    /// Ids of all named graphs that contain at least one quad, in id order.
    pub fn named_graphs(&self) -> impl Iterator<Item = TermId> + '_ {
        let index = &self.version.index;
        std::iter::successors(index.next_named_graph(None), |&graph| {
            index.next_named_graph(Some(graph))
        })
    }

    /// Id of `term` as of this snapshot; `None` if the term is unknown (so no quad uses it).
    pub fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.dictionary
            .lookup_bounded(term, self.version.dictionary_len)
    }

    pub fn lookup_quad(&self, quad: QuadRef<'_>) -> Option<EncodedQuad> {
        self.dictionary
            .lookup_quad_bounded(quad, self.version.dictionary_len)
    }

    pub fn decode(&self, id: TermId) -> Option<Term> {
        self.dictionary.decode(id)
    }

    pub fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.dictionary.decode_quad(quad)
    }
}
