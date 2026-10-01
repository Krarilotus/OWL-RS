//! Write transactions: exact deltas over a base snapshot, published atomically on commit.

use hashbrown::HashSet;
use nrese_rdf::{Quad, QuadRef, Term, TermRef};
use parking_lot::MutexGuard;

use super::{Inner, ReadModel, Snapshot, Stack, Version};
use crate::durability::codec::CommitRecord;
use crate::error::EngineResult;
use crate::quad::{EncodedQuad, EncodedTriple, QuadPattern};
use crate::term::TermId;

/// Result of a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitSummary {
    /// Revision after the commit; unchanged if the transaction made no net change.
    pub revision: u64,
    /// Asserted quads added and removed.
    pub inserted: u64,
    pub deleted: u64,
    /// Inferred quads added and removed.
    pub inferred_inserted: u64,
    pub inferred_deleted: u64,
}

/// Pending changes to one stack, kept *exact* against the base snapshot: `inserts` are
/// absent from the base, `deletes` are present in it, and the two sets are disjoint. That
/// invariant is what the index's sign-sum visibility rule relies on.
#[derive(Default)]
struct Delta {
    inserts: HashSet<EncodedQuad>,
    deletes: HashSet<EncodedQuad>,
}

impl Delta {
    /// Adds `quad`; returns `false` if it was already visible. `in_base` is only evaluated
    /// when needed.
    fn insert(&mut self, quad: EncodedQuad, in_base: impl FnOnce() -> bool) -> bool {
        if self.deletes.remove(&quad) {
            return true; // re-insert of a quad deleted earlier in this transaction
        }
        !in_base() && self.inserts.insert(quad)
    }

    /// Removes `quad`; returns `false` if it was not visible.
    fn remove(&mut self, quad: EncodedQuad, in_base: impl FnOnce() -> bool) -> bool {
        if self.inserts.remove(&quad) {
            return true; // undo an insert made earlier in this transaction
        }
        in_base() && self.deletes.insert(quad)
    }

    fn contains(&self, quad: &EncodedQuad, in_base: impl FnOnce() -> bool) -> bool {
        self.inserts.contains(quad) || (!self.deletes.contains(quad) && in_base())
    }

    fn counts(&self) -> (usize, usize) {
        (self.inserts.len(), self.deletes.len())
    }

    fn net(&self) -> i64 {
        self.inserts.len() as i64 - self.deletes.len() as i64
    }
}

/// A write transaction holding the engine's single writer slot.
///
/// It keeps one exact delta per stack. Asserted quads are written through the plain methods
/// ([`insert`](Self::insert), [`remove`](Self::remove), …); inferred ones only through the
/// `*_inferred` methods, which the reasoner uses and request handlers must not.
///
/// **Disjointness.** No quad is ever both asserted and inferred. An inferred quad is visible
/// only while it is not asserted (including pending changes), and the commit drops every
/// inferred quad that ends up asserted: it becomes explicit, as in GraphDB.
/// [`insert_inferred`](Self::insert_inferred) refuses asserted quads. Retracting an asserted
/// quad never touches the inferred stack; re-deriving a still-supported statement is the
/// reasoner's job. Because the rule is applied to the final state, asserting and retracting
/// a quad within one transaction leaves its inferred copy intact.
///
/// Reads through the transaction see the base plus pending changes, so later operations of
/// one SPARQL update request observe earlier ones. Reads without a model argument use
/// [`ReadModel::Materialised`]. Dropping the transaction aborts it.
pub struct Transaction<'e> {
    engine: &'e Inner,
    _slot: MutexGuard<'e, ()>,
    base: Snapshot,
    asserted: Delta,
    inferred: Delta,
}

impl<'e> Transaction<'e> {
    pub(super) fn new(engine: &'e Inner, slot: MutexGuard<'e, ()>) -> Self {
        Self {
            base: engine.shared.snapshot(),
            engine,
            _slot: slot,
            asserted: Delta::default(),
            inferred: Delta::default(),
        }
    }

