//! Memory accounting for queries.
//!
//! Operators charge the bytes of the tables they produce and release them when a table is
//! consumed. When a charge would exceed the limit, the query fails with [`BudgetExceeded`]
//! instead of growing the process: the scorecard showed server memory growing from 16 to
//! 24 GiB under 8 concurrent clients without any bound.
//!
//! Three levels apply. A [`SharedBudget`] is the server's, for all requests that run at
//! the same time. An [`Envelope`] is one request's: everything the request holds draws
//! from it (its evaluations and the results it retains, the DL tasks' capacity, DL
//! compilation scratch), and it draws from the server's. A [`Budget`] made by
//! [`Envelope::part`] is one part's, such as an evaluation, counted on its own so that
//! what the part leaves behind can be handed to its result. Each level charges the one
//! above it too, and returns what it holds when it ends, however it ends.
//!
//! A [`SharedBudget`] can itself lie within another ([`SharedBudget::within`]): the
//! process's, which everything charged in it draws from (queries, maintenance), above the
//! queries' and others'. Their limits may add up to more than the process's: it refuses
//! what doesn't fit them all.

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The memory a group of operations may hold together: all running queries, or the
/// process's for everything it charges.
#[derive(Debug)]
pub struct SharedBudget {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
    /// The budget this one lies within, charged first.
    parent: Option<Arc<SharedBudget>>,
}

/// A shared budget's refusal: the limit of the level that refused, and its use then.
#[derive(Debug, Clone, Copy)]
struct Refused {
    limit: usize,
    used: usize,
}

impl SharedBudget {
    pub fn new(limit_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: limit_bytes,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            parent: None,
        })
    }

    /// `limit_bytes` within `parent`: a charge here is charged there too, first, and
    /// refused where either refuses.
    pub fn within(limit_bytes: usize, parent: Arc<SharedBudget>) -> Arc<Self> {
        Arc::new(Self {
            limit: limit_bytes,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            parent: Some(parent),
        })
    }

    /// Bytes that may still be charged here: the own rest, and no more than the levels
    /// above have left.
    pub fn remaining(&self) -> usize {
        let own = self.limit.saturating_sub(self.used());
        self.parent
            .as_ref()
            .map_or(own, |parent| own.min(parent.remaining()))
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// The highest amount in use at any time.
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    /// Charges `bytes` above, then here; refused anywhere, nothing stays charged.
    fn charge(&self, bytes: usize) -> Result<(), Refused> {
        if let Some(parent) = &self.parent {
            parent.charge(bytes)?;
        }
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let Some(next) = used.checked_add(bytes).filter(|&next| next <= self.limit) else {
                if let Some(parent) = &self.parent {
                    parent.release(bytes);
                }
                return Err(Refused {
                    limit: self.limit,
                    used,
                });
            };
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => {
                    self.peak.fetch_max(next, Ordering::Relaxed);
                    return Ok(());
                }
                Err(actual) => used = actual,
            }
        }
    }

    /// Releases `bytes` here, at most what is charged, and as much above.
    fn release(&self, bytes: usize) {
        let before = self
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                Some(used.saturating_sub(bytes))
            })
            .unwrap_or_else(|used| used);
        if let Some(parent) = &self.parent {
            parent.release(bytes.min(before));
        }
    }
}

#[derive(Debug)]
pub struct Budget {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
    /// A charge failed here or above.
    refused: AtomicBool,
    parent: Option<Parent>,
}

/// What a budget charges besides itself.
#[derive(Debug)]
enum Parent {
    /// The server's, for all requests.
    Server(Arc<SharedBudget>),
    /// The request's that this budget is a part of.
    Envelope(Envelope),
}

/// A request's memory envelope: the one budget everything the request holds draws from,
/// within the server's. A clone shares it. A charge past it fails without charging, and
/// the request ends with its status (an error, or a DL answer that says what it couldn't
/// decide), never the process.
#[derive(Debug, Clone)]
pub struct Envelope(Arc<Budget>);

impl Envelope {
    /// Shares this exact existing owner, without allocating a new budget or
    /// changing its limit or parent. Nested `part()` charges draw from it beside
    /// the caller's live allocations, including results that escape the call.
    pub fn from_budget(budget: Arc<Budget>) -> Self {
        Self(budget)
    }

    /// `limit` bytes (`None`: unlimited) within `server`'s budget.
    pub fn new(limit: Option<usize>, server: Option<Arc<SharedBudget>>) -> Self {
        Self(Arc::new(
            limit
                .map_or_else(Budget::unlimited, Budget::new)
                .within(server),
        ))
    }

    pub fn unlimited() -> Self {
        Self::new(None, None)
    }

    /// This envelope, if a charge to it can ever fail: holders that only exist to be
    /// refused (an engine's observation, a size estimate) skip an unbounded one.
    pub fn bounded(&self) -> Option<&Self> {
        self.0.is_bounded().then_some(self)
    }

    /// A budget for one part of the request: what the part holds, counted on its own and
    /// drawn from this envelope.
    pub fn part(&self) -> Budget {
        let mut part = Budget::unlimited();
        part.parent = Some(Parent::Envelope(self.clone()));
        part
    }
}

impl Default for Envelope {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl Deref for Envelope {
    type Target = Budget;

