//! The query's computed terms and optional decode cache, each with its own lifetime.
//!
//! Payload fields precede their reservation. Result delivery moves only computed values'
//! reservation; neither cache infers its ownership from the budget's total counter.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, Reservation, computed_id};
use nrese_rdf::Term;

use super::Holding;

/// Existing optional decode-cache limit; this migration changes ownership only.
const DECODE_CACHE_ENTRIES: usize = 1 << 18;

/// The existing map-growth bound, charged beside the old table before reallocation.
fn grown_table<T>(len: usize, capacity: usize) -> usize {
    match len < capacity {
        true => 0,
        false => nrese_exec::map_bytes::<T>(capacity.saturating_mul(2).max(3)),
    }
}

#[derive(Default)]
pub(super) struct Computed {
    values: Vec<Term>,
    ids: HashMap<Term, u64>,
    table: usize,
    /// One copy's heap bound, shared by the values and the map's cloned keys.
    heap: usize,
    held: Option<Reservation>,
}

impl Deref for Computed {
    type Target = [Term];

    fn deref(&self) -> &[Term] {
        &self.values
    }
}

impl Computed {
    /// Borrowed collision probe for response-local fresh blank-node identities.
    pub(super) fn contains(&self, term: &Term) -> bool {
        self.ids.contains_key(term)
    }

    /// O(1) expected; one combined admission on a miss, before either clone/growth.
    pub(super) fn intern(
        &mut self,
        term: &Term,
        budget: &Arc<Budget>,
    ) -> Result<u64, BudgetExceeded> {
        if let Some(&id) = self.ids.get(term) {
            return Ok(id);
        }
        let old_list = self.values.capacity() * size_of::<Term>();
        let list = match self.values.len() < self.values.capacity() {
            true => 0,
            false => (self.values.capacity() + self.values.capacity().max(4)) * size_of::<Term>(),
        };
        let table = grown_table::<(Term, u64)>(self.ids.len(), self.ids.capacity());
        let heap = term.heap_bytes();
        let bytes = heap
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(list))
            .and_then(|bytes| bytes.checked_add(table))
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        self.held
            .get_or_insert_with(|| Reservation::new(Arc::clone(budget)))
            .charge(bytes)?;
        if list > 0 {
            self.values.reserve_exact(self.values.capacity().max(4));
        }
        let id = computed_id(self.values.len() as u64);
        self.values.push(term.clone());
        self.ids.insert(term.clone(), id);
        self.heap += heap;
        let held = self.held.as_mut().expect("charged before insertion");
        if list > 0 {
            held.release(old_list);
        }
        if table > 0 {
            held.release(std::mem::replace(&mut self.table, table));
        }
        Ok(id)
    }

    /// Drops the interning map, then returns its charge and moves the values/token.
    /// O(1) handoff beyond the map's already-required destruction; no term-size walk.
    pub(super) fn into_values(self, budget: &Arc<Budget>) -> Holding<Vec<Term>> {
        let Self {
            values,
            ids,
            table,
            heap,
            held,
        } = self;
        // Keep values and their token paired even during map destruction/unwind.
        let mut values = Holding::new(
            values,
            held.unwrap_or_else(|| Reservation::new(Arc::clone(budget))),
        );
        drop(ids);
        values._held.release(table + heap);
        values
    }
}

#[derive(Default)]
pub(super) struct Decoded {
    values: HashMap<u64, Option<Term>>,
    table: usize,
    held: Option<Reservation>,
}

impl Decoded {
    pub(super) fn get(&self, id: &u64) -> Option<&Option<Term>> {
        self.values.get(id)
    }

    /// Optional cache admission: failing to retain a decoded value changes no answer.
    pub(super) fn keep(&mut self, id: u64, term: &Option<Term>, budget: &Arc<Budget>) {
        if self.values.len() >= DECODE_CACHE_ENTRIES {
            return;
        }
        let heap = term.as_ref().map_or(0, Term::heap_bytes);
        let table = grown_table::<(u64, Option<Term>)>(self.values.len(), self.values.capacity());
        let Some(bytes) = heap.checked_add(table) else {
            return;
        };
        if bytes > budget.remaining() {
            return;
        }
        let held = self
            .held
            .get_or_insert_with(|| Reservation::new(Arc::clone(budget)));
        if held.charge(bytes).is_err() {
            return;
        }
        self.values.insert(id, term.clone());
        if table > 0 {
            held.release(std::mem::replace(&mut self.table, table));
        }
    }
}
