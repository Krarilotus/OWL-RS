//! A transaction's pending changes over its base snapshot: one exact delta per stack, and the
//! reads that see the base plus them. Shared by the write transaction ([`super::Transaction`])
//! and, from the dictionary lifecycle's package 2b, a private one.
//!
//! What it keeps (the transaction owner's conditions, 8 Nov):
//! - **One base for life.** The deltas are exact against the snapshot they were made over, the
//!   one this holds from construction; nothing reads or extends them against another. For a
//!   write transaction that is also the WAL's replay contract: its record is the change from
//!   its base, the version before it.
//! - **Disjointness on the final state.** No quad is both asserted and inferred: an inferred
//!   quad is visible only while it isn't asserted, pending changes included
//!   ([`Pending::stack_contains`]), and those that end up asserted ([`Pending::shadowed_inferred`])
//!   leave the inferred stack. Both pending-snapshot builders take that set from here.
//! - **No way to publication.** It is a value; only the write transaction turns its deltas into a
//!   commit record ([`Pending::into_parts`]). That is by visibility: this module is the write
//!   transaction's child, and what is `pub(super)` here, `into_parts`, the deltas' sets and the
//!   takeover among it, is the write transaction's alone. What the private transaction
//!   (`super::super::private`) reads and changes is `pub(in crate::engine)`.

use hashbrown::HashSet;

use crate::engine::{ReadModel, Snapshot, Stack};
use crate::quad::{EncodedQuad, QuadPattern};
use crate::term::TermId;

/// Pending changes to one stack, kept *exact* against the base snapshot: `inserts` are
/// absent from the base, `deletes` are present in it, and the two sets are disjoint. That
/// invariant is what the index's sign-sum visibility rule relies on.
#[derive(Default)]
pub(in crate::engine) struct Delta {
    pub(super) inserts: HashSet<EncodedQuad>,
    pub(super) deletes: HashSet<EncodedQuad>,
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

    pub(in crate::engine) fn counts(&self) -> (usize, usize) {
        (self.inserts.len(), self.deletes.len())
    }

    /// No net change: what a commit publishes no run for, and, for the asserted stack,
    /// keeps the asserted identity for ([`super::Transaction::asserted_identity_after`]).
    pub(in crate::engine) fn is_empty(&self) -> bool {
        self.counts() == (0, 0)
    }

    /// The pending inserts, read only.
    pub(in crate::engine) fn inserted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.inserts.iter().copied()
    }

    /// The pending deletes, read only.
    pub(in crate::engine) fn deleted(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
        self.deletes.iter().copied()
    }

    fn net(&self) -> i64 {
        self.inserts.len() as i64 - self.deletes.len() as i64
    }
}

/// The pending changes of both stacks over one base snapshot, held for life.
pub(in crate::engine) struct Pending {
    base: Snapshot,
    asserted: Delta,
    inferred: Delta,
}

/// [`Pending::snapshot_changes`]: the changes a pending snapshot of the whole base applies, each
/// stack's inserts and deletes, disjointness applied.
pub(in crate::engine) struct SnapshotChanges {
    pub(in crate::engine) asserted_inserts: Vec<EncodedQuad>,
    pub(in crate::engine) asserted_deletes: Vec<EncodedQuad>,
    pub(in crate::engine) inferred_inserts: Vec<EncodedQuad>,
    pub(in crate::engine) inferred_deletes: Vec<EncodedQuad>,
}

impl Pending {
    /// No changes yet, over `base`, which these changes are exact against for their life.
    pub(in crate::engine) fn over(base: Snapshot) -> Self {
        Self {
            base,
            asserted: Delta::default(),
            inferred: Delta::default(),
        }
    }

    pub(in crate::engine) fn base(&self) -> &Snapshot {
        &self.base
    }

    pub(in crate::engine) fn asserted(&self) -> &Delta {
        &self.asserted
    }

    pub(in crate::engine) fn inferred(&self) -> &Delta {
        &self.inferred
    }