    fn deref(&self) -> &Budget {
        &self.0
    }
}

/// The same envelope, not an equal one: one request's.
impl PartialEq for Envelope {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Envelope {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub limit: usize,
    pub requested: usize,
    pub used: usize,
    /// The limit is the server's, for all queries together: this query would fit if
    /// others held less.
    pub shared: bool,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.shared {
            "the server's query memory is in use"
        } else {
            "query memory budget exceeded"
        };
        write!(
            f,
            "{what}: {} MiB in use, {} MiB more requested, limit {} MiB",
            self.used >> 20,
            self.requested >> 20,
            self.limit >> 20
        )
    }
}

impl std::error::Error for BudgetExceeded {}

impl Budget {
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            limit: limit_bytes,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            refused: AtomicBool::new(false),
            parent: None,
        }
    }

    pub fn unlimited() -> Self {
        Self::new(usize::MAX)
    }

    /// Also charges `shared`, the server's budget for all queries.
    #[must_use]
    pub fn within(mut self, shared: Option<Arc<SharedBudget>>) -> Self {
        self.parent = shared.map(Parent::Server);
        self
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
    /// Whether a charge can ever fail: a finite limit here or above.
    pub fn is_bounded(&self) -> bool {
        self.limit != usize::MAX
            || match &self.parent {
                Some(Parent::Server(_)) => true,
                Some(Parent::Envelope(envelope)) => envelope.0.is_bounded(),
                None => false,
            }
    }

    /// Reserves `bytes`, or fails without reserving anything.
    pub fn charge(&self, bytes: usize) -> Result<(), BudgetExceeded> {
        self.take(bytes)?;
        #[cfg(any(test, feature = "measure"))]
        crate::measure::charged(self, bytes);
        Ok(())
    }

    /// [`Self::charge`] here and above, unseen by a measurement: the caller tells it whose
    /// the bytes are.
    fn take(&self, bytes: usize) -> Result<(), BudgetExceeded> {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let Some(next) = used.checked_add(bytes).filter(|&next| next <= self.limit) else {
                return Err(self.refuse(bytes, used));
            };
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => {
                    self.peak.fetch_max(next, Ordering::Relaxed);
                    break;
                }
                Err(actual) => used = actual,
            }
        }
        let above = match &self.parent {
            Some(Parent::Server(shared)) => {
                shared.charge(bytes).map_err(|refused| BudgetExceeded {
                    limit: refused.limit,
                    requested: bytes,
                    used: refused.used,
                    shared: true,
                })
            }
            Some(Parent::Envelope(envelope)) => envelope.take(bytes),
            None => Ok(()),
        };
        if above.is_err() {
            self.used.fetch_sub(bytes, Ordering::Relaxed);
            self.refused.store(true, Ordering::Relaxed);
        }
        above
    }

    /// The refusal of `bytes`, recorded: for a request no budget could grant (a size that
    /// overflows).
    pub fn refusal(&self, bytes: usize) -> BudgetExceeded {
        self.refuse(bytes, self.used())
    }

    /// The refusal of `bytes` with `used` in use, recorded.
    fn refuse(&self, bytes: usize, used: usize) -> BudgetExceeded {
        self.refused.store(true, Ordering::Relaxed);
        BudgetExceeded {
            limit: self.limit,
            requested: bytes,
            used,
            shared: false,
        }
    }

    /// Whether a charge ever failed, here or above: what was computed under this budget
    /// may be limited by it (a request's own limit is no fact about the data, so a result
    /// it limited isn't published beyond the request).
    pub fn refused(&self) -> bool {
        self.refused.load(Ordering::Relaxed)
    }

    pub fn release(&self, bytes: usize) {
        let _released = self.give(bytes);
        #[cfg(any(test, feature = "measure"))]
        crate::measure::released(self, _released);
    }

    /// [`Self::release`] here and above, unseen by a measurement; what was released.
    fn give(&self, bytes: usize) -> usize {
        let bytes = bytes.min(self.used());
        self.used.fetch_sub(bytes, Ordering::Relaxed);
        self.release_above(bytes);
        bytes
    }

    fn release_above(&self, bytes: usize) {
        match &self.parent {
            Some(Parent::Server(shared)) => shared.release(bytes),
            Some(Parent::Envelope(envelope)) => {
                envelope.give(bytes);
            }
            None => {}
        }
    }

    /// The envelope this budget is a part of, if it is one: a measurement of the envelope
    /// counts its parts' charges ([`crate::measure`]).
    #[cfg(any(test, feature = "measure"))]
    pub(crate) fn above(&self) -> Option<&Budget> {
        match &self.parent {
            Some(Parent::Envelope(envelope)) => Some(envelope),
            _ => None,
        }
    }

    /// Bytes that may still be charged: the own rest, and no more than the levels above
    /// have left.
    pub fn remaining(&self) -> usize {
        let own = self.limit.saturating_sub(self.used());
        match &self.parent {
            Some(Parent::Server(shared)) => own.min(shared.remaining()),
            Some(Parent::Envelope(envelope)) => own.min(envelope.remaining()),
            None => own,
        }
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// The highest amount in use at any time.
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    /// Whether the rest is the server's and not the request's own (for the error message).
    pub fn bounded_by_shared(&self) -> bool {
        let own = self.limit.saturating_sub(self.used());
        match &self.parent {
            Some(Parent::Server(shared)) => shared.remaining() < own,
            Some(Parent::Envelope(envelope)) => {
                envelope.remaining() < own && envelope.bounded_by_shared()
            }
            None => false,
        }
    }
}

impl Drop for Budget {
    /// A part or request that ends, with a result or an error, holds nothing of the
    /// budgets above it.
    fn drop(&mut self) {
        let used = self.used();
        self.release_above(used);
        #[cfg(any(test, feature = "measure"))]
        crate::measure::released(self, used);
    }
}

/// Bytes charged to a budget that outlive the operation which charged them: released when
/// the reservation drops, however its holder ends. `B` borrows or shares the budget.
///
/// A measurement ([`crate::measure`]) tells reservations apart: it counts the charges and
/// releases of those made while it runs, and of no other.
#[derive(Debug)]
pub struct Reservation<B: Deref<Target = Budget> = Arc<Budget>> {
    budget: B,
    bytes: usize,
    /// The measurements' generation it was made in ([`crate::measure`]).
    #[cfg(any(test, feature = "measure"))]
    tag: u64,
}

impl<B: Deref<Target = Budget>> Reservation<B> {
    pub fn new(budget: B) -> Self {
        Self {
            budget,
            bytes: 0,
            #[cfg(any(test, feature = "measure"))]
            tag: crate::measure::tag(),
        }
    }

    /// Takes over `bytes` already charged to `budget` (by [`Budget::charge`]).
    pub fn adopt(budget: B, bytes: usize) -> Self {
        let mut held = Self::new(budget);
        held.bytes = bytes;
        #[cfg(any(test, feature = "measure"))]
        {
            crate::measure::reserved(&held.budget, held.tag, bytes);
            crate::measure::released(&held.budget, bytes);
        }
        held
    }

    /// Reserves `bytes` more, or fails without reserving anything.
    pub fn charge(&mut self, bytes: usize) -> Result<(), BudgetExceeded> {
        let Some(total) = self.bytes.checked_add(bytes) else {
            return Err(self.budget.refuse(bytes, self.budget.used()));
        };
        self.budget.take(bytes)?;
        self.bytes = total;
        #[cfg(any(test, feature = "measure"))]
        crate::measure::reserved(&self.budget, self.tag, bytes);
        Ok(())
    }