    fn delta(&self, stack: Stack) -> &Delta {
        match stack {
            Stack::Asserted => &self.asserted,
            Stack::Inferred => &self.inferred,
        }
    }

    /// True if `quad` is in `stack`'s base plus pending delta, before disjointness is applied.
    fn delta_contains(&self, stack: Stack, quad: &EncodedQuad) -> bool {
        self.delta(stack)
            .contains(quad, || self.base.stack_contains(stack, quad))
    }

    /// True if `quad` is visible in `stack`, including pending changes. An asserted quad
    /// hides the same inferred quad, as the commit will drop it.
    fn stack_contains(&self, stack: Stack, quad: &EncodedQuad) -> bool {
        self.delta_contains(stack, quad)
            && (stack == Stack::Asserted || !self.delta_contains(Stack::Asserted, quad))
    }

    /// Inferred quads that are also asserted after the pending changes; the commit drops them
    /// from the inferred stack. The base stacks are disjoint, so only pending inserts of
    /// either stack can overlap. O(d log n) for d pending inserts.
    fn shadowed_inferred(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        let newly_asserted = self
            .asserted
            .inserts
            .iter()
            .filter(|quad| self.delta_contains(Stack::Inferred, quad));
        let newly_inferred = self.inferred.inserts.iter().filter(|quad| {
            !self.asserted.inserts.contains(*quad) && self.delta_contains(Stack::Asserted, quad)
        });
        newly_asserted.chain(newly_inferred).copied()
    }

    /// The state this transaction started from, without its pending changes.
    pub fn base(&self) -> &Snapshot {
        &self.base
    }

    /// The state the transaction would commit, as a snapshot: the base with the pending
    /// changes of each stack as one more run, as a commit publishes them, but not
    /// published. It sees the terms the transaction interned. Costs O(d log d) for d
    /// pending changes.
    pub fn pending_snapshot(&self) -> Snapshot {
        let shadowed: HashSet<EncodedQuad> = self.shadowed_inferred().collect();
        let asserted_inserts: Vec<EncodedQuad> = self.asserted.inserts.iter().copied().collect();
        let asserted_deletes: Vec<EncodedQuad> = self.asserted.deletes.iter().copied().collect();
        // A statement asserted now leaves the inferred stack (as `take_over_inferred`).
        let inferred_inserts: Vec<EncodedQuad> = self
            .inferred
            .inserts
            .iter()
            .filter(|quad| !shadowed.contains(*quad))
            .copied()
            .collect();
        let mut inferred_deletes: Vec<EncodedQuad> =
            self.inferred.deletes.iter().copied().collect();
        inferred_deletes.extend(shadowed.iter().filter(|quad| {
            !self.inferred.inserts.contains(*quad)
                && !self.inferred.deletes.contains(*quad)
                && self.base.stack_contains(Stack::Inferred, quad)
        }));
        let base = self.base.version();
        let run = |stack: Stack, inserts: &[EncodedQuad], deletes: &[EncodedQuad]| {
            crate::index::run::Run::from_delta(stack.layout(), inserts, deletes)
        };
        let version = Version {
            asserted: base.asserted.with_run(run(
                Stack::Asserted,
                &asserted_inserts,
                &asserted_deletes,
            )),
            inferred: base.inferred.with_run(run(
                Stack::Inferred,
                &inferred_inserts,
                &inferred_deletes,
            )),
            revision: base.revision,
            dictionary_len: self.engine.shared.dictionary.len(),
        };
        self.base.with_version(version)
    }

    /// Number of asserted and inferred quads, including pending changes.
    pub fn len(&self) -> u64 {
        self.len_in(ReadModel::Materialised)
    }

