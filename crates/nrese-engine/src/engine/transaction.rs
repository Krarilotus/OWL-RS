//! Write transactions: exact deltas over a base snapshot, published atomically on commit.

use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use hashbrown::HashSet;
use nrese_rdf::{Quad, QuadRef, Term, TermRef};
use parking_lot::MutexGuard;

use super::pending::{Pending, SnapshotChanges};
use super::{Inner, ReadModel, Snapshot, Version};
use crate::durability::codec::CommitRecord;
use crate::error::EngineResult;
use crate::index::Layout;
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
    /// The asserted state after it ([`Snapshot::asserted_identity`](crate::Snapshot::asserted_identity)):
    /// new where it changed asserted statements, the base's otherwise. A caller that
    /// keeps state by it learns here what the commit published, never from a later read.
    pub asserted: super::AssertedIdentity,
}

/// A write transaction holding the engine's single writer slot.
///
/// It keeps one exact delta per stack, in its `Pending` changes. Asserted quads are written
/// through the plain methods ([`insert`](Self::insert), [`remove`](Self::remove), …); inferred
/// ones only through the `*_inferred` methods, which the reasoner uses and request handlers
/// must not.
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
    /// The writer slot; `None` for a speculative transaction ([`crate::Engine::speculative`]),
    /// which never commits.
    _slot: Option<MutexGuard<'e, ()>>,
    /// The base snapshot and the pending changes over it.
    pending: Pending,
    /// What [`Self::asserted_identity_after`] answered first, which the commit publishes.
    after: OnceLock<super::AssertedIdentity>,
}

impl<'e> Transaction<'e> {
    pub(super) fn new(engine: &'e Inner, slot: Option<MutexGuard<'e, ()>>) -> Self {
        Self {
            pending: Pending::over(engine.shared.snapshot()),
            engine,
            _slot: slot,
            after: OnceLock::new(),
        }
    }

    /// The asserted identity the commit will publish
    /// ([`CommitSummary::asserted`]): a new one, reserved here, where the pending changes
    /// change asserted statements, the base's otherwise (an inferred-only change, none, or
    /// inserts undone by removes). A caller can key state by it before the commit makes
    /// it visible. The same on every call. It holds only if no asserted change follows the
    /// call (debug builds check that, and the commit's id against it); a commit that fails
    /// or never happens leaves the id to no snapshot. Never persisted: a reopen gives new
    /// ones.
    pub fn asserted_identity_after(&self) -> super::AssertedIdentity {
        *self
            .after
            .get_or_init(|| match self.pending.asserted().is_empty() {
                true => self.pending.base().asserted_identity(),
                false => super::AssertedIdentity::fresh(),
            })
    }

    /// Debug builds: no asserted change follows [`Self::asserted_identity_after`].
    fn assert_unanswered(&self, changed: bool) -> bool {
        debug_assert!(
            !changed || self.after.get().is_none(),
            "an asserted change after asserted_identity_after"
        );
        changed
    }

    /// The state this transaction started from, without its pending changes.
    pub fn base(&self) -> &Snapshot {
        self.pending.base()
    }

    /// The state the transaction would commit, as a snapshot: the base with the pending
    /// changes of each stack as one more run, as a commit publishes them, but not
    /// published. It sees the terms the transaction interned. Costs O(d log d) for d
    /// pending changes.
    pub fn pending_snapshot(&self) -> Snapshot {
        // A statement asserted now leaves the inferred stack (as `take_over_inferred`).
        let SnapshotChanges {
            asserted_inserts,
            asserted_deletes,
            inferred_inserts,
            inferred_deletes,
        } = self.pending.snapshot_changes();
        let base = self.pending.base().version();
        let content = super::Content::fresh();
        if let Some(corpora) = self.engine.shared.statistics.corpora.get() {
            let changed = [
                &asserted_inserts,
                &asserted_deletes,
                &inferred_inserts,
                &inferred_deletes,
            ];
            corpora.record(
                base.content,
                content,
                changed
                    .into_iter()
                    .flatten()
                    .map(|quad| (quad.subject, quad.predicate, quad.object)),
            );
        }
        let version = Version {
            content,
            // Pending: its own, even without asserted changes (staged terms).
            asserted_content: super::AssertedIdentity::fresh(),
            asserted: base
                .asserted
                .with_delta(&asserted_inserts, &asserted_deletes),
            inferred: base
                .inferred
                .with_delta(&inferred_inserts, &inferred_deletes),
            revision: base.revision,
            committed_micros: base.committed_micros,
            dictionary_len: self.engine.shared.dictionary.len(),
            equality: match self.changes_equality() {
                true => Default::default(),
                false => std::sync::Arc::clone(&base.equality),
            },
            canonical: Default::default(),
        };
        self.pending.base().with_version(version)
    }

