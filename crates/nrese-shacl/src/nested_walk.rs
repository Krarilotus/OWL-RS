//! Suspended callers of a depth-first shape walk, indexed by their active keys.

use std::hash::Hash;

use hashbrown::{HashMap, TryReserveError};
use nrese_exec::charged_map::table_bound;
use nrese_exec::{Budget, BudgetExceeded, Reservation};

pub(crate) struct Suspended<'a, K, F> {
    // A root and its direct child need only one suspended frame, inline.
    root: Option<(K, F)>,
    deeper: Option<HashMap<K, F>>,
    /// Capacity without tombstones, recorded after the last allocation.
    full_capacity: usize,
    // The table is freed before its charge is returned.
    held: Reservation<&'a Budget>,
}

impl<'a, K: Eq + Hash, F> Suspended<'a, K, F> {
    pub(crate) fn new(budget: &'a Budget) -> Self {
        Self {
            root: None,
            deeper: None,
            full_capacity: 0,
            held: Reservation::new(budget),
        }
    }

    pub(crate) fn contains(&self, key: &K) -> bool {
        self.root.as_ref().is_some_and(|(root, _)| root == key)
            || self
                .deeper
                .as_ref()
                .is_some_and(|deeper| deeper.contains_key(key))
    }

    pub(crate) fn park(&mut self, key: K, frame: F) -> Result<(), BudgetExceeded> {
        if self.root.is_none() {
            self.root = Some((key, frame));
            return Ok(());
        }
        let deeper = self.deeper.get_or_insert_with(HashMap::new);
        if deeper.len() == deeper.capacity() {
            Self::reserve(deeper, &mut self.full_capacity, &mut self.held)?;
        }
        let previous = deeper.insert(key, frame);
        debug_assert!(previous.is_none());
        Ok(())
    }

    pub(crate) fn resume(&mut self, key: &K) -> F {
        if self.root.as_ref().is_some_and(|(root, _)| root == key) {
            self.root.take().expect("the suspended root").1
        } else {
            self.deeper
                .as_mut()
                .and_then(|deeper| deeper.remove(key))
                .expect("a suspended caller")
        }
    }

    /// hashbrown 0.16 rehashes in place when live entries will fit in half the table.
    /// Otherwise it allocates at least the next size, charged beside the old table.
    fn reserve(
        deeper: &mut HashMap<K, F>,
        full_capacity: &mut usize,
        held: &mut Reservation<&Budget>,
    ) -> Result<(), BudgetExceeded> {
        let wanted = deeper.len() + 1;
        if wanted <= *full_capacity / 2 {
            deeper.reserve(1);
            return Ok(());
        }
        let bound = table_bound::<(K, F)>(wanted.max(*full_capacity + 1)).unwrap_or(usize::MAX);
        held.charge(bound)?;
        match deeper.try_reserve(1) {
            Ok(()) => {}
            Err(TryReserveError::AllocError { layout }) => std::alloc::handle_alloc_error(layout),
            Err(TryReserveError::CapacityOverflow) => {
                unreachable!("a table within its bound has a layout")
            }
        }
        *full_capacity = deeper.capacity();
        debug_assert!(deeper.allocation_size() <= bound);
        held.resize(deeper.allocation_size())
    }
}