    /// Number of quads visible in `model`, including pending changes. O(1) for the asserted
    /// stack; the inferred count costs O(d log n) for d pending inserts.
    pub fn len_in(&self, model: ReadModel) -> u64 {
        Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack))
            .map(|stack| {
                let base = self.base.len_in(stack.model());
                let shadowed = match stack {
                    Stack::Asserted => 0,
                    Stack::Inferred => self.shadowed_inferred().count() as u64,
                };
                base.checked_add_signed(self.delta(stack).net())
                    .and_then(|len| len.checked_sub(shadowed))
                    .expect("exact deltas keep counts non-negative")
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pending asserted (inserted, deleted) counts.
    pub fn pending(&self) -> (usize, usize) {
        self.asserted.counts()
    }

    /// Pending asserted inserts: quads absent from the base that the commit would add.
    pub fn inserted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.asserted.inserts.iter().copied()
    }

    /// Pending asserted deletes: quads present in the base that the commit would remove.
    pub fn deleted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.asserted.deletes.iter().copied()
    }

    /// Pending inferred (inserted, deleted) counts, before the commit drops inferred quads
    /// that end up asserted.
    pub fn inferred_pending(&self) -> (usize, usize) {
        self.inferred.counts()
    }

    pub fn contains(&self, quad: &EncodedQuad) -> bool {
        self.contains_in(ReadModel::Materialised, quad)
    }

    pub fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool {
        Stack::ALL
            .into_iter()
            .any(|stack| model.includes(stack) && self.stack_contains(stack, quad))
    }

    /// Asserted and inferred quads matching `pattern`, including pending changes.
    pub fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        self.quads_for_pattern_in(ReadModel::Materialised, pattern)
    }

    /// Quads matching `pattern` in `model`, including pending changes. Per stack, base quads
    /// come first, in index order, followed by pending inserts in no particular order.
    pub fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'e> {
        let pattern = *pattern;
        Stack::ALL
            .into_iter()
            .filter(move |&stack| model.includes(stack))
            .flat_map(move |stack| {
                let delta = self.delta(stack);
                let inferred = stack == Stack::Inferred;
                // Base inferred quads can only be shadowed by pending asserted inserts (the
                // base stacks are disjoint): a hash probe instead of an index lookup.
                let base = self.base.stack_quads(stack, &pattern).filter(move |quad| {
                    let shadowed = inferred && self.asserted.inserts.contains(quad);
                    !shadowed && !delta.deletes.contains(quad)
                });
                let pending = delta.inserts.iter().copied().filter(move |quad| {
                    let shadowed = inferred && self.delta_contains(Stack::Asserted, quad);
                    !shadowed && pattern.matches(quad)
                });
                base.chain(pending)
            })
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
            .chain(self.asserted.inserts.iter().map(|quad| quad.graph))
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

    pub fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.engine.shared.dictionary.decode_quad(quad)
    }

    pub fn decode(&self, id: TermId) -> Option<Term> {
        self.engine.shared.dictionary.decode(id)
    }

    /// Asserts `quad`; returns `false` if it was already asserted.
    pub fn insert(&mut self, quad: QuadRef<'_>) -> bool {
        let encoded = self.engine.shared.dictionary.intern_quad(quad);
        self.insert_encoded(encoded)
    }

    /// Retracts the asserted `quad`; returns `false` if it was not asserted. Never interns.
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

    /// Asserts an already-encoded quad. Its ids must come from this engine's dictionary.
    /// Returns `false` if it was already asserted; asserting an inferred quad returns `true`
    /// (it becomes explicit).
    pub fn insert_encoded(&mut self, quad: EncodedQuad) -> bool {
        let base = &self.base;
        self.asserted
            .insert(quad, || base.stack_contains(Stack::Asserted, &quad))
    }

    /// Retracts an asserted quad; inferred quads are unaffected.
    pub fn remove_encoded(&mut self, quad: EncodedQuad) -> bool {
        let base = &self.base;
        self.asserted
            .remove(quad, || base.stack_contains(Stack::Asserted, &quad))
    }

    /// Retracts every asserted quad matching `pattern` (SPARQL `CLEAR`/`DROP`, GSP
    /// `PUT`/`DELETE`). Returns the number of quads removed.
    pub fn remove_matching(&mut self, pattern: &QuadPattern) -> u64 {
        let matching: Vec<_> = self
            .quads_for_pattern_in(ReadModel::Asserted, pattern)
            .collect();
        matching
            .into_iter()
            .map(|quad| u64::from(self.remove_encoded(quad)))
            .sum()
    }

    /// Adds an inferred statement (in the default graph). Returns `false` if it is already
    /// inferred, or asserted (asserted statements are never also inferred). Reasoner only.
    pub fn insert_inferred(&mut self, triple: EncodedTriple) -> bool {
        let quad = triple.in_default_graph();
        if self.stack_contains(Stack::Asserted, &quad) {
            return false;
        }
        let base = &self.base;
        self.inferred
            .insert(quad, || base.stack_contains(Stack::Inferred, &quad))
    }

    /// Removes an inferred statement; returns `false` if it was not visible as inferred.
    /// Reasoner only.
    pub fn remove_inferred(&mut self, triple: EncodedTriple) -> bool {
        let quad = triple.in_default_graph();
        if !self.stack_contains(Stack::Inferred, &quad) {
            return false;
        }
        let base = &self.base;
        self.inferred
            .remove(quad, || base.stack_contains(Stack::Inferred, &quad))
    }

    /// Pending inferred inserts.
    pub fn inferred_inserted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        self.inferred.inserts.iter().map(|&quad| quad.into())
    }

    /// Pending inferred deletes.
    pub fn inferred_deleted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        self.inferred.deletes.iter().map(|&quad| quad.into())
    }

    /// Enforces disjointness: inferred quads that end up asserted leave the inferred stack.
    fn take_over_inferred(&mut self) {
        let shadowed: Vec<_> = self.shadowed_inferred().collect();
        let base = &self.base;
        for quad in shadowed {
            self.inferred
                .remove(quad, || base.stack_contains(Stack::Inferred, &quad));
        }
    }

    /// Publishes the pending changes of both stacks as one new revision. In durable mode the
    /// change is in the WAL (and synced, per policy) before it becomes visible. Cost
    /// O(d log d) for a delta of d quads plus bounded inline compaction; independent of the
    /// dataset size.
    ///
    /// On error nothing is published and the transaction is discarded.
    pub fn commit(mut self) -> EngineResult<CommitSummary> {
        self.take_over_inferred();
        let Self {
            engine,
            _slot,
            base,
            asserted,
            inferred,
        } = self;
        let count = |set: &HashSet<EncodedQuad>| set.len() as u64;
        let summary = |revision| CommitSummary {
            revision,
            inserted: count(&asserted.inserts),
            deleted: count(&asserted.deletes),
            inferred_inserted: count(&inferred.inserts),
            inferred_deleted: count(&inferred.deletes),
        };
        if asserted.counts() == (0, 0) && inferred.counts() == (0, 0) {
            return Ok(summary(base.revision()));
        }
        let summary = summary(base.revision() + 1);
        let shared = &engine.shared;
        let dictionary_len = shared.dictionary.len();
        let dictionary_start = base.dictionary_len();
        let triples = |set: HashSet<EncodedQuad>| set.into_iter().map(Into::into).collect();
        let record = CommitRecord {
            revision: summary.revision,
            dictionary_start,
            // Every term interned since the last commit, including by aborted transactions,
            // so the logged dictionary stays one contiguous id range.
            keys: match shared.durable {
                Some(_) => shared
                    .dictionary
                    .export_keys(dictionary_start, dictionary_len),
                None => Vec::new(),
            },
            inserts: asserted.inserts.into_iter().collect(),
            deletes: asserted.deletes.into_iter().collect(),
            inferred_inserts: triples(inferred.inserts),
            inferred_deletes: triples(inferred.deletes),
        };
        let [asserted_run, inferred_run] = record.runs();
        let revision = record.revision;
        let publish = move |current: &Version| {
            debug_assert_eq!(current.revision + 1, revision, "single writer");
            Version {
                asserted: current.asserted.with_run(asserted_run),
                inferred: current.inferred.with_run(inferred_run),
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
        Ok(summary)
    }
}