    /// [`Self::pending_snapshot`] as a reader of `view` reads it: `view` is a restricted
    /// snapshot of this transaction's base (its asserted statements, a subset of its
    /// inferred ones and statements added to them: [`Snapshot::with_inferred_only`],
    /// [`Snapshot::with_inferred_mask`]). The result is `view` with the pending asserted
    /// changes, and without the inferred statements the pending changes delete, assert, or
    /// that `hidden` names. The pending inferred inserts aren't in it: only the reasoner
    /// writes them, for the commit. O(d log d + h) for d pending changes and h hidden
    /// statements: the view's runs are shared, never copied or scanned.
    pub fn pending_over(&self, view: &Snapshot, hidden: &[EncodedQuad]) -> Snapshot {
        let seen = view.version();
        let gone = self.pending.over_gone(&seen.inferred, hidden);
        let asserted = self.pending.asserted();
        let asserted_inserts: Vec<EncodedQuad> = asserted.inserts.iter().copied().collect();
        let asserted_deletes: Vec<EncodedQuad> = asserted.deletes.iter().copied().collect();
        let content = super::Content::fresh();
        if let Some(corpora) = self.engine.shared.statistics.corpora.get() {
            corpora.record(
                seen.content,
                content,
                [&asserted_inserts, &asserted_deletes, &gone]
                    .into_iter()
                    .flatten()
                    .map(|quad| (quad.subject, quad.predicate, quad.object)),
            );
        }
        let version = Version {
            content,
            // Pending: its own, even without asserted changes (staged terms).
            asserted_content: super::AssertedIdentity::fresh(),
            asserted: seen
                .asserted
                .with_delta(&asserted_inserts, &asserted_deletes),
            inferred: seen.inferred.with_delta(&[], &gone),
            revision: seen.revision,
            committed_micros: seen.committed_micros,
            dictionary_len: self.engine.shared.dictionary.len(),
            equality: match self.changes_equality() {
                true => Default::default(),
                false => std::sync::Arc::clone(&seen.equality),
            },
            canonical: Default::default(),
        };
        view.with_version(version)
    }

    /// Whether the pending changes touch an `owl:sameAs` statement while reads expand
    /// equality classes: then the next version computes its classes anew.
    fn changes_equality(&self) -> bool {
        let Some(same_as) = *self.engine.shared.equality.read() else {
            return false;
        };
        self.pending.touches_predicate(same_as)
    }

    /// Number of asserted and inferred quads, including pending changes.
    pub fn len(&self) -> u64 {
        self.len_in(ReadModel::Materialised)
    }

    /// Number of quads visible in `model`, including pending changes. O(1) for the asserted
    /// stack; the inferred count costs O(d log n) for d pending inserts.
    pub fn len_in(&self, model: ReadModel) -> u64 {
        self.pending.len_in(model)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pending asserted (inserted, deleted) counts.
    pub fn pending(&self) -> (usize, usize) {
        self.pending.asserted().counts()
    }

    /// Pending asserted inserts: quads absent from the base that the commit would add.
    pub fn inserted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.pending.asserted().inserts.iter().copied()
    }

