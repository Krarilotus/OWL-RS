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
//! - **Growth charged where reserved** (SYN-54 review r1, F1; DUR, 8 Nov): one budget, attached
//!   once for life ([`Pending::charge_to`]), pays for the asserted inserts' table: a reserve
//!   ([`Pending::try_reserve_inserts`]) charges the grown table before it is allocated, beside
//!   the old one while both live, and the charge is then exactly the table's allocation. It is
//!   held as long as the sets are: dropped with them, or handed through [`Pending::into_parts`]
//!   to the commit, which frees the sets first. Inserts past the reserved room grow the table
//!   uncharged, as every insert did before; the next reserve charges what they grew.

use std::sync::Arc;

use hashbrown::HashSet;
use nrese_exec::{Budget, Reservation};

use crate::engine::{ReadModel, Snapshot, Stack};
use crate::error::{EngineError, EngineResult};
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

    /// Room for `more` inserts, the grown table charged to `charge` before it is allocated:
    /// the bound of the table hashbrown grows to (`max(len + more, full capacity + 1)`; the full
    /// capacity is at most the allocation over an entry's size) beside what is held, then the
    /// charge trimmed to the new allocation. Refused, nothing changes.
    fn reserve_inserts(&mut self, more: usize, charge: &mut Reservation) -> EngineResult<()> {
        let too_large = || EngineError::Configuration(format!("no room for {more} more inserts"));
        let table = &mut self.inserts;
        let wanted = table.len().checked_add(more).ok_or_else(too_large)?;
        let held = charge.bytes().max(table.allocation_size());
        if table.capacity() >= wanted {
            // Room already: the charge covers the table as it is.
            charge.resize(held)?;
            return Ok(());
        }
        let full = table.allocation_size() / size_of::<EncodedQuad>();
        let grown = wanted.max(full + 1);
        let bound =
            nrese_exec::charged_map::table_bound::<EncodedQuad>(grown).ok_or_else(too_large)?;
        // The new table beside the old one, as the rehash holds both.
        charge.resize(held.checked_add(bound).ok_or_else(too_large)?)?;
        match table.try_reserve(more) {
            Ok(()) => {}
            Err(hashbrown::TryReserveError::AllocError { layout }) => {
                std::alloc::handle_alloc_error(layout)
            }
            Err(hashbrown::TryReserveError::CapacityOverflow) => {
                unreachable!("a table within its bound has a layout")
            }
        }
        let allocated = table.allocation_size();
        debug_assert!(
            allocated <= bound,
            "{allocated} bytes past the bound {bound}"
        );
        charge.resize(allocated)?;
        Ok(())
    }
}

/// What pays for the pending changes' growth: one budget for life, and what it holds.
struct Charge {
    budget: Arc<Budget>,
    held: Reservation,
}

