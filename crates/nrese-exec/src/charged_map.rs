//! A hash map whose table is charged before it is allocated ([`ChargedMap`]).

use std::hash::Hash;
use std::ops::Deref;

use foldhash::fast::FixedState;
use hashbrown::hash_map::Entry;

use crate::budget::hold::TOKEN;
use crate::budget::{Hold, Pool, Reservation, Share};
use crate::{Budget, BudgetExceeded};

/// A hash map whose table is charged before it is allocated, to a budget or to a [`Pool`]
/// ([`Hold`]). When an insertion needs room, the table reuses tombstones in place if it
/// can. To grow, it charges an upper bound of the new table beside the old, lets the table
/// grow, releases the old table's charge once it is freed, and trims the new charge to the
/// table's allocation. Between growths it is charged exactly its allocation. Dropped, it
/// frees the table, then releases the charge. Keys and values that own memory elsewhere
/// are not counted. The upper bound follows hashbrown's bucket and layout rule.
#[derive(Debug)]
pub struct ChargedMap<K, V, H: Hold = Reservation> {
    /// Before `held`: fields drop in order, the table first.
    map: hashbrown::HashMap<K, V, FixedState>,
    /// Exactly the table's allocation, between growths.
    held: H,
    /// Usable entry slots before any are lost to tombstones.
    full_capacity: usize,
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
            full_capacity: 0,
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

    /// Removes a key, retaining the allocation and its charge. Its tombstone may make
    /// the next insertion rehash even below the original capacity.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.map.remove(key)
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

    /// Room for at least one more entry, reusing tombstones or doubling the table.
    fn grow(&mut self) -> Result<(), BudgetExceeded> {
        let wanted = self
            .map
            .len()
            .checked_add(1)
            .ok_or_else(|| self.held.too_large(TOKEN))?;
        // hashbrown 0.16 rehashes in place when the entries fit in half the table.
        if wanted <= self.full_capacity / 2 {
            Self::reserve(&mut self.map, 1);
            assert_eq!(self.map.allocation_size(), self.held.held(TOKEN));
            return Ok(());
        }
        let to = wanted.max(self.full_capacity.saturating_mul(2));
        let bound = table_bound::<(K, V)>(to).ok_or_else(|| self.held.too_large(TOKEN))?;
        let old = self.held.renew(TOKEN, bound)?;
        let additional = to - self.map.len();
        Self::reserve(&mut self.map, additional);
        drop(old);
        self.full_capacity = self.map.capacity();
        let allocated = self.map.allocation_size();
        assert!(
            allocated <= bound,
            "the table's allocation, {allocated} bytes, past its bound of {bound}"
        );
        self.held.trim(TOKEN, allocated);
        Ok(())
    }

    fn reserve(map: &mut hashbrown::HashMap<K, V, FixedState>, additional: usize) {
        match map.try_reserve(additional) {
            Ok(()) => {}
            Err(hashbrown::TryReserveError::AllocError { layout }) => {
                std::alloc::handle_alloc_error(layout)
            }
            Err(hashbrown::TryReserveError::CapacityOverflow) => {
                unreachable!("a table within its bound has a layout")
            }
        }
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

    /// Spec: removing entries keeps the table charged. When tombstones exhaust its
    /// capacity, inserting must reuse or grow the table as hashbrown does, and any
    /// allocation must be covered by a charge before it happens.
    #[test]
    fn tombstones_are_reused_or_grown_within_the_charge() {
        if !crate::measure::alone() {
            return;
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        struct Collision(u64);

        impl Hash for Collision {
            fn hash<S: std::hash::Hasher>(&self, state: &mut S) {
                0_u64.hash(state);
            }
        }

        const FULL: u64 = 112;
        for remaining in [FULL / 2 - 1, FULL - 1] {
            let budget = Budget::unlimited();
            let mut map = ChargedMap::new(&budget);
            for key in 0..FULL {
                map.insert(Collision(key), key).unwrap();
            }
            let mut reference = map.map.clone();
            let allocated = reference.allocation_size();
            let peak = budget.peak();
            for _ in remaining..FULL {
                // Choose a removal that leaves a tombstone. Collisions keep the probe
                // groups dense enough for both the in-place and allocating cases.
                let key = reference
                    .keys()
                    .copied()
                    .find(|key| {
                        let mut copy = reference.clone();
                        copy.remove(key);
                        copy.capacity() < reference.capacity()
                    })
                    .expect("a removal that leaves a tombstone");
                assert_eq!(map.remove(&key), reference.remove(&key));
            }
            assert_eq!(reference.len(), reference.capacity());
            assert_eq!(budget.used(), allocated);

            reference.reserve(1);
            let reuses_table = remaining < FULL / 2;
            assert_eq!(reference.allocation_size() == allocated, reuses_table);
            let key = Collision(FULL);
            let expected = reference.insert(key, FULL);
            let (inserted, excess) =
                crate::measure::measure_within(&budget, || map.insert(key, FULL).unwrap());
            assert_eq!(inserted, expected);
            assert_eq!(map.len(), reference.len());
            for (key, value) in &reference {
                assert_eq!(map.get(key), Some(value));
            }
            assert_eq!(excess, 0);
            assert_eq!(map.allocation_size(), reference.allocation_size());
            assert_eq!(budget.used(), reference.allocation_size());
            if reuses_table {
                assert_eq!(budget.peak(), peak, "in-place rehash needs no charge");
            }
        }
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
