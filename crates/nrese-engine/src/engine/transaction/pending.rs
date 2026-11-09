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
//! - **Growth charged before it happens** (SYN-54 review r1, F1; DUR, 8 Nov; D16-039): one
//!   budget, attached once for life ([`Pending::charge_to`]), pays for both asserted tables, the
//!   inserts and the deletes. A table grows only after the grown table is charged, beside the
//!   old one while both live; the charge is then exactly the tables' allocation. That holds for
//!   a reserve ([`Pending::try_reserve_inserts`]) and for every charged change
//!   ([`Pending::try_insert_asserted`], [`Pending::try_remove_asserted`],
//!   [`Pending::try_remove_matching`]), which charge a table at its growth points only: a full
//!   table's next new entry. It is held as long as the sets are: dropped with them, or handed
//!   through [`Pending::into_parts`] to the commit, which frees the sets first. The uncharged
//!   changes ([`Pending::insert_asserted`], [`Pending::remove_asserted`]) are for a `Pending`
//!   no budget pays for (an engine user's own transaction, a test); on a charged one their
//!   growth is charged at the next charged change. The inferred stack's delta is the
//!   reasoner's, and no budget pays for it here.

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

    /// What both tables hold.
    fn allocation(&self) -> usize {
        self.inserts.allocation_size() + self.deletes.allocation_size()
    }

    /// Room for `more` entries in the `table`, the grown table charged to `charge` before it is
    /// allocated: the bound of the table hashbrown grows to (`max(len + more, full capacity +
    /// 1)`; the full capacity is at most the allocation over an entry's size) beside what both
    /// tables hold, then the charge trimmed to their new allocation. Refused, nothing changes.
    fn reserve(&mut self, table: Table, more: usize, charge: &mut Reservation) -> EngineResult<()> {
        let too_large = || EngineError::Configuration(format!("no room for {more} more changes"));
        let held = charge.bytes().max(self.allocation());
        let set = match table {
            Table::Inserts => &mut self.inserts,
            Table::Deletes => &mut self.deletes,
        };
        let wanted = set.len().checked_add(more).ok_or_else(too_large)?;
        if set.capacity() >= wanted {
            // Room already: the charge covers the tables as they are.
            charge.resize(held)?;
            return Ok(());
        }
        let full = set.allocation_size() / size_of::<EncodedQuad>();
        let grown = wanted.max(full + 1);
        let bound =
            nrese_exec::charged_map::table_bound::<EncodedQuad>(grown).ok_or_else(too_large)?;
        // The new table beside both, as the rehash holds the old one too.
        charge.resize(held.checked_add(bound).ok_or_else(too_large)?)?;
        let before = set.allocation_size();
        match set.try_reserve(more) {
            Ok(()) => {}
            Err(hashbrown::TryReserveError::AllocError { layout }) => {
                std::alloc::handle_alloc_error(layout)
            }
            Err(hashbrown::TryReserveError::CapacityOverflow) => {
                unreachable!("a table within its bound has a layout")
            }
        }
        debug_assert!(
            set.allocation_size() <= bound.max(before),
            "{} bytes past the bound {bound}",
            set.allocation_size()
        );
        charge.resize(self.allocation())?;
        Ok(())
    }

    /// [`Self::insert`], the inserts' table charged to `charge` first where `quad` is a new entry
    /// of a full one ([`Self::reserve`]). Refused, nothing changes.
    fn try_insert(
        &mut self,
        quad: EncodedQuad,
        in_base: impl FnOnce() -> bool,
        charge: &mut Reservation,
    ) -> EngineResult<bool> {
        if self.deletes.remove(&quad) {
            return Ok(true);
        }
        if in_base() {
            return Ok(false);
        }
        if self.inserts.len() >= self.inserts.capacity() && !self.inserts.contains(&quad) {
            self.reserve(Table::Inserts, 1, charge)?;
        }
        Ok(self.inserts.insert(quad))
    }

    /// [`Self::remove`], the deletes' table charged to `charge` first where `quad` is a new entry
    /// of a full one ([`Self::reserve`]). Refused, nothing changes.
    fn try_remove(
        &mut self,
        quad: EncodedQuad,
        in_base: impl FnOnce() -> bool,
        charge: &mut Reservation,
    ) -> EngineResult<bool> {
        if self.inserts.remove(&quad) {
            return Ok(true);
        }
        if !in_base() {
            return Ok(false);
        }
        if self.deletes.len() >= self.deletes.capacity() && !self.deletes.contains(&quad) {
            self.reserve(Table::Deletes, 1, charge)?;
        }
        Ok(self.deletes.insert(quad))
    }
}