    /// Pending asserted deletes: quads present in the base that the commit would remove.
    pub fn deleted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.pending.asserted().deletes.iter().copied()
    }

    /// Pending inferred (inserted, deleted) counts, before the commit drops inferred quads
    /// that end up asserted.
    pub fn inferred_pending(&self) -> (usize, usize) {
        self.pending.inferred().counts()
    }

    pub fn contains(&self, quad: &EncodedQuad) -> bool {
        self.contains_in(ReadModel::Materialised, quad)
    }

    pub fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool {
        self.pending.contains_in(model, quad)
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
        self.pending.quads_for_pattern_in(model, pattern)
    }

    /// True if the named graph `graph` contains at least one quad, including pending changes.
    pub fn contains_named_graph(&self, graph: TermId) -> bool {
        self.pending.contains_named_graph(graph)
    }

    /// Named graphs that are non-empty including pending changes, in id order.
    pub fn named_graphs(&self) -> Vec<TermId> {
        self.pending.named_graphs()
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
        let changed = self.pending.insert_asserted(quad);
        self.assert_unanswered(changed)
    }

    /// Retracts an asserted quad; inferred quads are unaffected.
    pub fn remove_encoded(&mut self, quad: EncodedQuad) -> bool {
        let changed = self.pending.remove_asserted(quad);
        self.assert_unanswered(changed)
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
        self.pending.insert_inferred(triple.in_default_graph())
    }

    /// Removes an inferred statement; returns `false` if it was not visible as inferred.
    /// Reasoner only.
    pub fn remove_inferred(&mut self, triple: EncodedTriple) -> bool {
        self.pending.remove_inferred(triple.in_default_graph())
    }

    /// Pending inferred inserts.
    pub fn inferred_inserted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        self.pending
            .inferred()
            .inserts
            .iter()
            .map(|&quad| quad.into())
    }

    /// Pending inferred deletes.
    pub fn inferred_deleted(&self) -> impl Iterator<Item = EncodedTriple> + '_ {
        self.pending
            .inferred()
            .deletes
            .iter()
            .map(|&quad| quad.into())
    }

    /// Enforces disjointness: inferred quads that end up asserted leave the inferred stack.
    fn take_over_inferred(&mut self) {
        self.pending.take_over_inferred();
    }

    /// Publishes the pending changes of both stacks as one new revision. In durable mode the
    /// change is in the WAL (and synced, per policy) before it becomes visible. Cost
    /// O(d log d) for a delta of d quads plus bounded inline compaction; independent of the
    /// dataset size.
    ///
    /// On error nothing is published and the transaction is discarded.
    pub fn commit(mut self) -> EngineResult<CommitSummary> {
        if self._slot.is_none() {
            return Err(crate::EngineError::Speculative);
        }
        self.take_over_inferred();
        let changes_equality = self.changes_equality();
        // The id the commit publishes where it changes asserted statements: the one
        // reserved ([`Self::asserted_identity_after`]), or one chosen when published. Never
        // the base's: a change after an answer of "unchanged" still gets a new one.
        let reserved = self.after.get().copied().filter(|&after| {
            !self.pending.asserted().is_empty() && after != self.pending.base().asserted_identity()
        });
        let Self {
            engine,
            _slot,
            pending,
            after,
        } = self;
        let (base, asserted, inferred) = pending.into_parts();
        let promised = after.into_inner();
        let count = |set: &HashSet<EncodedQuad>| set.len() as u64;
        let summary = |revision, asserted_identity| CommitSummary {
            revision,
            asserted: asserted_identity,
            inserted: count(&asserted.inserts),
            deleted: count(&asserted.deletes),
            inferred_inserted: count(&inferred.inserts),
            inferred_deleted: count(&inferred.deletes),
        };
        let checked = |summary: CommitSummary| {
            debug_assert!(
                promised.is_none_or(|promised| promised == summary.asserted),
                "the commit publishes what asserted_identity_after said"
            );
            summary
        };
        if asserted.is_empty() && inferred.is_empty() {
            return Ok(checked(summary(base.revision(), base.asserted_identity())));
        }
        let mut summary = summary(base.revision() + 1, base.asserted_identity());
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
            // Stamped as it goes to the log ([`Inner::publish_record`]).
            committed_micros: None,
        };
        // Kept: the base's (single writer; a merge keeps it).
        if let Some(new) =
            engine.publish_record(&record, true, reserved, dictionary_len, changes_equality)?
        {
            summary.asserted = new;
        }
        Ok(checked(summary))
    }
}

