//! Write transactions: an exact delta over a base snapshot, published atomically on commit.

use hashbrown::HashSet;
use oxrdf::{QuadRef, Term, TermRef};
use parking_lot::MutexGuard;

use super::{Inner, Snapshot, Version};
use crate::durability::codec::CommitRecord;
use crate::error::EngineResult;
use crate::index::run::Run;
use crate::quad::{EncodedQuad, QuadPattern};
use crate::term::TermId;

/// Result of a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitSummary {
    /// Revision after the commit; unchanged if the transaction made no net change.
    pub revision: u64,
    pub inserted: u64,
    pub deleted: u64,
}

/// A write transaction holding the engine's single writer slot.
///
/// Pending changes are kept as an *exact* delta against the base snapshot: `inserts` are
/// absent from the base, `deletes` are present in it, and the two sets are disjoint. That
/// invariant is what the index's sign-sum visibility rule relies on.
///
/// Reads through the transaction ([`quads_for_pattern`](Self::quads_for_pattern),
/// [`contains`](Self::contains)) see the base plus pending changes, so later operations of
/// one SPARQL update request observe earlier ones. Dropping the transaction aborts it.
pub struct Transaction<'e> {
    engine: &'e Inner,
    _slot: MutexGuard<'e, ()>,
    base: Snapshot,
    inserts: HashSet<EncodedQuad>,
    deletes: HashSet<EncodedQuad>,
}

impl<'e> Transaction<'e> {
    pub(super) fn new(engine: &'e Inner, slot: MutexGuard<'e, ()>) -> Self {
        Self {
            base: engine.shared.snapshot(),
            engine,
            _slot: slot,
            inserts: HashSet::new(),
            deletes: HashSet::new(),
        }
    }

    /// The state this transaction started from, without its pending changes.
    pub fn base(&self) -> &Snapshot {
        &self.base
    }

    /// Number of quads including pending changes. O(1).
    pub fn len(&self) -> u64 {
        self.base.len() + self.inserts.len() as u64 - self.deletes.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pending (inserted, deleted) counts.
    pub fn pending(&self) -> (usize, usize) {
        (self.inserts.len(), self.deletes.len())
    }

    pub fn contains(&self, quad: &EncodedQuad) -> bool {
        self.inserts.contains(quad) || (!self.deletes.contains(quad) && self.base.contains(quad))
    }

    /// Quads matching `pattern`, including pending changes. Base quads come first, in index
    /// order, followed by pending inserts in no particular order.
    pub fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        let pattern = *pattern;
        let deletes = &self.deletes;
        let base = self
            .base
            .quads_for_pattern(&pattern)
            .filter(move |quad| !deletes.contains(quad));
        let pending = self
            .inserts
            .iter()
            .copied()
            .filter(move |quad| pattern.matches(quad));
        base.chain(pending)
    }

    /// True if the named graph `graph` contains at least one quad, including pending changes.
    pub fn contains_named_graph(&self, graph: TermId) -> bool {
        !graph.is_default_graph()
            && self
                .quads_for_pattern(&QuadPattern::in_graph(graph))
                .next()
                .is_some()
    }

    /// Named graphs that are non-empty including pending changes, in id order.
    pub fn named_graphs(&self) -> Vec<TermId> {
        let mut graphs: Vec<TermId> = self
            .base
            .named_graphs()
            .chain(self.inserts.iter().map(|quad| quad.graph))
            .filter(|graph| !graph.is_default_graph())
            .collect();
        graphs.sort_unstable();
        graphs.dedup();
        graphs.retain(|&graph| self.contains_named_graph(graph));
        graphs
    }

    /// Returns the id of `term`, adding it to the dictionary if needed.
    pub fn intern(&self, term: TermRef<'_>) -> TermId {
        self.engine.shared.dictionary.intern(term)
    }