    /// Returns up to `bytes` of this reservation to the budget.
    pub fn release(&mut self, bytes: usize) {
        let bytes = bytes.min(self.bytes);
        let _released = self.budget.give(bytes);
        self.bytes -= bytes;
        #[cfg(any(test, feature = "measure"))]
        crate::measure::unreserved(&self.budget, self.tag, _released);
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The budget it charges.
    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    /// The handle it holds the budget by (an `Arc` to make more charges with).
    pub fn handle(&self) -> &B {
        &self.budget
    }

    /// Holds exactly `bytes`: charges the growth or returns the rest. A failed charge
    /// leaves what it held.
    pub fn resize(&mut self, bytes: usize) -> Result<(), BudgetExceeded> {
        match bytes.checked_sub(self.bytes) {
            Some(more) => self.charge(more),
            None => {
                self.release(self.bytes - bytes);
                Ok(())
            }
        }
    }

    /// Holds `bytes` in place of what it held, as a reservation made now: they are charged,
    /// and what it held is handed back as a reservation of its own, released when that
    /// drops. Until then both are held, as a reallocation holds the old block and the new.
    /// A refusal changes nothing.
    pub(crate) fn renew(&mut self, bytes: usize) -> Result<Reservation<&Budget>, BudgetExceeded> {
        self.budget.take(bytes)?;
        let old = Reservation {
            budget: &*self.budget,
            bytes: std::mem::replace(&mut self.bytes, bytes),
            #[cfg(any(test, feature = "measure"))]
            tag: std::mem::replace(&mut self.tag, crate::measure::tag()),
        };
        #[cfg(any(test, feature = "measure"))]
        crate::measure::reserved(old.budget, self.tag, bytes);
        Ok(old)
    }

    /// Holds `other`'s bytes too, of the same budget: nothing is charged or released.
    ///
    /// # Panics
    /// If `other` charges another budget.
    pub fn absorb(&mut self, mut other: Reservation<B>) {
        assert!(std::ptr::eq(&*self.budget, &*other.budget), "one budget");
        let bytes = std::mem::take(&mut other.bytes);
        self.bytes += bytes;
        #[cfg(any(test, feature = "measure"))]
        {
            crate::measure::reserved(&self.budget, self.tag, bytes);
            crate::measure::unreserved(&self.budget, other.tag, bytes);
        }
    }

    /// Combines one operation's tokens without changing their measurement origin.
    /// Borrows `other` so a failed check leaves its payload/token pair intact.
    pub(crate) fn absorb_same_origin(&mut self, other: &mut Reservation<B>) {
        assert!(std::ptr::eq(self.budget(), other.budget()), "one budget");
        #[cfg(any(test, feature = "measure"))]
        assert_eq!(self.tag, other.tag, "one measurement origin");
        let bytes = self.bytes.checked_add(other.bytes).expect("reserved bytes");
        self.bytes = bytes;
        other.bytes = 0;
    }

    /// Hands the bytes to a holder that releases them itself (with [`Budget::release`]);
    /// nothing is released.
    pub fn transfer(mut self) -> usize {
        let bytes = std::mem::take(&mut self.bytes);
        #[cfg(any(test, feature = "measure"))]
        {
            crate::measure::charged(&self.budget, bytes);
            crate::measure::unreserved(&self.budget, self.tag, bytes);
        }
        bytes
    }
}

impl Reservation<&Budget> {
    /// Retains the exact budget and original measurement origin, without readmission.
    /// No readmission or measurement retagging occurs. The caller keeps the promoted
    /// token with its original payload until that payload is freed.
    ///
    /// # Panics
    /// If `budget` is not the identical budget this reservation borrows.
    pub fn into_shared(mut self, budget: &Arc<Budget>) -> Reservation<Arc<Budget>> {
        assert!(std::ptr::eq(self.budget(), &**budget), "one budget");
        Reservation {
            budget: Arc::clone(budget),
            bytes: std::mem::take(&mut self.bytes),
            #[cfg(any(test, feature = "measure"))]
            tag: self.tag,
        }
    }
}

impl<B: Deref<Target = Budget>> Drop for Reservation<B> {
    fn drop(&mut self) {
        self.release(self.bytes);
    }
}

/// A vector whose memory is charged to a budget before it is allocated. Every change of its
/// capacity takes one path ([`ChargedVec::set_capacity`]): the new capacity is charged
/// first, beside the old (a reallocation holds both while it moves the items), then
/// allocated, then the old capacity released; one the budget has no room for fails without
/// allocating. It never holds more than it was charged, however a call ends: what it lends
/// a writer ([`ChargedVec::fill`]) can't grow it. Dropped, it frees the items, then
/// releases their charge; [`ChargedVec::into_parts`] hands the items out with their
/// reservation.
#[derive(Debug)]
pub struct ChargedVec<T, B: Deref<Target = Budget> = Arc<Budget>> {
    /// Before `held`: fields drop in order, the items first.
    items: Vec<T>,
    /// Exactly `items`' capacity in bytes.
    held: Reservation<B>,
}

impl<T, B: Deref<Target = Budget>> ChargedVec<T, B> {
    pub fn new(budget: B) -> Self {
        Self {
            items: Vec::new(),
            held: Reservation::new(budget),
        }
    }

    /// Room for `capacity` items, reserved first.
    pub fn with_capacity(budget: B, capacity: usize) -> Result<Self, BudgetExceeded> {
        let mut vec = Self::new(budget);
        vec.reserve_exact(capacity)?;
        Ok(vec)
    }

    /// `items`, at most `bound` of them: the room for `bound` reserved first.
    pub fn collect(
        budget: B,
        bound: usize,
        items: impl IntoIterator<Item = T>,
    ) -> Result<Self, BudgetExceeded> {
        let mut vec = Self::with_capacity(budget, bound)?;
        vec.extend_bounded(bound, items)?;
        Ok(vec)
    }

    /// The bytes of `capacity` items, if a vector can hold them.
    fn bytes(capacity: usize) -> Option<usize> {
        std::alloc::Layout::array::<T>(capacity)
            .ok()
            .map(|layout| layout.size())
    }

    /// Room for `additional` more items: the capacity doubles where the budget has room
    /// for it, else grows by what is asked.
    #[inline]
    pub fn reserve(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        match self.items.capacity() - self.items.len() >= additional {
            true => Ok(()),
            false => self.grow(additional),
        }
    }

    /// [`Self::reserve`] past the capacity: out of the line of the writes that don't need it.
    #[cold]
    #[inline(never)]
    fn grow(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        let needed = self.needed(additional)?;
        let doubled = needed.max(self.items.capacity().saturating_mul(2));
        let fits = Self::bytes(doubled).is_some_and(|bytes| bytes <= self.held.budget.remaining());
        self.set_capacity(if fits { doubled } else { needed })
    }

    /// Room for exactly `additional` more items.
    pub fn reserve_exact(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        let needed = self.needed(additional)?;
        match needed <= self.items.capacity() {
            true => Ok(()),
            false => self.set_capacity(needed),
        }
    }

    /// The length with `additional` more items, if it can be counted.
    fn needed(&self, additional: usize) -> Result<usize, BudgetExceeded> {
        let budget = &self.held.budget;
        self.items
            .len()
            .checked_add(additional)
            .ok_or_else(|| budget.refuse(usize::MAX, budget.used()))
    }