impl Inner {
    /// Publishes `record` as the next revision, its terms already in the dictionary
    /// (`dictionary_len` of them): logged first in durable mode, then post-commit
    /// maintenance. The caller holds the writer slot: a commit, or a replicated record.
    /// Returns the published version's new [`AssertedIdentity`](super::AssertedIdentity),
    /// `None` where it keeps the previous one (no asserted change). A `local` commit may
    /// bring the new one, `reserved` ([`Transaction::asserted_identity_after`]); otherwise
    /// it is chosen here.
    ///
    /// A `local` commit is stamped by the store's clock ([`Shared::stamp`]) when its record
    /// goes to the log, its runs built and its layout converted, and the record and the
    /// version take that one time (review r1, R1-F1). A replicated record keeps the
    /// primary's time, which the clock takes too.
    pub(super) fn publish_record(
        &self,
        record: &CommitRecord,
        local: bool,
        reserved: Option<super::AssertedIdentity>,
        dictionary_len: u64,
        changes_equality: bool,
    ) -> EngineResult<Option<super::AssertedIdentity>> {
        // New where the record changes asserted statements; kept for an inferred-only one.
        // Chosen once, for the durable and the in-memory path alike.
        let changes_asserted = !record.inserts.is_empty() || !record.deletes.is_empty();
        debug_assert!(local || reserved.is_none(), "only a local commit reserves");
        debug_assert!(
            reserved.is_none() || changes_asserted,
            "the commit's delta and its record agree on an asserted change"
        );
        let asserted_identity =
            changes_asserted.then(|| reserved.unwrap_or_else(super::AssertedIdentity::fresh));
        let shared = &self.shared;
        // The first quad in a named graph turns a default-graph asserted stack into a quad
        // stack. No merge or mapped checkpoint may replace its runs between the conversion
        // and the publication: the writer slot excludes other commits, the compaction slot
        // both of those. It is converted first, in parallel without the slot, and kept where
        // the stack's runs are still the ones converted. The record's runs are built before
        // that, in the layout the stack will have (no holder of the compaction slot runs
        // parallel work; only commits, under the writer slot, change the layout).
        let latest = shared.versions.load();
        let converting = latest.asserted.layout() == Layout::DefaultGraph && record.names_a_graph();
        let asserted_layout = match converting {
            true => Layout::Quads,
            false => latest.asserted.layout(),
        };
        drop(latest);
        let [asserted_run, inferred_run] = record.runs(asserted_layout);
        let (_compaction, converted) = match converting {
            true => {
                let (slot, converted) = shared.versions.converted_asserted();
                (Some(slot), Some(converted))
            }
            false => (None, None),
        };
        debug_assert!(
            converted
                .as_ref()
                .is_none_or(|index| index.layout() == asserted_layout)
        );
        let revision = record.revision;
        // The contents published from and to, for reader-scoped searches' lineage
        // ([`super::corpora`]), recorded once the publication's locks are let go.
        let published = std::cell::Cell::new(None);
        let published_to = &published;
        let publish = move |current: &Version, committed: Option<u64>| {
            debug_assert_eq!(current.revision + 1, revision, "single writer");
            let asserted = converted.as_ref().unwrap_or(&current.asserted);
            let content = super::Content::fresh();
            published_to.set(Some((current.content, content)));
            Version {
                content,
                asserted_content: asserted_identity.unwrap_or(current.asserted_content),
                asserted: asserted.with_run(asserted_run),
                inferred: current.inferred.with_run(inferred_run),
                revision,
                // The latest of its commits: unknown stays unknown, and a replicated record
                // (a primary's log may step back) bounds what came before too.
                committed_micros: crate::durability::later(current.committed_micros, committed),
                dictionary_len,
                equality: match changes_equality {
                    true => Default::default(),
                    false => std::sync::Arc::clone(&current.equality),
                },
                canonical: Default::default(),
            }
        };
        let wal_bytes = match &shared.durable {
            Some(durable) => {
                let mut wal = durable.wal.lock();
                let mut stamp = || shared.stamp();
                let stamp: Option<&mut dyn FnMut() -> u64> = match local {
                    true => Some(&mut stamp),
                    false => None,
                };
                let committed = wal.append(record, stamp)?;
                if !local {
                    shared
                        .clock
                        .fetch_max(committed.unwrap_or(0), Ordering::Relaxed);
                }
                shared
                    .versions
                    .publish(|current| publish(current, committed));
                wal.bytes_since_checkpoint()
            }
            None => {
                let committed = match local {
                    true => Some(shared.stamp()),
                    false => {
                        shared
                            .clock
                            .fetch_max(record.committed_micros.unwrap_or(0), Ordering::Relaxed);
                        record.committed_micros
                    }
                };
                shared
                    .versions
                    .publish(|current| publish(current, committed));
                0
            }
        };
        // What the commit touched, where reader-scoped searches keep corpora: under the
        // writer slot (steps are recorded in commit order), after the publication's locks.
        // A search of the new version meanwhile finds no step and walks.
        if let (Some(corpora), Some((parent, child))) =
            (shared.statistics.corpora.get(), published.get())
        {
            let quads = record.inserts.iter().chain(&record.deletes);
            let triples = record
                .inferred_inserts
                .iter()
                .chain(&record.inferred_deletes);
            corpora.record(
                parent,
                child,
                quads
                    .map(|q| (q.subject, q.predicate, q.object))
                    .chain(triples.map(|t| (t.subject, t.predicate, t.object))),
            );
        }
        // After the commit's own compaction slot (a conversion's): compacting takes it.
        drop(_compaction);
        self.after_commit(wal_bytes);
        Ok(asserted_identity)
    }
}
