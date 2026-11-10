//! A hash map whose table is charged before it is allocated ([`ChargedMap`]).

use std::hash::Hash;
use std::ops::Deref;

use foldhash::fast::FixedState;
use hashbrown::hash_map::Entry;

use crate::budget::hold::TOKEN;
use crate::budget::{Hold, Pool, Reservation, Share};
use crate::{Budget, BudgetExceeded};

/// A hash map whose table is charged before it is allocated, to a budget or to a [`Pool`]
/// ([`Hold`]): the table grows on one path ([`ChargedMap::get_or_insert_with`] where it is
/// full), which charges an upper bound of the new table beside the old (a rehash holds both),
/// lets the table grow, releases the old table's charge once it is freed, and trims the new
/// charge to the table's allocation. Between growths it is charged exactly its allocation;
/// it never grows otherwise (no entry is removed, so an insert into a table with room never
/// rehashes). Dropped, it frees the table, then releases the charge. Keys and values that own
/// memory elsewhere are not counted. The upper bound follows hashbrown's bucket and layout rule;
/// a test checks it against the allocation at every growth.
#[derive(Debug)]
pub struct ChargedMap<K, V, H: Hold = Reservation> {
    /// Before `held`: fields drop in order, the table first.
    map: hashbrown::HashMap<K, V, FixedState>,
    /// Exactly the table's allocation, between growths.
    held: H,
}

impl<K, V, B: Deref<Target = Budget>> ChargedMap<K, V, Reservation<B>> {
    pub fn new(budget: B) -> Self {
        Self::empty(Reservation::new(budget))
    }
}

impl<'p, K, V, B: Deref<Target = Budget>> ChargedMap<K, V, Share<'p, B>> {
    /// An empty map drawing from `pool`.
    pub fn in_pool(pool: &'p Pool<B>) -> Self {
        Self::empty(Share::empty(pool))
    }
}