    /// The one path that changes the capacity, to `capacity` items (at least the length):
    /// charged beside the old capacity, the vector reallocated, the old capacity released.
    /// A capacity no vector can hold is refused as one the budget has no room for.
    fn set_capacity(&mut self, capacity: usize) -> Result<(), BudgetExceeded> {
        let budget = &self.held.budget;
        let bytes =
            Self::bytes(capacity).ok_or_else(|| budget.refuse(usize::MAX, budget.used()))?;
        // Released when it drops: after the reallocation, or as an unwinding one ends.
        let _old = self.held.renew(bytes)?;
        if capacity > self.items.capacity() {
            self.items.reserve_exact(capacity - self.items.len());
        } else {
            self.items.shrink_to(capacity);
        }
        debug_assert_eq!(Self::bytes(self.items.capacity()), Some(bytes));
        Ok(())
    }

    #[inline]
    pub fn push(&mut self, item: T) -> Result<(), BudgetExceeded> {
        self.reserve(1)?;
        self.items.push(item);
        Ok(())
    }

    #[inline]
    pub fn extend_from_slice(&mut self, items: &[T]) -> Result<(), BudgetExceeded>
    where
        T: Clone,
    {
        self.reserve(items.len())?;
        self.items.extend_from_slice(items);
        Ok(())
    }

    /// `items` appended, at most `bound` of them (an iterator that may yield more is cut
    /// there): the room for `bound` reserved first.
    #[inline]
    pub fn extend_bounded(
        &mut self,
        bound: usize,
        items: impl IntoIterator<Item = T>,
    ) -> Result<(), BudgetExceeded> {
        self.reserve(bound)?;
        let capacity = self.items.capacity();
        // At most `bound` items into room for `bound`: the vector never grows (and an
        // iterator of known length is written in one loop without capacity checks).
        self.items.extend(items.into_iter().take(bound));
        debug_assert_eq!(self.items.capacity(), capacity);
        Ok(())
    }

    /// `len` items, those past the current ones `value`'s clones.
    pub fn resize(&mut self, len: usize, value: T) -> Result<(), BudgetExceeded>
    where
        T: Clone,
    {
        self.reserve(len.saturating_sub(self.items.len()))?;
        self.items.resize(len, value);
        Ok(())
    }

    /// `fill` given room for `bound` more items, reserved first: for a writer that knows
    /// how many items it adds at most (a decoder that knows its bound). It can't grow the
    /// vector: an item past the room panics before anything is allocated.
    pub fn fill<R>(
        &mut self,
        bound: usize,
        fill: impl FnOnce(&mut Room<'_, T>) -> R,
    ) -> Result<R, BudgetExceeded> {
        self.reserve(bound)?;
        Ok(fill(&mut Room(&mut self.items, self.held.budget())))
    }

    pub fn truncate(&mut self, len: usize) {
        self.items.truncate(len);
    }

    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.items.retain(keep);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn capacity(&self) -> usize {
        self.items.capacity()
    }

    /// The bytes held: the capacity's.
    pub fn charged(&self) -> usize {
        self.held.bytes()
    }

    /// The items, and the reservation of their capacity, for a holder that keeps it until
    /// they are freed.
    pub fn into_parts(self) -> (Vec<T>, Reservation<B>) {
        let Self { items, held } = self;
        (items, held)
    }
}

impl<T, B: Deref<Target = Budget>> Deref for ChargedVec<T, B> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<T, B: Deref<Target = Budget>> std::ops::DerefMut for ChargedVec<T, B> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.items
    }
}

/// The most memory a hash map with room for `capacity` entries of `T` allocates (hashbrown's
/// layout): buckets the power of two at or above 8/7 of it (at least 4, or 8), an entry and
/// a control byte each, a group of control bytes more, and alignment.
pub fn map_bytes<T>(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let buckets = match capacity {
        0..4 => 4,
        4..8 => 8,
        _ => (capacity.saturating_mul(8) / 7).next_power_of_two(),
    };
    buckets
        .saturating_mul(size_of::<T>() + 1)
        .saturating_add(2 * 16)
}

/// Working memory of an operation (sort buckets, masks, hash chains), charged to its
/// budget before it grows: a [`ChargedVec`] borrowing the budget.
pub type Scratch<'b, T> = ChargedVec<T, &'b Budget>;

/// The room a [`ChargedVec`] lends a writer ([`ChargedVec::fill`]): items are added within
/// the capacity reserved, which it never grows.
#[derive(Debug)]
pub struct Room<'v, T>(&'v mut Vec<T>, &'v Budget);

impl<T> Room<'_, T> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Panics where no room is left.
    pub fn push(&mut self, item: T) {
        assert!(self.0.len() < self.0.capacity(), "past the room reserved");
        self.0.push(item);
    }

    /// Panics where there is no room for them.
    pub fn extend_from_slice(&mut self, items: &[T])
    where
        T: Clone,
    {
        assert!(
            items.len() <= self.0.capacity() - self.0.len(),
            "past the room reserved"
        );
        self.0.extend_from_slice(items);
    }

    /// The room left, to be written; [`Self::set_len`] then takes what was written.
    pub fn spare_capacity_mut(&mut self) -> &mut [std::mem::MaybeUninit<T>] {
        self.0.spare_capacity_mut()
    }

    /// As [`Vec::set_len`].
    ///
    /// # Safety
    /// `len` is at most the capacity, and the items up to it are initialised.
    pub unsafe fn set_len(&mut self, len: usize) {
        // SAFETY: the caller's.
        unsafe { self.0.set_len(len) }
    }

    /// `n` items after the room's, the `i`-th `item(i)`, written in parallel: each worker
    /// writes a range of slots of its own, and the length is published once, after every
    /// item is written. If `item` panics anywhere, every item written (in any range) is
    /// dropped once, no slot not written is read, the room holds what it held, and the
    /// panic goes on. Panics where the room has fewer than `n` slots left.
    pub fn par_fill(&mut self, n: usize, item: impl Fn(usize) -> T + Sync)
    where
        T: Send,
    {
        use rayon::prelude::*;
        let len = self.0.len();
        assert!(n <= self.0.capacity() - len, "past the room reserved");
        // The only pointer to the slots: no reference to them is made until the length is
        // published.
        let slots = Slots(self.0.as_mut_ptr().wrapping_add(len));
        let part = n.div_ceil(4 * rayon::current_num_threads()).max(1);
        let written = (0..n.div_ceil(part))
            .into_par_iter()
            .map(|p| {
                let mut written = Written::new(slots, p * part);
                for i in p * part..(p * part + part).min(n) {
                    let value = item(i);
                    // SAFETY: (`Slots`) slot `i` is this range's, and not yet written.
                    unsafe { slots.0.add(i).write(value) };
                    written.len += 1;
                }
                written
            })
            // Adjacent ranges, in order: one range of every item, or, where a worker
            // panicked, the ranges written dropped as the panic unwinds.
            .reduce(|| Written::new(slots, 0), Written::join);
        debug_assert_eq!((written.start, written.len), (0, n));
        // Every slot written: the items are the vector's.
        std::mem::forget(written);
        // SAFETY: the `n` slots after the items are written, within the capacity.
        unsafe { self.0.set_len(len + n) };
    }

    /// [`Self::par_fill`] with items that can fail: the first `Err` stops the fill, every
    /// worker seeing it (one relaxed load per item) before its next item. Every worker is
    /// joined before it returns; on an error every item written is dropped once, the room
    /// holds what it held, and the error of the lowest index observed is returned (workers
    /// stopped early may skip lower items, so it needn't be the one a sequential fill would
    /// meet first). A panic is handled as in [`Self::par_fill`]. Scratch headers draw on
    /// the destination's exact budget, admitted before allocation or dispatch. A refusal
    /// is converted to `E` before any item runs.
    pub fn try_par_fill<E: Send + From<BudgetExceeded>>(
        &mut self,
        n: usize,
        item: impl Fn(usize) -> Result<T, E> + Sync,
    ) -> Result<(), E>
    where
        T: Send,
    {
        parallel::try_fill(self, n, item)
    }
}

