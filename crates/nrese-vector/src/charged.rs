//! A binary heap whose memory is charged to a query's budget before it is allocated, as
//! [`nrese_exec::ChargedVec`] charges a vector's: a search's heaps grow with its `k` and
//! beam, which a caller may widen towards the whole space, so they are the query's memory.
//! `nrese_exec` has no charged heap; this one is built on its [`Reservation`].

use std::collections::BinaryHeap;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

/// A max-heap charged to a budget: every growth of its capacity is charged first, beside
/// the old capacity (a reallocation holds both while it moves the items), then the old
/// capacity's charge is released. A growth the budget has no room for fails without
/// allocating. Dropped, it frees the items, then releases their charge.
#[derive(Debug)]
pub(crate) struct Heap<'b, T: Ord> {
    /// Before `held`: fields drop in order, the items first.
    heap: BinaryHeap<T>,
    /// Exactly `heap`'s capacity in bytes.
    held: Reservation<&'b Budget>,
    budget: &'b Budget,
}

impl<'b, T: Ord> Heap<'b, T> {
    /// Room for `capacity` items, charged first.
    pub(crate) fn with_capacity(
        budget: &'b Budget,
        capacity: usize,
    ) -> Result<Self, BudgetExceeded> {
        let mut held = Reservation::new(budget);
        held.charge(bytes::<T>(budget, capacity)?)?;
        let heap = BinaryHeap::with_capacity(capacity);
        held.resize(bytes::<T>(budget, heap.capacity())?)?;
        Ok(Self { heap, held, budget })
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.heap.len()
    }

    #[inline]
    pub(crate) fn peek(&self) -> Option<&T> {
        self.heap.peek()
    }

    #[inline]
    pub(crate) fn pop(&mut self) -> Option<T> {
        self.heap.pop()
    }

    /// Pushes `item`, doubling the capacity first if it is full. A search pushes on its
    /// innermost loop: the push stays as cheap as the bare heap's, the growth (rare, a
    /// doubling) out of line.
    #[inline]
    pub(crate) fn push(&mut self, item: T) -> Result<(), BudgetExceeded> {
        if self.heap.len() == self.heap.capacity() {
            self.grow(self.heap.len() + 1)?;
        }
        self.heap.push(item);
        Ok(())
    }

    /// Room for `additional` more items, grown (and charged) first if there is less: the
    /// pushes into it ([`Self::push_reserved`]) then neither grow nor fail.
    #[inline]
    pub(crate) fn reserve(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        if self.heap.capacity() - self.heap.len() < additional {
            self.grow(self.heap.len().saturating_add(additional))?;
        }
        Ok(())
    }

    /// Pushes `item` into room [`Self::reserve`] made.
    #[inline]
    pub(crate) fn push_reserved(&mut self, item: T) {
        debug_assert!(
            self.heap.len() < self.heap.capacity(),
            "pushed past the room reserved"
        );
        self.heap.push(item);
    }

    /// Grows the capacity to at least `least`, doubling at the least: the new block charged
    /// beside the old one, then the old one's charge released.
    #[cold]
    #[inline(never)]
    fn grow(&mut self, least: usize) -> Result<(), BudgetExceeded> {
        let target = self.heap.capacity().saturating_mul(2).max(least).max(4);
        self.held.charge(bytes::<T>(self.budget, target)?)?;
        self.heap.reserve_exact(target - self.heap.len());
        self.held
            .resize(bytes::<T>(self.budget, self.heap.capacity())?)
    }

    /// The items in ascending order, each mapped by `map`, in a vector charged to the same
    /// budget (charged before it is allocated, while the heap's block is still held).
    /// Sorted in place by the slice's sort, not the heap's own heap sort, which takes about
    /// twice the comparisons (instruction counts, 7 Nov).
    pub(crate) fn into_sorted<U>(
        self,
        mut map: impl FnMut(T) -> U,
    ) -> Result<ChargedVec<U, &'b Budget>, BudgetExceeded> {
        let Self { heap, held, budget } = self;
        let mut items = heap.into_vec();
        items.sort_unstable();
        let len = items.len();
        let out = ChargedVec::collect(budget, len, items.into_iter().map(&mut map));
        drop(held);
        out
    }
}

/// Bytes of `n` items of `T`, or a refusal for a size past `usize`.
pub(crate) fn bytes<T>(budget: &Budget, n: usize) -> Result<usize, BudgetExceeded> {
    n.checked_mul(size_of::<T>().max(1)).ok_or(BudgetExceeded {
        limit: budget.limit(),
        requested: usize::MAX,
        used: budget.used(),
        shared: false,
        unread: false,
    })
}