impl<K, V, H: Hold> ChargedMap<K, V, H> {
    fn empty(held: H) -> Self {
        debug_assert_eq!(held.held(TOKEN), 0);
        Self {
            map: hashbrown::HashMap::with_hasher(FixedState::default()),
            held,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The bytes held: the table's allocation.
    pub fn charged(&self) -> usize {
        self.held.held(TOKEN)
    }
}

impl<K: Eq + Hash, V, H: Hold> ChargedMap<K, V, H> {
    /// The table's allocation, in bytes.
    pub fn allocation_size(&self) -> usize {
        self.map.allocation_size()
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    /// The value under `key`, `value()` put there first where there is none, and whether it
    /// was put there now. A full table grows first, charged before it allocates; refused,
    /// nothing changes.
    pub fn get_or_insert_with(
        &mut self,
        key: K,
        value: impl FnOnce() -> V,
    ) -> Result<(&mut V, bool), BudgetExceeded> {
        if self.map.len() == self.map.capacity() && !self.map.contains_key(&key) {
            self.grow()?;
        }
        Ok(match self.map.entry(key) {
            Entry::Occupied(entry) => (entry.into_mut(), false),
            Entry::Vacant(entry) => (entry.insert(value()), true),
        })
    }

    /// `value` put under `key`, and the value it replaced: one probe, where an entry probes twice
    /// for a new key. hashbrown makes room for one more before it probes, so a full table grows
    /// first, charged, even where `key` is there. Refused, nothing changes.
    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, BudgetExceeded> {
        if self.map.len() == self.map.capacity() {
            self.grow()?;
        }
        Ok(self.map.insert(key, value))
    }

    /// Room for at least one more entry, doubling: the new table's bound charged beside the
    /// old table's charge, the table grown, then the old charge released (its table freed in
    /// the growth) and the new trimmed to the allocation.
    fn grow(&mut self) -> Result<(), BudgetExceeded> {
        let wanted = self
            .map
            .len()
            .checked_add(1)
            .ok_or_else(|| self.held.too_large(TOKEN))?;
        let to = wanted.max(self.map.capacity().saturating_mul(2));
        let bound = table_bound::<(K, V)>(to).ok_or_else(|| self.held.too_large(TOKEN))?;
        let old = self.held.renew(TOKEN, bound)?;
        match self.map.try_reserve(to - self.map.len()) {
            Ok(()) => {}
            Err(hashbrown::TryReserveError::AllocError { layout }) => {
                std::alloc::handle_alloc_error(layout)
            }
            Err(hashbrown::TryReserveError::CapacityOverflow) => {
                unreachable!("a table within its bound has a layout")
            }
        }
        drop(old);
        let allocated = self.map.allocation_size();
        assert!(
            allocated <= bound,
            "the table's allocation, {allocated} bytes, past its bound of {bound}"
        );
        self.held.trim(TOKEN, allocated);
        Ok(())
    }
}

/// An upper bound of the bytes a hashbrown table of entries `E` allocates for room for
/// `capacity` entries, if it can be allocated: its buckets (at least 16 below 15 entries, else
/// a power of two at least 8/7 of them), their entries rounded up to the control bytes'
/// alignment, a control byte a bucket and a group's more (hashbrown 0.16's `TableLayout`; a
/// group is at most 16 bytes). `E` is the table's entry: a map's `(K, V)`, a set's `T`. A
/// table that removes entries can rehash without growing its length past its capacity: its
/// holder charges this bound for the capacity hashbrown grows to, before it grows, as
/// [`ChargedMap`] does for its own (`nrese-engine`'s pending inserts, SYN-54, 8 Nov). Held
/// against hashbrown's allocation by this module's tests.
pub fn table_bound<E>(capacity: usize) -> Option<usize> {
    const GROUP: usize = 16;
    let buckets = match capacity {
        0..15 => 16,
        _ => (capacity.checked_mul(8)? / 7).checked_next_power_of_two()?,
    };
    let align = std::mem::align_of::<E>().max(GROUP);
    let entries = std::mem::size_of::<E>()
        .checked_mul(buckets)?
        .checked_add(align - 1)?
        & !(align - 1);
    let bytes = entries.checked_add(buckets)?.checked_add(GROUP)?;
    (bytes <= isize::MAX as usize).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A charged map's table is charged at least its allocation at every growth (the bound
    /// holds against hashbrown) and exactly between growths, never more than it was charged
    /// at any moment, and dropped it frees the table, then releases it all: for entries of 16
    /// and of 24 bytes, grown to 2^16 keys, a key added again changing nothing.
    #[test]
    fn a_charged_maps_table_is_charged_while_it_lives() {
        if !crate::measure::alone() {
            return;
        }
        fn grown<V: Default>(budget: &Budget) {
            let mut map: ChargedMap<u64, V, Reservation<&Budget>> = ChargedMap::new(budget);
            for key in 0..1u64 << 16 {
                let (_, new) = map.get_or_insert_with(key, V::default).unwrap();
                assert!(new);
                assert_eq!(map.charged(), map.allocation_size(), "at key {key}");
                assert_eq!(budget.used(), map.charged());
            }
            let charged = map.charged();
            let (_, new) = map.get_or_insert_with(7, V::default).unwrap();
            assert!(!new);
            assert_eq!((map.len(), map.charged()), (1 << 16, charged));
        }
        let budget = Budget::unlimited();
        let ((), excess) = crate::measure::measure_within(&budget, || {
            grown::<u64>(&budget);
            grown::<(u64, u64)>(&budget);
        });
        assert_eq!((excess, budget.used()), (0, 0));
    }

    /// A growth the budget refuses changes nothing and allocates nothing: the map keeps its
    /// entries, its table and its charge, and answers the keys it has.
    #[test]
    fn a_refused_growth_changes_nothing() {
        let budget = Budget::new(4 << 10);
        let mut map: ChargedMap<u64, u64, Reservation<&Budget>> = ChargedMap::new(&budget);
        let mut key = 0;
        let refused = loop {
            match map.get_or_insert_with(key, || key) {
                Ok(_) => key += 1,
                Err(refused) => break refused,
            }
        };
        assert!(refused.requested > 0);
        assert_eq!(map.len() as u64, key);
        assert_eq!(map.charged(), map.allocation_size());
        assert_eq!(budget.used(), map.charged());
        assert_eq!(map.get(&0), Some(&0));
        let (_, new) = map.get_or_insert_with(0, || 99).unwrap();
        assert!(!new, "a key it has needs no room");
    }

    /// The bound is at least the allocation of hashbrown's table at every capacity it grows
    /// to, for small and large entries.
    #[test]
    fn the_bound_holds_for_every_capacity_a_table_grows_to() {
        fn check<E>() {
            let mut table: hashbrown::HashMap<u64, E, FixedState> =
                hashbrown::HashMap::with_hasher(FixedState::default());
            for capacity in (0..20)
                .map(|k| 1usize << k)
                .chain([3, 7, 14, 15, 29, 57, 1000])
            {
                table.clear();
                table.shrink_to(0);
                table.reserve(capacity);
                let bound = table_bound::<(u64, E)>(capacity).unwrap();
                assert!(
                    table.allocation_size() <= bound,
                    "{capacity} entries of {} bytes: {} allocated, bound {bound}",
                    std::mem::size_of::<(u64, E)>(),
                    table.allocation_size()
                );
            }
        }
        check::<u8>();
        check::<u64>();
        check::<[u64; 5]>();
    }
}