/// What one range of [`Room::try_par_fill`] leaves: the items it wrote, and the index and error
/// of the item it stopped at, if one failed.
type FillRange<T, E> = (Written<T>, Option<(usize, E)>);

/// The spare slots [`Room::par_fill`] writes, by one pointer taken from the vector before
/// the work splits.
///
/// SAFETY: the slots are `n` within the vector's capacity, and the vector is borrowed
/// mutably for the whole fill, so nothing else reaches them. Each slot index belongs to one
/// range, and each range to one worker, which alone writes it, and one [`Written`] token at
/// a time owns the items of a range (tokens are joined, never copied), so no slot is
/// written or dropped twice; a slot outside every token's range is never read. Hence the
/// pointer may be shared between the workers (`Send`, `Sync`) when the items may move
/// between threads (`T: Send`).
struct Slots<T>(*mut T);

impl<T> Clone for Slots<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Slots<T> {}

// SAFETY: see `Slots`.
unsafe impl<T: Send> Send for Slots<T> {}
// SAFETY: see `Slots`.
unsafe impl<T: Send> Sync for Slots<T> {}

/// The items written into slots `start..start + len` by [`Room::par_fill`], not yet the
/// vector's: they are dropped with this, unless it is forgotten once the vector takes them.
struct Written<T> {
    slots: Slots<T>,
    start: usize,
    len: usize,
}

impl<T> Written<T> {
    fn new(slots: Slots<T>, start: usize) -> Self {
        Self {
            slots,
            start,
            len: 0,
        }
    }

    /// `self` and the range right after it, as one.
    fn join(self, next: Self) -> Self {
        if self.len == 0 {
            return next;
        }
        if next.len == 0 {
            return self;
        }
        assert_eq!(self.start + self.len, next.start, "adjacent ranges");
        let joined = Self {
            slots: self.slots,
            start: self.start,
            len: self.len + next.len,
        };
        std::mem::forget((self, next));
        joined
    }
}

impl<T> Drop for Written<T> {
    fn drop(&mut self) {
        // SAFETY: (`Slots`) the `len` slots from `start` were written, and this alone owns them.
        unsafe {
            let first = self.slots.0.add(self.start);
            std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(first, self.len));
        }
    }
}

/// A [`Reservation`] that follows what its holder samples it holds, from any thread (a
/// DL engine's capacity estimate). It charges growth an eighth ahead, so a growing holder
/// reaches the budget's counters once per eighth rather than at every sample, and returns
/// what it no longer needs once the holder falls below half of it.
#[derive(Debug)]
pub struct Gauge<B: Deref<Target = Budget> = Arc<Budget>> {
    held: Mutex<Reservation<B>>,
    /// `held`'s bytes, read without the lock.
    reserved: AtomicUsize,
}

impl<B: Deref<Target = Budget>> Gauge<B> {
    pub fn new(budget: B) -> Self {
        Self {
            held: Mutex::new(Reservation::new(budget)),
            reserved: AtomicUsize::new(0),
        }
    }

    /// Records that the holder holds `bytes`; false if the budget has no room for them
    /// (the gauge then holds what it held). O(1); takes the lock only to change the charge.
    pub fn follow(&self, bytes: usize) -> bool {
        let reserved = self.reserved.load(Ordering::Relaxed);
        if bytes <= reserved && bytes >= reserved / 2 {
            return true;
        }
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        // An eighth ahead where the budget has room for it, so that only a charge of
        // what the holder holds can be refused.
        let ahead = bytes.saturating_add(bytes / 8);
        let target = match ahead.saturating_sub(held.bytes()) <= held.budget.remaining() {
            true => ahead,
            false => bytes,
        };
        let fits =
            bytes <= held.bytes() && bytes >= held.bytes() / 2 || held.resize(target).is_ok();
        self.reserved.store(held.bytes(), Ordering::Relaxed);
        fits
    }

    /// The bytes charged now.
    pub fn bytes(&self) -> usize {
        self.reserved.load(Ordering::Relaxed)
    }

    /// How much more than `held` the holder may come to hold: what it has reserved beyond
    /// it, and what the budget has left.
    pub fn room(&self, held: usize) -> usize {
        let held_now = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held_now
            .bytes()
            .saturating_sub(held)
            .saturating_add(held_now.budget.remaining())
    }
}

#[cfg(test)]
mod tests {
    use super::fill_tests::FillError;
    use super::*;