/// The pending changes of both stacks over one base snapshot, held for life.
pub(in crate::engine) struct Pending {
    base: Snapshot,
    asserted: Delta,
    inferred: Delta,
    /// After the deltas: fields drop in order, the sets freed before their charge goes.
    charge: Option<Charge>,
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
            charge: None,
        }
    }

    /// Pays for the pending changes' growth from `budget`, for their life. Attaching the
    /// budget they have again changes nothing; another one is refused
    /// ([`EngineError::Configuration`]): a charge never moves between budgets.
    pub(in crate::engine) fn charge_to(&mut self, budget: Arc<Budget>) -> EngineResult<()> {
        match &self.charge {
            None => {
                self.charge = Some(Charge {
                    held: Reservation::new(Arc::clone(&budget)),
                    budget,
                });
                Ok(())
            }
            Some(charge) if Arc::ptr_eq(&charge.budget, &budget) => Ok(()),
            Some(_) => Err(EngineError::Configuration(
                "the pending changes are already charged to another budget".to_owned(),
            )),
        }
    }

    /// Whether a budget is attached ([`Self::charge_to`]).
    pub(in crate::engine) fn charged(&self) -> bool {
        self.charge.is_some()
    }

    /// Room for `more` asserted inserts, charged to the attached budget before the table
    /// grows: the next `more` inserts allocate nothing. Refused
    /// ([`EngineError::MemoryLimit`]), nothing changes; without a budget attached
    /// ([`Self::charge_to`]) it is an error, never an uncharged success.
    pub(in crate::engine) fn try_reserve_inserts(&mut self, more: usize) -> EngineResult<()> {
        let Some(charge) = &mut self.charge else {
            return Err(EngineError::Configuration(
                "no budget is attached to the pending changes".to_owned(),
            ));
        };
        self.asserted.reserve_inserts(more, &mut charge.held)
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
    /// a record, and their charge: first, so a commit that binds the parts in this order drops
    /// it last, after the sets.
    pub(super) fn into_parts(self) -> (Option<Reservation>, Snapshot, Delta, Delta) {
        let Self {
            base,
            asserted,
            inferred,
            charge,
        } = self;
        (charge.map(|charge| charge.held), base, asserted, inferred)
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

/// The charge of the pending changes' growth (module docs; DUR's conditions, 8 Nov): a refused
/// reserve changes nothing; reserved room takes its inserts without growing, removals between
/// them included; the charge is held while the sets live and is gone after a drop and after a
/// commit; one budget for life; no reserve without one.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::TermKind;
    use crate::{Engine, EngineConfig};

    fn quad(i: u64) -> EncodedQuad {
        let iri = |n| TermId::new(TermKind::Iri, n);
        EncodedQuad::new(iri(i + 10), iri(1), iri(2), TermId::DEFAULT_GRAPH)
    }

    fn pending(engine: &Engine, budget: &Arc<Budget>) -> Pending {
        let mut pending = Pending::over(engine.snapshot());
        pending.charge_to(Arc::clone(budget)).unwrap();
        pending
    }

    #[test]
    fn a_refused_reserve_changes_nothing() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let budget = Arc::new(Budget::new(64 * 1024));
        let mut pending = pending(&engine, &budget);
        pending.try_reserve_inserts(100).unwrap();
        for i in 0..100 {
            pending.insert_asserted(quad(i));
        }
        let before = (
            pending.asserted.inserts.len(),
            pending.asserted.inserts.capacity(),
            budget.used(),
        );
        assert!(before.2 > 0, "the reserved table is charged");
        let refused = pending.try_reserve_inserts(1 << 20);
        assert!(
            matches!(refused, Err(EngineError::MemoryLimit(_))),
            "{refused:?}"
        );
        let after = (
            pending.asserted.inserts.len(),
            pending.asserted.inserts.capacity(),
            budget.used(),
        );
        assert_eq!(after, before, "length, capacity and charge as they were");
    }

    #[test]
    fn reserved_room_takes_its_inserts_without_growing() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let budget = Arc::new(Budget::unlimited());
        let mut pending = pending(&engine, &budget);
        pending.try_reserve_inserts(1000).unwrap();
        let capacity = pending.asserted.inserts.capacity();
        let charged = budget.used();
        assert!(capacity >= 1000);
        assert_eq!(charged, pending.asserted.inserts.allocation_size());
        // 600 inserts, 300 of them undone, then 400 more: 1000 inserts in all.
        for i in 0..600 {
            assert!(pending.insert_asserted(quad(i)));
        }
        for i in 0..300 {
            assert!(pending.remove_asserted(quad(i)));
        }
        for i in 600..1000 {
            assert!(pending.insert_asserted(quad(i)));
        }
        assert_eq!(pending.asserted.inserts.capacity(), capacity, "no growth");
        assert_eq!(
            budget.used(),
            charged,
            "the charge is the table's allocation"
        );
    }

    #[test]
    fn the_charge_lives_as_long_as_the_pending_changes() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let budget = Arc::new(Budget::unlimited());
        let mut dropped = pending(&engine, &budget);
        dropped.try_reserve_inserts(1000).unwrap();
        assert!(budget.used() > 0);
        drop(dropped);
        assert_eq!(
            budget.used(),
            0,
            "a dropped transaction's charge is released"
        );

        let mut tx = engine.transaction();
        assert!(!tx.charges_pending());
        tx.charge_pending_to(Arc::clone(&budget)).unwrap();
        assert!(tx.charges_pending());
        tx.try_reserve_inserts(1000).unwrap();
        for i in 0..1000 {
            let iri = |name: String| nrese_rdf::NamedNode::new_unchecked(name);
            let statement = nrese_rdf::Quad::new(
                iri(format!("http://example.com/s{i}")),
                iri("http://example.com/p".to_owned()),
                iri("http://example.com/o".to_owned()),
                nrese_rdf::GraphName::DefaultGraph,
            );
            tx.insert(statement.as_ref());
        }
        assert!(budget.used() >= 1000 * size_of::<EncodedQuad>());
        tx.commit().unwrap();
        assert_eq!(budget.used(), 0, "a commit releases the charge");
        assert_eq!(engine.snapshot().len_in(ReadModel::Asserted), 1000);
    }

    #[test]
    fn one_budget_for_life_and_none_means_no_reserve() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut unattached = Pending::over(engine.snapshot());
        assert!(matches!(
            unattached.try_reserve_inserts(10),
            Err(EngineError::Configuration(_))
        ));
        let (first, second) = (Arc::new(Budget::unlimited()), Arc::new(Budget::unlimited()));
        let mut pending = pending(&engine, &first);
        assert!(
            pending.charge_to(Arc::clone(&first)).is_ok(),
            "the same budget again"
        );
        assert!(matches!(
            pending.charge_to(Arc::clone(&second)),
            Err(EngineError::Configuration(_))
        ));
        pending.try_reserve_inserts(10).unwrap();
        assert_eq!(second.used(), 0, "nothing is charged to a refused budget");
        assert!(first.used() > 0);
    }
}