    /// Id of `term` if it is known. Includes terms interned by this transaction.
    pub fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.engine.shared.dictionary.lookup(term)
    }

    pub fn decode(&self, id: TermId) -> Option<Term> {
        self.engine.shared.dictionary.decode(id)
    }

    /// Adds `quad`; returns `false` if it was already present.
    pub fn insert(&mut self, quad: QuadRef<'_>) -> bool {
        let encoded = self.engine.shared.dictionary.intern_quad(quad);
        self.insert_encoded(encoded)
    }

    /// Removes `quad`; returns `false` if it was not present. Never interns.
    pub fn remove(&mut self, quad: QuadRef<'_>) -> bool {
        match self
            .engine
            .shared
            .dictionary
            .lookup_quad_bounded(quad, u64::MAX)
        {
            Some(encoded) => self.remove_encoded(encoded),
            None => false,
        }
    }

    /// Adds an already-encoded quad. Its ids must come from this engine's dictionary.
    pub fn insert_encoded(&mut self, quad: EncodedQuad) -> bool {
        if self.deletes.remove(&quad) {
            return true; // re-insert of a quad deleted earlier in this transaction
        }
        !self.base.contains(&quad) && self.inserts.insert(quad)
    }

    pub fn remove_encoded(&mut self, quad: EncodedQuad) -> bool {
        if self.inserts.remove(&quad) {
            return true; // undo an insert made earlier in this transaction
        }
        self.base.contains(&quad) && self.deletes.insert(quad)
    }

    /// Removes every quad matching `pattern` (SPARQL `CLEAR`/`DROP`, GSP `PUT`/`DELETE`).
    /// Returns the number of quads removed.
    pub fn remove_matching(&mut self, pattern: &QuadPattern) -> u64 {
        let matching: Vec<_> = self.quads_for_pattern(pattern).collect();
        matching
            .into_iter()
            .map(|quad| u64::from(self.remove_encoded(quad)))
            .sum()
    }

    /// Publishes the pending changes as one new revision. In durable mode the change is in
    /// the WAL (and synced, per policy) before it becomes visible. Cost O(d log d) for a delta
    /// of d quads plus bounded inline compaction; independent of the dataset size.
    ///
    /// On error nothing is published and the transaction is discarded.
    pub fn commit(self) -> EngineResult<CommitSummary> {
        let Self {
            engine,
            _slot,
            base,
            inserts,
            deletes,
        } = self;
        let (inserted, deleted) = (inserts.len() as u64, deletes.len() as u64);
        if inserted + deleted == 0 {
            return Ok(CommitSummary {
                revision: base.revision(),
                inserted,
                deleted,
            });
        }
        let shared = &engine.shared;
        let dictionary_len = shared.dictionary.len();
        let dictionary_start = base.dictionary_len();
        let record = CommitRecord {
            revision: base.revision() + 1,
            dictionary_start,
            // Every term interned since the last commit, including by aborted transactions,
            // so the logged dictionary stays one contiguous id range.
            keys: match shared.durable {
                Some(_) => shared
                    .dictionary
                    .export_keys(dictionary_start, dictionary_len),
                None => Vec::new(),
            },
            inserts: inserts.into_iter().collect(),
            deletes: deletes.into_iter().collect(),
        };
        let run = Run::from_delta(&record.inserts, &record.deletes);
        let revision = record.revision;
        let publish = move |current: &Version| {
            debug_assert_eq!(current.revision + 1, revision, "single writer");
            Version {
                index: current.index.with_run(run),
                revision,
                dictionary_len,
            }
        };
        let wal_bytes = match &shared.durable {
            Some(durable) => {
                let mut wal = durable.wal.lock();
                wal.append(&record)?;
                shared.versions.publish(publish);
                wal.bytes_since_checkpoint()
            }
            None => {
                shared.versions.publish(publish);
                0
            }
        };
        engine.after_commit(wal_bytes);
        Ok(CommitSummary {
            revision,
            inserted,
            deleted,
        })
    }
}