    #[test]
    fn n7_existing_budget_envelope_refuses_concurrent_overlap() {
        let root = Arc::new(Budget::new(1000));
        let envelope = Envelope::from_budget(Arc::clone(&root));
        assert!(Arc::ptr_eq(&envelope.0, &root));
        let mut caller = Reservation::new(&*root);
        caller.charge(200).unwrap();
        let barrier = std::sync::Barrier::new(3);
        let accepted = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let envelope = &envelope;
                let root = &root;
                let barrier = &barrier;
                let accepted = &accepted;
                scope.spawn(move || {
                    let child = envelope.part();
                    let Some(Parent::Envelope(parent)) = &child.parent else {
                        panic!("same owner")
                    };
                    assert!(Arc::ptr_eq(&parent.0, root));
                    let mut live = Reservation::new(&child);
                    barrier.wait();
                    if live.charge(600).is_ok() {
                        accepted.fetch_add(1, Ordering::SeqCst);
                    }
                    barrier.wait();
                    barrier.wait();
                    drop(live);
                });
            }
            barrier.wait();
            barrier.wait();
            assert_eq!(accepted.load(Ordering::SeqCst), 1);
            assert_eq!(root.used(), 800);
            barrier.wait();
        });
        assert_eq!(root.used(), 200);
        drop(caller);
        assert_eq!(root.used(), 0);
    }

    #[test]
    fn n7_existing_budget_envelope_keeps_escaping_result_charged() {
        let root = Arc::new(Budget::new(1024));
        let weak = Arc::downgrade(&root);
        let envelope = Envelope::from_budget(Arc::clone(&root));
        let child = Arc::new(envelope.part());
        let result = ChargedVec::<u64>::with_capacity(child, 128).unwrap();
        assert_eq!(root.used(), 1024); // One payload, not a duplicated charge.
        assert!(root.charge(1).is_err());
        drop(envelope);
        drop(root);
        assert_eq!(weak.upgrade().unwrap().used(), 1024);
        drop(result);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn a_shared_rebind_keeps_the_actual_budget_and_its_charge_alive() {
        let server = SharedBudget::new(60);
        let budget = Arc::new(Budget::new(60).within(Some(Arc::clone(&server))));
        let mut borrowed = Reservation::new(&*budget);
        borrowed.charge(60).unwrap();
        let shared = borrowed.into_shared(&budget);
        assert!(std::ptr::eq(shared.budget(), &*budget));
        assert_eq!((shared.bytes(), budget.used(), budget.peak()), (60, 60, 60));
        assert_eq!(Arc::strong_count(&budget), 2);
        drop(budget);
        assert_eq!(server.used(), 60);
        drop(shared);
        assert_eq!(server.used(), 0);
    }

    #[test]
    fn a_shared_rebind_rejects_a_sibling_budget_without_losing_its_token() {
        let envelope = Envelope::unlimited();
        let budget = Arc::new(envelope.part());
        let sibling = Arc::new(envelope.part());
        let mut unrelated = Reservation::new(&*budget);
        unrelated.charge(7).unwrap();
        let mut borrowed = Reservation::new(&*budget);
        borrowed.charge(60).unwrap();
        let failed = std::panic::catch_unwind(|| borrowed.into_shared(&sibling));
        assert!(failed.is_err());
        assert_eq!((budget.used(), sibling.used(), envelope.used()), (7, 0, 7));
        assert_eq!(
            (Arc::strong_count(&budget), Arc::strong_count(&sibling)),
            (1, 1)
        );
        drop(unrelated);
        assert_eq!(envelope.used(), 0);
    }

    /// A retagging transfer/adopt mutant gives an old token new credit and hides the
    /// second deliberately uncharged probe. Compare separate measurements so the first
    /// probe's peak cannot mask that false pass.
    #[test]
    fn a_shared_rebind_does_not_cover_a_new_probe_with_old_credit() {
        if !crate::measure::alone() {
            return;
        }
        let budget = Arc::new(Budget::new(4096));
        let old = Scratch::<u8>::with_capacity(&*budget, 4096)
            .unwrap()
            .into_parts();
        let probe = || {
            let bytes = vec![0u8; 1024];
            std::hint::black_box(&bytes);
            drop(bytes);
        };
        let ((), before) = crate::measure::measure_within(&budget, probe);
        let ((), after) = crate::measure::measure_within(&budget, || {
            let pair = (old.0, old.1.into_shared(&budget));
            probe();
            drop(pair);
        });
        assert_eq!((before, after, budget.used()), (1024, 1024, 0));
    }

    /// A worker that panics after other ranges are written leaves nothing behind: every
    /// item written is dropped once, the vector holds what it held, and the items' memory
    /// (charged to the budget, so a leak would stay charged) is all released. The text
    /// filter's fill published nothing and leaked them. Filled without a panic, it holds
    /// nothing uncharged beside the items.
    #[test]
    fn a_panicking_parallel_fill_drops_every_item_it_wrote() {
        if !crate::measure::alone() {
            return;
        }
        static MADE: AtomicUsize = AtomicUsize::new(0);
        static DROPPED: AtomicUsize = AtomicUsize::new(0);
        /// An item holding memory charged to the budget, counting its drops.
        struct Item<'b>(#[allow(dead_code)] ChargedVec<u8, &'b Budget>);
        impl Drop for Item<'_> {
            fn drop(&mut self) {
                DROPPED.fetch_add(1, Ordering::SeqCst);
            }
        }
        let budget = Budget::unlimited();
        let item = |i: usize, panics_at: usize| {
            assert!(i != panics_at, "the worker at item {i} panics");
            MADE.fetch_add(1, Ordering::SeqCst);
            Item(ChargedVec::with_capacity(&budget, 1_000).unwrap())
        };
        // One thread: the ranges are written in order, so the panic comes after some.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let mut items: ChargedVec<Item<'_>, &Budget> = ChargedVec::new(&budget);
        items.push(Item(ChargedVec::new(&budget))).unwrap();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            items.fill(100, |room| {
                pool.install(|| room.par_fill(100, |i| item(i, 60)))
            })
        }));
        assert!(unwound.is_err());
        assert_eq!(MADE.load(Ordering::SeqCst), 60, "earlier ranges completed");
        assert_eq!(DROPPED.load(Ordering::SeqCst), 60, "each dropped once");
        assert_eq!(items.len(), 1);
        assert_eq!(budget.used(), items.charged(), "the items' memory released");
        let ((), excess) = crate::measure::measure_within(&budget, || {
            items
                .fill(100, |room| {
                    pool.install(|| room.par_fill(100, |i| item(i, usize::MAX)))
                })
                .unwrap();
        });
        assert_eq!((items.len(), excess), (101, 0));
    }

    /// A fill whose item fails stops its siblings and leaves nothing behind: every item
    /// written is dropped once, the vector holds what it held, the items' memory is released,
    /// and the error returned is the lowest index observed. Without a failure it publishes
    /// every item.
    #[test]
    fn a_failing_parallel_fill_stops_and_drops_every_item_it_wrote() {
        if !crate::measure::alone() {
            return;
        }
        super::fill_tests::header_admission_and_lifetimes();
        static MADE: AtomicUsize = AtomicUsize::new(0);
        static DROPPED: AtomicUsize = AtomicUsize::new(0);
        struct Item<'b>(#[allow(dead_code)] ChargedVec<u8, &'b Budget>);
        impl Drop for Item<'_> {
            fn drop(&mut self) {
                DROPPED.fetch_add(1, Ordering::SeqCst);
            }
        }
        let budget = Budget::unlimited();
        let item = |i: usize, fails: &[usize]| {
            if fails.contains(&i) {
                return Err(FillError::Item(i));
            }
            MADE.fetch_add(1, Ordering::SeqCst);
            Ok(Item(ChargedVec::with_capacity(&budget, 100).unwrap()))
        };
        let fill = |threads: usize, fails: &[usize]| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let mut items: ChargedVec<Item<'_>, &Budget> = ChargedVec::new(&budget);
            items.push(Item(ChargedVec::new(&budget))).unwrap();
            let filled = items
                .fill(100, |room| {
                    pool.install(|| room.try_par_fill(100, |i| item(i, fails)))
                })
                .unwrap();
            (filled, items.len())
        };
        // One thread: the ranges in order, so the failure at 60 stops everything after it.
        assert_eq!(fill(1, &[60, 80]), (Err(FillError::Item(60)), 1));
        assert_eq!(MADE.load(Ordering::SeqCst), 60);
        assert_eq!(
            DROPPED.load(Ordering::SeqCst),
            60 + 1,
            "each once, then the vector"
        );
        // Several threads: whichever failures were observed, the lowest is returned, and
        // every item made is dropped once.
        let made = MADE.load(Ordering::SeqCst);
        let (filled, len) = fill(4, &[30, 70]);
        assert!(
            matches!(filled, Err(FillError::Item(30 | 70))),
            "{filled:?}"
        );
        assert_eq!(len, 1);
        assert_eq!(
            DROPPED.load(Ordering::SeqCst) - 61,
            MADE.load(Ordering::SeqCst) - made + 1
        );
        assert_eq!(budget.used(), 0, "the items' memory released");
        assert_eq!(fill(4, &[]), (Ok(()), 101));
    }

    /// Two failures both observed return the lower: items 2 and 6 of 8, a range each on two
    /// workers (each half of the fill on its own), each waiting until the other has started,
    /// for at most 5 seconds, so that neither stops the other first. And a worker that panics
    /// while another fails unwinds out of the fill with every item made dropped once, the
    /// vector as it was and the items' memory released (Sol, w1-step1 r1, F1).
    #[test]
    fn a_parallel_fill_returns_its_lowest_failure_and_unwinds_past_one() {
        use std::time::{Duration, Instant};
        static MADE: AtomicUsize = AtomicUsize::new(0);
        static DROPPED: AtomicUsize = AtomicUsize::new(0);
        struct Item<'b>(#[allow(dead_code)] ChargedVec<u8, &'b Budget>);
        impl Drop for Item<'_> {
            fn drop(&mut self) {
                DROPPED.fetch_add(1, Ordering::SeqCst);
            }
        }
        let budget = Budget::unlimited();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let started = AtomicUsize::new(0);
        let meet = || {
            started.fetch_add(1, Ordering::SeqCst);
            let since = Instant::now();
            while started.load(Ordering::SeqCst) < 2 && since.elapsed() < Duration::from_secs(5) {
                std::thread::yield_now();
            }
        };
        let made = || {
            MADE.fetch_add(1, Ordering::SeqCst);
            Ok(Item(ChargedVec::with_capacity(&budget, 100).unwrap()))
        };
        let mut items: ChargedVec<Item<'_>, &Budget> = ChargedVec::new(&budget);
        items.push(Item(ChargedVec::new(&budget))).unwrap();
        let filled = items
            .fill(8, |room| {
                pool.install(|| {
                    room.try_par_fill(8, |i| match i {
                        2 | 6 => {
                            meet();
                            Err(FillError::Item(i))
                        }
                        _ => made(),
                    })
                })
            })
            .unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 2, "both failures observed");
        assert_eq!(filled, Err(FillError::Item(2)), "the lower of the two");
        assert_eq!(items.len(), 1);
        assert_eq!(DROPPED.load(Ordering::SeqCst), MADE.load(Ordering::SeqCst));
        assert_eq!(budget.used(), items.charged(), "the items' memory released");
        // Item 2 panics once item 6 has started, and item 6 fails.
        started.store(0, Ordering::SeqCst);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            items.fill(8, |room| {
                pool.install(|| {
                    room.try_par_fill(8, |i| match i {
                        2 => {
                            meet();
                            panic!("the worker at item 2 panics");
                        }
                        6 => {
                            meet();
                            Err(FillError::Item(i))
                        }
                        _ => made(),
                    })
                })
            })
        }));
        assert!(unwound.is_err(), "the panic unwinds out of the fill");
        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "the failure observed too"
        );
        assert_eq!(items.len(), 1, "the vector as it was");
        assert_eq!(
            DROPPED.load(Ordering::SeqCst),
            MADE.load(Ordering::SeqCst),
            "each item made dropped once"
        );
        assert_eq!(budget.used(), items.charged(), "the items' memory released");
    }

    /// A charged vector never holds more than it was charged: every path that grows it
    /// reserves first, a writer it lends can't grow it, a capacity no vector can hold is
    /// refused, and its items leave with their reservation.
    #[test]
    fn a_charged_vector_never_holds_more_than_it_was_charged() {
        if !crate::measure::alone() {
            return;
        }
        let budget = Budget::new(1 << 20);
        let mut vec: ChargedVec<u64, &Budget> = ChargedVec::collect(&budget, 4, 0..10).unwrap();
        // A writer past what it said it would add panics before the vector grows.
        let past = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vec.fill(0, |room| room.push(4))
        }));
        assert!(past.is_err());
        assert_eq!((vec.capacity(), budget.used()), (4, 32));
        let ((), excess) = crate::measure::measure_within(&budget, || {
            vec.push(4).unwrap();
            vec.extend_from_slice(&[5; 100]).unwrap();
            vec.resize(300, 6).unwrap();
            vec.fill(50, |room| room.extend_from_slice(&[7; 50]))
                .unwrap();
            assert!(vec.reserve_exact(usize::MAX - vec.len()).is_err());
            assert_eq!(budget.used(), vec.capacity() * 8);
            drop(vec);
        });
        assert_eq!((excess, budget.used()), (0, 0));
        // The items keep their charge to the server once the last other holder of their
        // budget is gone.
        let server = SharedBudget::new(1 << 20);
        let part = Arc::new(Budget::unlimited().within(Some(Arc::clone(&server))));
        let (items, held) = ChargedVec::<u8>::with_capacity(part, 1_000)
            .unwrap()
            .into_parts();
        assert_eq!(server.used(), 1_000);
        drop(items);
        drop(held);
        assert_eq!(server.used(), 0);
    }

    #[test]
    fn a_reservation_releases_what_it_holds_when_dropped() {
        let budget = Arc::new(Budget::new(100));
        let mut held = Reservation::new(Arc::clone(&budget));
        held.charge(60).unwrap();
        assert!(held.charge(50).is_err());
        assert_eq!(held.bytes(), 60);
        held.release(20);
        assert_eq!(budget.used(), 40);
        budget.charge(10).unwrap();
        let adopted = Reservation::adopt(&*budget, 10);
        drop(held);
        assert_eq!(budget.used(), 10);
        assert_eq!(adopted.transfer(), 10);
        assert_eq!(budget.used(), 10, "a transfer releases nothing");
    }

    #[test]
    fn charges_up_to_the_limit_and_tracks_the_peak() {
        let budget = Budget::new(100);
        budget.charge(60).unwrap();
        let err = budget.charge(50).unwrap_err();
        assert_eq!((err.used, err.requested, err.shared), (60, 50, false));
        budget.release(60);
        budget.charge(100).unwrap();
        assert_eq!(budget.peak(), 100);
    }

    #[test]
    fn queries_share_the_servers_budget_and_return_it_when_they_end() {
        let server = SharedBudget::new(100);
        let first = Budget::new(80).within(Some(Arc::clone(&server)));
        let second = Budget::new(80).within(Some(Arc::clone(&server)));
        first.charge(70).unwrap();
        assert_eq!(second.remaining(), 30);
        // The second query is within its own limit, and the server has no room for it.
        let err = second.charge(40).unwrap_err();
        assert_eq!(
            (err.shared, err.limit, err.used, err.requested),
            (true, 100, 70, 40)
        );
        assert_eq!(second.used(), 0, "a failed charge reserves nothing");
        // Its own limit is reported as its own.
        let err = first.charge(20).unwrap_err();
        assert_eq!((err.shared, err.limit), (false, 80));
        second.charge(30).unwrap();
        assert_eq!(server.used(), 100);
        first.release(20);
        assert_eq!(server.used(), 80);
        // A query that ends, however, returns what it still holds.
        drop(first);
        assert_eq!(server.used(), 30);
        drop(second);
        assert_eq!((server.used(), server.peak()), (0, 100));
    }

    /// The process's budget above the queries' and maintenance's: each level refuses
    /// what doesn't fit it, a refusal leaves nothing charged at any level, and what is
    /// released is released all the way up.
    #[test]
    fn shared_budgets_nest_within_the_processs() {
        let process = SharedBudget::new(100);
        let queries = SharedBudget::within(80, Arc::clone(&process));
        let query = Budget::new(usize::MAX).within(Some(Arc::clone(&queries)));
        let maintenance = Budget::new(usize::MAX).within(Some(Arc::clone(&process)));
        query.charge(70).unwrap();
        assert_eq!((queries.used(), process.used()), (70, 70));
        // The queries' own limit refuses, the process's is left as it was.
        let err = query.charge(20).unwrap_err();
        assert_eq!((err.shared, err.limit, err.used), (true, 80, 70));
        assert_eq!((query.used(), queries.used(), process.used()), (70, 70, 70));
        // Maintenance takes what the process has left; then the process refuses the
        // queries what their own limit would allow.
        maintenance.charge(25).unwrap();
        assert_eq!((query.remaining(), queries.remaining()), (5, 5));
        let err = query.charge(8).unwrap_err();
        assert_eq!((err.shared, err.limit, err.used), (true, 100, 95));
        assert_eq!(
            (queries.used(), process.used()),
            (70, 95),
            "nothing charged"
        );
        assert!(query.bounded_by_shared());
        // Released, and ended, all the way up.
        query.release(30);
        assert_eq!((queries.used(), process.used()), (40, 65));
        drop(maintenance);
        drop(query);
        assert_eq!((queries.used(), process.used()), (0, 0));
        assert_eq!((queries.peak(), process.peak()), (70, 95));
    }

    #[test]
    fn a_requests_parts_draw_from_its_envelope_within_the_servers_budget() {
        let server = SharedBudget::new(100);
        let request = Envelope::new(Some(60), Some(Arc::clone(&server)));
        let (first, second) = (request.part(), request.part());
        first.charge(40).unwrap();
        // Each part counts what it holds; the envelope, all of them.
        assert_eq!((first.used(), request.used(), server.used()), (40, 40, 40));
        assert_eq!(second.remaining(), 20);
        let err = second.charge(30).unwrap_err();
        assert_eq!((err.shared, err.limit, err.used), (false, 60, 40));
        assert_eq!((second.used(), request.used()), (0, 40), "nothing charged");
        assert!(second.refused() && request.refused() && !first.refused());
        assert!(request.bounded().is_some() && second.is_bounded());
        assert!(
            Envelope::unlimited().bounded().is_none() && !Envelope::unlimited().part().is_bounded()
        );
        assert!(
            Envelope::new(None, Some(Arc::clone(&server)))
                .bounded()
                .is_some()
        );
        // The server's limit, reached through another request, is reported as the server's.
        let other = Envelope::new(None, Some(Arc::clone(&server)));
        other.charge(50).unwrap();
        assert!(second.bounded_by_shared());
        assert!(second.charge(15).unwrap_err().shared);
        drop(other);
        // A reservation follows what its holder holds.
        let mut held = Reservation::new(request.clone());
        held.resize(15).unwrap();
        assert!(held.resize(25).is_err(), "past the envelope");
        assert_eq!(held.bytes(), 15);
        held.resize(5).unwrap();
        assert_eq!((request.used(), server.used()), (45, 45));
        // A part that ends returns what it holds to every level above it.
        drop(first);
        drop(held);
        assert_eq!((request.used(), server.used()), (0, 0));
        assert_eq!(request.peak(), 55);
    }

    #[test]
    fn a_gauge_charges_growth_ahead_and_returns_what_its_holder_gave_up() {
        let request = Envelope::new(Some(1_000), None);
        let gauge = Gauge::new(request.clone());
        assert!(gauge.follow(400));
        assert_eq!(gauge.bytes(), 450, "an eighth ahead");
        assert!(gauge.follow(440));
        assert_eq!(request.used(), 450, "within what it holds: no charge");
        // Past the envelope with the eighth, within it without.
        assert!(gauge.follow(950));
        assert_eq!(gauge.bytes(), 950);
        assert!(!request.refused(), "no charge ahead was refused");
        assert!(!gauge.follow(1_001));
        assert_eq!(request.used(), 950, "a failed charge keeps what it held");
        assert!(gauge.follow(100));
        assert_eq!(request.used(), 112);
        drop(gauge);
        assert_eq!(request.used(), 0);
    }
}

#[cfg(test)]
#[path = "budget/fill_tests.rs"]
mod fill_tests;

#[path = "budget/parallel.rs"]
mod parallel;