/// One of a [`Delta`]'s two tables.
#[derive(Clone, Copy)]
enum Table {
    Inserts,
    Deletes,
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
    /// Bumped by every write (whether it changed anything or not): a pending snapshot made at
    /// one generation is the pending snapshot until the next (the transaction's memo, DUR-6).
    generation: u64,
    /// The last pending snapshot made, with the budget it is charged to (DUR-6): kept, with its
    /// charge, until the next write (which drops it) or the transaction's drop.
    memo: parking_lot::Mutex<Option<Memo>>,
    /// After the deltas: fields drop in order, the sets freed before their charge goes.
    charge: Option<Charge>,
}

/// A pending snapshot kept ([`Pending::memo`]): made at `generation`, charged to `budget`
/// (weak: it keeps the budget's allocation, so its address isn't taken by another while kept).
pub(in crate::engine) struct Memo {
    pub(in crate::engine) generation: u64,
    pub(in crate::engine) budget: std::sync::Weak<nrese_exec::Budget>,
    pub(in crate::engine) snapshot: Snapshot,
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
            generation: 0,
            memo: parking_lot::Mutex::new(None),
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
        self.asserted
            .reserve(Table::Inserts, more, &mut charge.held)
    }

    /// Room for one more asserted insert where a budget is attached and the table is full:
    /// what [`Self::try_insert_asserted`] would charge, reserved before the quad's terms are
    /// interned, so a refused insert interns nothing. Without a budget, nothing to do.
    pub(in crate::engine) fn try_room_for_insert(&mut self) -> EngineResult<()> {
        match &mut self.charge {
            Some(charge) if self.asserted.inserts.len() >= self.asserted.inserts.capacity() => {
                self.asserted.reserve(Table::Inserts, 1, &mut charge.held)
            }
            _ => Ok(()),
        }
    }

    /// [`Self::insert_asserted`], its growth charged to the attached budget before the table
    /// grows. Refused ([`EngineError::MemoryLimit`]), nothing changes; without a budget it is
    /// [`Self::insert_asserted`].
    pub(in crate::engine) fn try_insert_asserted(
        &mut self,
        quad: EncodedQuad,
    ) -> EngineResult<bool> {
        let Some(charge) = &mut self.charge else {
            return Ok(self.insert_asserted(quad));
        };
        let base = &self.base;
        self.asserted.try_insert(
            quad,
            || base.stack_contains(Stack::Asserted, &quad),
            &mut charge.held,
        )
    }

    /// [`Self::remove_asserted`], its growth charged as [`Self::try_insert_asserted`]'s is.
    pub(in crate::engine) fn try_remove_asserted(
        &mut self,
        quad: EncodedQuad,
    ) -> EngineResult<bool> {
        let Some(charge) = &mut self.charge else {
            return Ok(self.remove_asserted(quad));
        };
        let base = &self.base;
        self.asserted.try_remove(
            quad,
            || base.stack_contains(Stack::Asserted, &quad),
            &mut charge.held,
        )
    }

    /// Retracts every asserted quad matching `pattern` (SPARQL `CLEAR`/`DROP`, GSP `PUT`/
    /// `DELETE`): the base's, into the deletes, and the pending inserts'. Returns how many were
    /// visible, and so removed. The deletes' growth is charged to the attached budget before it
    /// happens, as [`Self::try_remove_asserted`]'s; nothing else is allocated, whatever the
    /// number of matches. Refused ([`EngineError::MemoryLimit`]) part way, the quads before the
    /// refusal stay removed: the caller drops the transaction.
    pub(in crate::engine) fn try_remove_matching(
        &mut self,
        pattern: &QuadPattern,
    ) -> EngineResult<u64> {
        let Self {
            base,
            asserted,
            charge,
            ..
        } = self;
        let pending = asserted.inserts.len();
        asserted.inserts.retain(|quad| !pattern.matches(quad));
        let mut removed = (pending - asserted.inserts.len()) as u64;
        // Base quads are never pending inserts (the delta is exact): each is in the deletes or
        // goes there.
        for quad in base.stack_quads(Stack::Asserted, pattern) {
            if asserted.deletes.contains(&quad) {
                continue;
            }
            if let Some(charge) = charge.as_mut()
                && asserted.deletes.len() >= asserted.deletes.capacity()
            {
                asserted.reserve(Table::Deletes, 1, &mut charge.held)?;
            }
            asserted.deletes.insert(quad);
            removed += 1;
        }
        Ok(removed)
    }

    pub(in crate::engine) fn base(&self) -> &Snapshot {
        &self.base
    }

    /// The last pending snapshot kept, if no write came since ([`Memo`]).
    pub(in crate::engine) fn memo(&self) -> &parking_lot::Mutex<Option<Memo>> {
        &self.memo
    }

    /// A write: a new generation, and the snapshot kept (with its charge) dropped.
    fn changed(&mut self) {
        self.generation += 1;
        *self.memo.get_mut() = None;
    }

    /// The writes so far ([`Self::generation`]'s field).
    pub(in crate::engine) fn generation(&self) -> u64 {
        self.generation
    }

    /// At most what [`Self::snapshot_changes`] allocates at once: its shadowed set and its
    /// four lists, each list's capacity at most twice its length (grown by doubling) and the
    /// set's table at most twice its load-factor size, with their control bytes.
    pub(in crate::engine) fn snapshot_changes_bound(&self) -> usize {
        use std::mem::size_of;
        let quad = size_of::<EncodedQuad>();
        let list = |n: usize| 2 * n.max(4) * quad;
        // Shadowed: asserted inserts that are inferred, and inferred inserts that are asserted
        // (a statement retracted, inferred, then asserted again), as `shadowed_inferred` takes
        // them (sol-loop r1 on DUR-6, S1).
        let shadowed = self.asserted.inserts.len() + self.inferred.inserts.len();
        let set = 2 * ((shadowed * 8 / 7 + 1).next_power_of_two().max(4) * (quad + 1) + 16);
        set + list(self.asserted.inserts.len())
            + list(self.asserted.deletes.len())
            + list(self.inferred.inserts.len())
            + list(self.inferred.deletes.len() + shadowed)
    }

    /// At most what [`Self::over_gone`] allocates for `hidden`: its list, grown by doubling.
    pub(in crate::engine) fn over_gone_bound(&self, hidden: &[EncodedQuad]) -> usize {
        let n = self.inferred.deletes.len() + self.asserted.inserts.len() + hidden.len();
        2 * n.max(4) * std::mem::size_of::<EncodedQuad>()
    }

    pub(in crate::engine) fn asserted(&self) -> &Delta {
        &self.asserted
    }

    pub(in crate::engine) fn inferred(&self) -> &Delta {
        &self.inferred
    }

    /// The base and both deltas, for the write transaction's commit, which alone turns them into
    /// a record, and their charge: first, so a commit that binds the parts in this order drops
    /// it last, after the sets. The kept pending snapshot (DUR-6) goes here, with its charge,
    /// before the commit builds its record: a commit ends the transaction it was kept for.
    pub(super) fn into_parts(self) -> (Option<Reservation>, Snapshot, Delta, Delta) {
        let Self {
            base,
            asserted,
            inferred,
            generation: _,
            memo,
            charge,
        } = self;
        drop(memo);
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
        self.changed();
        let base = &self.base;
        self.asserted
            .insert(quad, || base.stack_contains(Stack::Asserted, &quad))
    }

    /// Retracts an asserted quad; returns whether it changed anything. Inferred quads are
    /// unaffected.
    pub(in crate::engine) fn remove_asserted(&mut self, quad: EncodedQuad) -> bool {
        self.changed();
        let base = &self.base;
        self.asserted
            .remove(quad, || base.stack_contains(Stack::Asserted, &quad))
    }

    /// Adds an inferred quad: `false` if it is already inferred, or asserted.
    pub(in crate::engine) fn insert_inferred(&mut self, quad: EncodedQuad) -> bool {
        self.changed();
        if self.stack_contains(Stack::Asserted, &quad) {
            return false;
        }
        let base = &self.base;
        self.inferred
            .insert(quad, || base.stack_contains(Stack::Inferred, &quad))
    }

    /// Removes an inferred quad: `false` if it was not visible as inferred.
    pub(in crate::engine) fn remove_inferred(&mut self, quad: EncodedQuad) -> bool {
        self.changed();
        if !self.stack_contains(Stack::Inferred, &quad) {
            return false;
        }
        let base = &self.base;
        self.inferred
            .remove(quad, || base.stack_contains(Stack::Inferred, &quad))
    }

    /// Enforces disjointness: inferred quads that end up asserted leave the inferred stack.
    pub(super) fn take_over_inferred(&mut self) {
        self.changed();
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

    /// An engine whose base holds quads 0..`n` (`quad(i)` with real ids interned).
    fn committed(n: u64) -> (Engine, Vec<EncodedQuad>) {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let iri = |name: String| nrese_rdf::NamedNode::new_unchecked(name);
        for i in 0..n {
            tx.insert(
                nrese_rdf::Quad::new(
                    iri(format!("http://example.com/s{i}")),
                    iri("http://example.com/p".to_owned()),
                    iri("http://example.com/o".to_owned()),
                    nrese_rdf::GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.commit().unwrap();
        let quads = engine
            .snapshot()
            .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
            .collect();
        (engine, quads)
    }

    /// `remove_matching` removes, and counts, what was visible: the base's quads not yet
    /// deleted and the pending inserts, and leaves nothing matching visible.
    #[test]
    fn removing_what_matches_removes_what_was_visible() {
        let (engine, base) = committed(100);
        for budget in [None, Some(Arc::new(Budget::unlimited()))] {
            let mut pending = Pending::over(engine.snapshot());
            if let Some(budget) = &budget {
                pending.charge_to(Arc::clone(budget)).unwrap();
            }
            for quad in &base[..30] {
                assert!(pending.remove_asserted(*quad));
            }
            for i in 0..20 {
                assert!(pending.insert_asserted(quad(1000 + i)));
            }
            let visible = pending
                .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
                .count() as u64;
            assert_eq!(visible, 70 + 20);
            assert_eq!(
                pending.try_remove_matching(&QuadPattern::all()).unwrap(),
                visible
            );
            assert_eq!(
                pending
                    .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
                    .count(),
                0,
                "nothing matching is left"
            );
            assert_eq!(
                pending.asserted.counts(),
                (0, 100),
                "every base quad deleted once"
            );
            assert_eq!(pending.try_remove_matching(&QuadPattern::all()).unwrap(), 0);
            if let Some(budget) = budget {
                assert_eq!(
                    budget.used(),
                    pending.asserted.allocation(),
                    "the charge is both tables' allocation"
                );
            }
        }
    }

    /// The charged changes charge a table before it grows: the budget's peak never passes its
    /// limit, a refusal is the memory error, and what is charged is the tables' allocation.
    #[test]
    fn charged_changes_never_grow_past_their_budget() {
        let (engine, base) = committed(5000);
        let limit = 64 * 1024;
        assert!(base.len() * size_of::<EncodedQuad>() > 2 * limit);
        let budget = Arc::new(Budget::new(limit));
        let mut clearing = pending(&engine, &budget);
        let refused = clearing.try_remove_matching(&QuadPattern::all());
        assert!(
            matches!(refused, Err(EngineError::MemoryLimit(_))),
            "{refused:?}"
        );
        assert!(
            budget.peak() <= limit,
            "peak {} within {limit}",
            budget.peak()
        );
        assert_eq!(budget.used(), clearing.asserted.allocation());

        let budget = Arc::new(Budget::new(limit));
        let mut inserting = pending(&engine, &budget);
        let mut refused = None;
        for i in 0..5000 {
            if let Err(error) = inserting.try_insert_asserted(quad(100_000 + i)) {
                refused = Some(error);
                break;
            }
        }
        assert!(
            matches!(refused, Some(EngineError::MemoryLimit(_))),
            "{refused:?}"
        );
        assert!(
            budget.peak() <= limit,
            "peak {} within {limit}",
            budget.peak()
        );
        assert_eq!(budget.used(), inserting.asserted.allocation());
        drop(inserting);
        assert_eq!(budget.used(), 0, "dropped, nothing stays charged");
    }

    /// Both tables share one charge (B2's condition, 9 Nov): growing the deletes, then
    /// reserving inserts, then growing the deletes again never trims the charge below both
    /// tables' allocation together.
    #[test]
    fn one_charge_covers_both_tables() {
        let (engine, base) = committed(2000);
        let budget = Arc::new(Budget::unlimited());
        let mut pending = pending(&engine, &budget);
        let covers = |pending: &Pending| {
            let (inserts, deletes) = (
                pending.asserted.inserts.allocation_size(),
                pending.asserted.deletes.allocation_size(),
            );
            assert!(inserts > 0 || deletes > 0);
            assert_eq!(
                budget.used(),
                inserts + deletes,
                "the charge is both tables'"
            );
        };
        for quad in &base[..1000] {
            assert!(pending.try_remove_asserted(*quad).unwrap());
        }
        covers(&pending);
        pending.try_reserve_inserts(3000).unwrap();
        covers(&pending);
        for i in 0..3000 {
            assert!(pending.try_insert_asserted(quad(100_000 + i)).unwrap());
        }
        for quad in &base[1000..] {
            assert!(pending.try_remove_asserted(*quad).unwrap());
        }
        covers(&pending);
        assert_eq!(pending.asserted.counts(), (3000, 2000));
    }

    /// A refused charged insert interns nothing: its room is charged before its terms are.
    #[test]
    fn a_refused_charged_insert_interns_nothing() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        tx.charge_pending_to(Arc::new(Budget::new(0))).unwrap();
        let iri = |name: &str| nrese_rdf::NamedNode::new_unchecked(name);
        let statement = nrese_rdf::Quad::new(
            iri("http://example.com/new-s"),
            iri("http://example.com/new-p"),
            iri("http://example.com/new-o"),
            nrese_rdf::GraphName::DefaultGraph,
        );
        let refused = tx.try_insert(statement.as_ref());
        assert!(
            matches!(refused, Err(EngineError::MemoryLimit(_))),
            "{refused:?}"
        );
        assert_eq!(
            tx.lookup(iri("http://example.com/new-s").as_ref().into()),
            None
        );
        assert_eq!(tx.pending(), (0, 0));
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