    /// The base and both deltas, for the write transaction's commit, which alone turns them into
    /// a record.
    pub(super) fn into_parts(self) -> (Snapshot, Delta, Delta) {
        (self.base, self.asserted, self.inferred)
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
    pub(super) fn stack_contains(&self, stack: Stack, quad: &EncodedQuad) -> bool {
        self.delta_contains(stack, quad)
            && (stack == Stack::Asserted || !self.delta_contains(Stack::Asserted, quad))
    }

    /// Inferred quads that are also asserted after the pending changes; the commit drops them
    /// from the inferred stack. The base stacks are disjoint, so only pending inserts of
    /// either stack can overlap. O(d log n) for d pending inserts.
    pub(super) fn shadowed_inferred(&self) -> impl Iterator<Item = EncodedQuad> + '_ {
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

    /// The changes a pending snapshot of the whole base applies, disjointness applied: a
    /// statement asserted now leaves the inferred stack (as [`Self::take_over_inferred`]).
    pub(in crate::engine) fn snapshot_changes(&self) -> SnapshotChanges {
        let shadowed: HashSet<EncodedQuad> = self.shadowed_inferred().collect();
        let asserted_inserts: Vec<EncodedQuad> = self.asserted.inserts.iter().copied().collect();
        let asserted_deletes: Vec<EncodedQuad> = self.asserted.deletes.iter().copied().collect();
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
        SnapshotChanges {
            asserted_inserts,
            asserted_deletes,
            inferred_inserts,
            inferred_deletes,
        }
    }

    /// The inferred statements of `inferred_view` (a restricted view of the base) that a pending
    /// view over it drops: those the pending changes delete or assert, and `hidden`. Sorted,
    /// each once.
    pub(in crate::engine) fn over_gone(
        &self,
        inferred_view: &crate::index::IndexVersion,
        hidden: &[EncodedQuad],
    ) -> Vec<EncodedQuad> {
        let mut gone: Vec<EncodedQuad> = self
            .inferred
            .deletes
            .iter()
            .chain(self.asserted.inserts.iter())
            .chain(hidden)
            .filter(|quad| inferred_view.contains(quad))
            .copied()
            .collect();
        gone.sort_unstable();
        gone.dedup();
        gone
    }

    /// Whether any pending change, of either stack, has `same_as` as its predicate.
    pub(super) fn touches_predicate(&self, same_as: TermId) -> bool {
        [
            &self.asserted.inserts,
            &self.asserted.deletes,
            &self.inferred.inserts,
            &self.inferred.deletes,
        ]
        .into_iter()
        .any(|set| set.iter().any(|quad| quad.predicate == same_as))
    }

    /// Number of quads visible in `model`, including pending changes. O(1) for the asserted
    /// stack; the inferred count costs O(d log n) for d pending inserts.
    pub(in crate::engine) fn len_in(&self, model: ReadModel) -> u64 {
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

    pub(in crate::engine) fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool {
        Stack::ALL
            .into_iter()
            .any(|stack| model.includes(stack) && self.stack_contains(stack, quad))
    }

    /// Quads matching `pattern` in `model`, including pending changes. Per stack, base quads
    /// come first, in index order, followed by pending inserts in no particular order.
    pub(in crate::engine) fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
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
    pub(in crate::engine) fn contains_named_graph(&self, graph: TermId) -> bool {
        !graph.is_default_graph()
            && self
                .quads_for_pattern_in(ReadModel::Materialised, &QuadPattern::in_graph(graph))
                .next()
                .is_some()
    }

    /// Named graphs that are non-empty including pending changes, in id order.
    pub(in crate::engine) fn named_graphs(&self) -> Vec<TermId> {
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

    /// Asserts `quad` (its ids from the base's dictionary or its transaction's vocabulary).
    /// Returns whether it changed anything: `false` if it was already asserted.
    pub(in crate::engine) fn insert_asserted(&mut self, quad: EncodedQuad) -> bool {
        let base = &self.base;
        self.asserted
            .insert(quad, || base.stack_contains(Stack::Asserted, &quad))
    }

    /// Retracts an asserted quad; returns whether it changed anything. Inferred quads are
    /// unaffected.
    pub(in crate::engine) fn remove_asserted(&mut self, quad: EncodedQuad) -> bool {
        let base = &self.base;
        self.asserted
            .remove(quad, || base.stack_contains(Stack::Asserted, &quad))
    }

    /// Adds an inferred quad: `false` if it is already inferred, or asserted.
    pub(in crate::engine) fn insert_inferred(&mut self, quad: EncodedQuad) -> bool {
        if self.stack_contains(Stack::Asserted, &quad) {
            return false;
        }
        let base = &self.base;
        self.inferred
            .insert(quad, || base.stack_contains(Stack::Inferred, &quad))
    }

    /// Removes an inferred quad: `false` if it was not visible as inferred.
    pub(in crate::engine) fn remove_inferred(&mut self, quad: EncodedQuad) -> bool {
        if !self.stack_contains(Stack::Inferred, &quad) {
            return false;
        }
        let base = &self.base;
        self.inferred
            .remove(quad, || base.stack_contains(Stack::Inferred, &quad))
    }

    /// Enforces disjointness: inferred quads that end up asserted leave the inferred stack.
    pub(super) fn take_over_inferred(&mut self) {
        let shadowed: Vec<_> = self.shadowed_inferred().collect();
        let base = &self.base;
        for quad in shadowed {
            self.inferred
                .remove(quad, || base.stack_contains(Stack::Inferred, &quad));
        }
    }
}
