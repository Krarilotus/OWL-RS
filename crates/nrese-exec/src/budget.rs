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

    /// Up to `wanted` bytes charged here and above, never more than half of the room left
    /// at a level, never refused: what was taken (0 where there is no room).
    fn take_up_to(&self, wanted: usize) -> usize {
        let take = take_half_of_room(&self.used, &self.peak, self.limit, wanted);
        if take == 0 {
            return 0;
        }
        let got = self
            .parent
            .as_ref()
            .map_or(take, |parent| parent.take_up_to(take));
        if got < take {
            self.used.fetch_sub(take - got, Ordering::Relaxed);
        }
        got
    }
}

/// Up to `wanted` bytes added to `used`, never more than half of what `limit` leaves of it:
/// one CAS loop, never refused, nothing recorded. What was added.
fn take_half_of_room(used: &AtomicUsize, peak: &AtomicUsize, limit: usize, wanted: usize) -> usize {
    let mut now = used.load(Ordering::Relaxed);
    loop {
        let take = wanted.min(limit.saturating_sub(now) / 2);
        if take == 0 {
            return 0;
        }
        match used.compare_exchange_weak(now, now + take, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => {
                peak.fetch_max(now + take, Ordering::Relaxed);
                return take;
            }
            Err(actual) => now = actual,
        }
    }
}

/// The calls a thread makes on budgets ([`Budget`]'s own-level take and give, its
/// refills), counted for tests: an item charged within a [`Grant`]'s spare must make none.
#[cfg(test)]
mod calls {
    use std::cell::Cell;

    thread_local! {
        static CALLS: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn count() {
        CALLS.with(|calls| calls.set(calls.get() + 1));
    }

    /// The calls this thread has made so far.
    pub(super) fn made() -> u64 {
        CALLS.with(Cell::get)
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
    /// The spare this budget's [`Grant`]s hold ([`Self::ahead`]). Changed only when a grant
    /// refills, gives back or ends, never per item.
    ahead: AtomicUsize,
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
            ahead: AtomicUsize::new(0),
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
        self.take_own(bytes)?;
        let above = self.take_above(bytes);
        if above.is_err() {
            self.used.fetch_sub(bytes, Ordering::Relaxed);
        }
        above
    }

    /// `bytes` charged at this level only: refused, and recorded, where its own limit
    /// hasn't room.
    fn take_own(&self, bytes: usize) -> Result<(), BudgetExceeded> {
        #[cfg(test)]
        calls::count();
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
                    return Ok(());
                }
                Err(actual) => used = actual,
            }
        }
    }

    /// `bytes` charged at the levels above exactly: refused where one hasn't room, which
    /// is recorded here too. Nothing to charge above a root.
    fn take_above(&self, bytes: usize) -> Result<(), BudgetExceeded> {
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
            self.refused.store(true, Ordering::Relaxed);
        }
        above
    }

    /// Up to `wanted` bytes charged at the levels above, never more than half of the room
    /// left at a level: never refused and never recorded as refused. What was taken (0
    /// above a root).
    fn take_above_up_to(&self, wanted: usize) -> usize {
        match &self.parent {
            Some(Parent::Server(shared)) => shared.take_up_to(wanted),
            Some(Parent::Envelope(envelope)) => envelope.take_up_to(wanted),
            None => 0,
        }
    }

    /// `bytes` charged here and at every level above where every level has room for them, else
    /// nothing; never recorded as refused, never waiting: an optional charge (a vector's
    /// doubling beyond what it asks, [`hold::Charge::renew_up_to`]). Whether it was taken.
    pub(crate) fn try_take(&self, bytes: usize) -> bool {
        #[cfg(test)]
        calls::count();
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let Some(next) = used.checked_add(bytes).filter(|&next| next <= self.limit) else {
                return false;
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
            Some(Parent::Server(shared)) => shared.charge(bytes).is_ok(),
            Some(Parent::Envelope(envelope)) => envelope.try_take(bytes),
            None => true,
        };
        if !above {
            self.used.fetch_sub(bytes, Ordering::Relaxed);
        }
        above
    }

    /// Up to `wanted` bytes charged here and at every level above, never more than half of
    /// the room left at a level: never refused and never recorded as refused, never
    /// waiting. What was taken. A [`Grant`]'s refill.
    pub(crate) fn take_up_to(&self, wanted: usize) -> usize {
        #[cfg(test)]
        calls::count();
        let take = take_half_of_room(&self.used, &self.peak, self.limit, wanted);
        if take == 0 {
            return 0;
        }
        let got = match &self.parent {
            Some(_) => self.take_above_up_to(take),
            None => take,
        };
        if got < take {
            self.used.fetch_sub(take - got, Ordering::Relaxed);
        }
        got
    }

    /// How much spare the [`Grant`]s on this budget may hold together: none but below an
    /// envelope (a request's part), so the server holds nothing ahead once the operation
    /// holding the grants ends; 64 KiB, and no more than a 64th of the envelope's limit, so
    /// siblings below a small envelope don't starve one another; none where that would be
    /// under 1 KiB, and every charge stays exact. Each grant holds this over its operator's
    /// workers ([`Grant::with_workers`]). The declared slack: at an envelope (and through it
    /// at the server) at most its live parts times this.
    fn grant_cap(&self) -> usize {
        const MOST: usize = 64 << 10;
        const LEAST: usize = 1 << 10;
        let cap = match &self.parent {
            Some(Parent::Envelope(envelope)) => MOST.min(envelope.limit() / 64),
            _ => 0,
        };
        if cap < LEAST { 0 } else { cap }
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
        let bytes = self.give_own(bytes);
        self.release_above(bytes);
        bytes
    }

    /// Up to `bytes` released at this level only: what was released.
    fn give_own(&self, bytes: usize) -> usize {
        #[cfg(test)]
        calls::count();
        let bytes = bytes.min(self.used());
        self.used.fetch_sub(bytes, Ordering::Relaxed);
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

    /// The spare this budget's live [`Grant`]s hold, charged here and above beyond their
    /// items: at most this (it follows each grant's refills, give-backs and end, not its
    /// items, so items drawn from a spare since leave it higher), 0 once they have ended.
    pub fn ahead(&self) -> usize {
        self.ahead.load(Ordering::Relaxed)
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

    /// Reserves `bytes` more, or fails without reserving anything. Nothing, it reserves
    /// without a call up the budget's levels: an empty table's charge, resize and release cost
    /// nothing (D16-039's timing: three such calls an empty scan, NOT EXISTS ×1.06).
    pub fn charge(&mut self, bytes: usize) -> Result<(), BudgetExceeded> {
        if bytes == 0 {
            return Ok(());
        }
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
        // Nothing released, no call up the levels (a reservation dropped empty, after
        // [`Self::transfer`] too).
        if bytes == 0 {
            return;
        }
        let _released = self.budget.give(bytes);
        self.bytes -= bytes;
        #[cfg(any(test, feature = "measure"))]
        crate::measure::unreserved(&self.budget, self.tag, _released);
    }

    pub fn bytes(&self) -> usize {
        self.bytes
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
    fn renew(&mut self, bytes: usize) -> Result<Reservation<&Budget>, BudgetExceeded> {
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

    /// Takes over `other`'s bytes, charged to the same budget: this reservation releases
    /// them with its own.
    ///
    /// # Panics
    /// If `other` is a reservation on another budget.
    pub fn absorb(&mut self, mut other: Reservation<B>) {
        assert!(
            std::ptr::eq(&*self.budget, &*other.budget),
            "absorb: a reservation on another budget"
        );
        let bytes = std::mem::take(&mut other.bytes);
        // To a measurement the bytes stay covered throughout (sol-loop r5 G1): nothing moves
        // between equal tags, and otherwise the new cover comes before the old one goes.
        #[cfg(any(test, feature = "measure"))]
        {
            if self.tag != other.tag {
                crate::measure::reserved(&self.budget, self.tag, bytes);
                crate::measure::unreserved(&other.budget, other.tag, bytes);
            }
        }
        self.bytes += bytes;
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

impl<B: Deref<Target = Budget>> Drop for Reservation<B> {
    fn drop(&mut self) {
        self.release(self.bytes);
    }
}

/// Bytes charged one item at a time (an aggregate's values, ORDER BY's computed keys,
/// CONSTRUCT's triples) from spare the grant holds charged at every level: an item within
/// it costs one local subtraction, no atomic, no call up the levels (the executor memory
/// stack's timing: a shared atomic per item cost GROUP_CONCAT up to x1.94 in parallel
/// workers). Past the spare the item is charged exactly ([`Budget`]'s `take`, refused and
/// recorded where it doesn't fit), then the spare refilled with [`Budget::take_up_to`]:
/// never refused, never recorded as refused, never more than half of what a level has
/// left, never waiting. Nothing is shared between grants, so nothing races. The owner of
/// the budget types ruled this shape (3 Nov); a `Grant` is the executor's one way to
/// charge items.
///
/// A grant's spare is at most its budget's [`Budget::grant_cap`] divided by the workers
/// its operator runs on, none where that is under 1 KiB: an envelope's part holds at most
/// one cap of spare across the grants live on it at once (a grant held across a rayon
/// `join` may have a stolen sibling's grant live on the same thread, so the bound is per
/// concurrently live grant). The spare is the declared slack at the part and above.
#[derive(Debug)]
pub struct Grant<B: Deref<Target = Budget> = Arc<Budget>> {
    budget: B,
    /// What the grant's items hold, charged at every level.
    own: usize,
    /// Charged at every level beyond `own`: room for the next items.
    spare: usize,
    /// What it counts in its budget's [`Budget::ahead`]: `spare` as of its last refill,
    /// give-back or end (items drawn from the spare don't touch it).
    counted: usize,
    cap: usize,
    /// The measurements' generation it was made in ([`crate::measure`]).
    #[cfg(any(test, feature = "measure"))]
    tag: u64,
}

impl<B: Deref<Target = Budget>> Grant<B> {
    /// A grant for an operator running on the global pool's threads.
    pub fn new(budget: B) -> Self {
        Self::with_workers(budget, rayon::current_num_threads())
    }

    /// A grant for an operator running on `workers` threads, each with a grant of its own.
    pub fn with_workers(budget: B, workers: usize) -> Self {
        const LEAST: usize = 1 << 10;
        let cap = budget.grant_cap() / workers.max(1);
        Self {
            budget,
            own: 0,
            spare: 0,
            counted: 0,
            cap: if cap < LEAST { 0 } else { cap },
            #[cfg(any(test, feature = "measure"))]
            tag: crate::measure::tag(),
        }
    }

    /// Charges `bytes`, or fails without charging anything: from the spare, a local
    /// subtraction; past it the rest charged exactly at every level (refused there as any
    /// charge), then a refill.
    pub fn take(&mut self, bytes: usize) -> Result<(), BudgetExceeded> {
        if let Some(left) = self.spare.checked_sub(bytes) {
            self.spare = left;
        } else {
            self.budget.take(bytes - self.spare)?;
            self.set_spare(0);
            if self.cap > 0 {
                let got = self.budget.take_up_to(self.cap);
                self.set_spare(got);
            }
        }
        self.own += bytes;
        #[cfg(any(test, feature = "measure"))]
        crate::measure::reserved(&self.budget, self.tag, bytes);
        Ok(())
    }

    /// Up to `wanted` more bytes for its items (a whole number of `unit`s), without refusal:
    /// from its spare first, then the rest whole where every level has room for it
    /// ([`Budget::try_take`]), else as far as half of the room left at a level allows
    /// ([`Budget::take_up_to`]); never refused nor recorded as refused. What it took
    /// ([`Share`]'s optional growth).
    fn take_up_to(&mut self, wanted: usize, unit: usize) -> usize {
        // Within the spare (a small buffer's growth, nothing wanted included): a subtraction.
        if wanted <= self.spare {
            self.spare -= wanted;
            self.own += wanted;
            #[cfg(any(test, feature = "measure"))]
            crate::measure::reserved(&self.budget, self.tag, wanted);
            return wanted;
        }
        let unit = unit.max(1);
        let from_spare = wanted.min(self.spare) / unit * unit;
        self.spare -= from_spare;
        let rest = wanted - from_spare;
        let above = match rest {
            0 => 0,
            rest if self.budget.try_take(rest) => rest,
            rest => {
                let above = self.budget.take_up_to(rest);
                // Whole units: the part of one returned at once.
                let part = above % unit;
                if part > 0 {
                    self.budget.give(part);
                }
                above - part
            }
        };
        let got = from_spare + above;
        self.own += got;
        #[cfg(any(test, feature = "measure"))]
        crate::measure::reserved(&self.budget, self.tag, got);
        got
    }

    /// Gives up to `bytes` back: kept as spare up to the cap, the rest released at every
    /// level.
    pub fn give_back(&mut self, bytes: usize) {
        let bytes = bytes.min(self.own);
        if bytes == 0 {
            return;
        }
        self.own -= bytes;
        let kept = bytes.min(self.cap.saturating_sub(self.spare));
        if kept < bytes {
            self.budget.give(bytes - kept);
        }
        self.set_spare(self.spare + kept);
        #[cfg(any(test, feature = "measure"))]
        crate::measure::unreserved(&self.budget, self.tag, bytes);
    }

    /// What the grant holds charged: its items'.
    pub fn bytes(&self) -> usize {
        self.own
    }

    /// The spare set to `spare`, its budget's [`Budget::ahead`] following: one atomic, at a
    /// refill, a give-back or the end, never per item.
    fn set_spare(&mut self, spare: usize) {
        self.spare = spare;
        if spare > self.counted {
            self.budget
                .ahead
                .fetch_add(spare - self.counted, Ordering::Relaxed);
        } else if spare < self.counted {
            self.budget
                .ahead
                .fetch_sub(self.counted - spare, Ordering::Relaxed);
        }
        self.counted = spare;
    }

    /// The spare released at every level.
    fn release_ahead(&mut self) {
        let spare = self.spare;
        self.set_spare(0);
        if spare > 0 {
            self.budget.give(spare);
        }
    }

    /// Releases everything: its items' charge and its spare.
    pub fn trim(&mut self) {
        let own = std::mem::take(&mut self.own);
        if own > 0 {
            self.budget.give(own);
            #[cfg(any(test, feature = "measure"))]
            crate::measure::unreserved(&self.budget, self.tag, own);
        }
        self.release_ahead();
    }

    /// Hands `bytes` of its items' charge (one [`Share`]'s) to a holder that releases them
    /// itself (with [`Budget::release`]): they stay charged at every level, and are no longer
    /// the grant's; nothing is released.
    fn hand_off(&mut self, bytes: usize) {
        let bytes = bytes.min(self.own);
        self.own -= bytes;
        #[cfg(any(test, feature = "measure"))]
        {
            crate::measure::charged(&self.budget, bytes);
            crate::measure::unreserved(&self.budget, self.tag, bytes);
        }
    }

    /// Hands its items' charge to a holder that releases it itself (with
    /// [`Budget::release`]); the spare is released.
    pub fn transfer(mut self) -> usize {
        self.release_ahead();
        let bytes = std::mem::take(&mut self.own);
        #[cfg(any(test, feature = "measure"))]
        {
            crate::measure::charged(&self.budget, bytes);
            crate::measure::unreserved(&self.budget, self.tag, bytes);
        }
        bytes
    }
}

impl<B: Deref<Target = Budget> + Clone> Grant<B> {
    /// Its items' charge as a [`Reservation`] on the same budget (to [`Reservation::absorb`]
    /// into another); the spare is released.
    pub fn into_reservation(mut self) -> Reservation<B> {
        self.release_ahead();
        let bytes = std::mem::take(&mut self.own);
        Reservation {
            budget: self.budget.clone(),
            bytes,
            #[cfg(any(test, feature = "measure"))]
            tag: self.tag,
        }
    }
}

impl<B: Deref<Target = Budget>> Drop for Grant<B> {
    fn drop(&mut self) {
        self.trim();
    }
}

/// What holds a charged buffer's charge ([`Charged`], [`crate::ChargedMap`]): a
/// [`Reservation`] on a budget, or a [`Share`] of a [`Pool`]. Its methods are the buffers'
/// own: each takes a token only this crate makes, so nothing but a buffer's growth, its
/// drop and its hand-over ([`Reservation::transfer`], [`Share::transfer`]) changes what a
/// buffer is charged (the executor memory stack's second lead, 8 Nov: a path bag's charge
/// kept beside its vector by hand disagreed with it at a new site in each of six rounds).
pub trait Hold: hold::Charge {}

impl<B: Deref<Target = Budget>> Hold for Reservation<B> {}

impl<B: Deref<Target = Budget>> Hold for Share<'_, B> {}

/// [`Hold`]'s methods, sealed: each takes a [`hold::Token`], which only this crate can make.
pub mod hold {
    use super::BudgetExceeded;

    /// Made only in this crate: a [`Charge`] method can't be called from outside it.
    #[derive(Clone, Copy)]
    pub struct Token(());

    pub(crate) const TOKEN: Token = Token(());

    pub trait Charge {
        /// What holds the old bytes while a buffer moves: released when it drops.
        type Old<'a>
        where
            Self: 'a;

        /// The bytes held.
        fn held(&self, _: Token) -> usize;

        /// Holds `bytes` in place of what it held, as a charge made now: they are charged,
        /// and what it held is handed back, released when that drops. Until then both are
        /// held, as a reallocation holds the old block and the new. A refusal changes nothing.
        fn renew(&mut self, _: Token, bytes: usize) -> Result<Self::Old<'_>, BudgetExceeded>;

        /// [`Self::renew`] to at least `least` bytes and at most `most`, in whole `unit`s:
        /// `least` charged exactly (refused, and recorded as refused, where it doesn't fit),
        /// the rest never refused nor recorded: whole where every level has room for it
        /// ([`super::Budget::try_take`]), else no more than half of the room left at a level
        /// ([`super::Budget::take_up_to`]'s rule, which leaves siblings room). What it holds
        /// then, with the old bytes' holder. A refusal changes nothing.
        fn renew_up_to(
            &mut self,
            _: Token,
            least: usize,
            most: usize,
            unit: usize,
        ) -> Result<(Self::Old<'_>, usize), BudgetExceeded>;

        /// What it holds beyond `bytes` released (an upper bound trimmed to what was
        /// allocated). Never refused.
        fn trim(&mut self, _: Token, bytes: usize);

        /// The refusal of a size no buffer can hold.
        fn too_large(&self, _: Token) -> BudgetExceeded;
    }
}

/// Test-only: a pause on one thread between a growing vector's exact charge and its
/// optional one, where a sibling's growth can come between them (the budget types' owner's
/// race, 8 Nov).
#[cfg(test)]
pub(crate) mod growing {
    use std::cell::RefCell;

    type Pause = Box<dyn Fn()>;

    thread_local! {
        static PAUSE: RefCell<Option<Pause>> = const { RefCell::new(None) };
    }

    /// `pause` runs at this thread's next growth, once.
    pub(crate) fn pause_with(pause: impl Fn() + 'static) {
        PAUSE.with(|slot| *slot.borrow_mut() = Some(Box::new(pause)));
    }

    pub(crate) fn pause() {
        if let Some(pause) = PAUSE.with(|slot| slot.borrow_mut().take()) {
            pause();
        }
    }
}

use hold::{Charge, TOKEN};

impl<B: Deref<Target = Budget>> Charge for Reservation<B> {
    type Old<'a>
        = Reservation<&'a Budget>
    where
        Self: 'a;

    fn held(&self, _: hold::Token) -> usize {
        self.bytes
    }

    fn renew(&mut self, _: hold::Token, bytes: usize) -> Result<Self::Old<'_>, BudgetExceeded> {
        Reservation::renew(self, bytes)
    }

    fn renew_up_to(
        &mut self,
        _: hold::Token,
        least: usize,
        most: usize,
        unit: usize,
    ) -> Result<(Self::Old<'_>, usize), BudgetExceeded> {
        self.budget.take(least)?;
        #[cfg(test)]
        growing::pause();
        let wanted = most.saturating_sub(least);
        let extra = match wanted == 0 || self.budget.try_take(wanted) {
            true => wanted,
            false => {
                let extra = self.budget.take_up_to(wanted);
                // Whole units: the part of one returned at once.
                let part = extra % unit.max(1);
                if part > 0 {
                    self.budget.give(part);
                }
                extra - part
            }
        };
        let held = least + extra;
        let old = Reservation {
            budget: &*self.budget,
            bytes: std::mem::replace(&mut self.bytes, held),
            #[cfg(any(test, feature = "measure"))]
            tag: std::mem::replace(&mut self.tag, crate::measure::tag()),
        };
        #[cfg(any(test, feature = "measure"))]
        crate::measure::reserved(old.budget, self.tag, held);
        Ok((old, held))
    }

    fn trim(&mut self, _: hold::Token, bytes: usize) {
        if self.bytes > bytes {
            self.release(self.bytes - bytes);
        }
    }

    fn too_large(&self, _: hold::Token) -> BudgetExceeded {
        self.budget.refuse(usize::MAX, self.budget.used())
    }
}

/// One thread's [`Grant`], which the buffers of one operation draw from together (a path
/// evaluator's bags, unions and columns): each holds its part as a [`Share`], so a small
/// buffer's growth comes from the grant's spare, a local subtraction, with no call up the
/// budget's levels (the executor memory stack's timings: an atomic per item or per node
/// cost up to x1.94 in parallel workers). Not `Sync`: one thread's. Its buffers borrow it,
/// so every one is freed, and its share given back, before the pool's grant releases what
/// it holds.
#[derive(Debug)]
pub struct Pool<B: Deref<Target = Budget> = Arc<Budget>>(std::cell::RefCell<Grant<B>>);

impl<B: Deref<Target = Budget>> Pool<B> {
    /// A pool for an operation on the global pool's threads ([`Grant::new`]).
    pub fn new(budget: B) -> Self {
        Self(std::cell::RefCell::new(Grant::new(budget)))
    }

    /// Whether its grant draws from `budget`.
    pub fn is_on(&self, budget: &Budget) -> bool {
        std::ptr::eq(&*self.0.borrow().budget, budget)
    }
}

/// What one buffer holds of a [`Pool`]'s grant: charged at every level of the pool's
/// budget, given back to the grant when it drops. It has no method that changes its
/// bytes: only its buffer's growth does ([`Hold`]).
#[derive(Debug)]
pub struct Share<'p, B: Deref<Target = Budget> = Arc<Budget>> {
    pool: &'p Pool<B>,
    bytes: usize,
}

impl<'p, B: Deref<Target = Budget>> Share<'p, B> {
    /// Nothing of `pool` held yet, for a buffer to grow.
    pub(crate) fn empty(pool: &'p Pool<B>) -> Self {
        Self { pool, bytes: 0 }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Hands the bytes to a holder that releases them itself (with [`Budget::release`]),
    /// out of the pool's grant: nothing is released.
    pub fn transfer(mut self) -> usize {
        let bytes = std::mem::take(&mut self.bytes);
        self.pool.0.borrow_mut().hand_off(bytes);
        bytes
    }
}

impl<'p, B: Deref<Target = Budget>> Charge for Share<'p, B> {
    type Old<'a>
        = Share<'p, B>
    where
        Self: 'a;

    fn held(&self, _: hold::Token) -> usize {
        self.bytes
    }

    fn renew(&mut self, _: hold::Token, bytes: usize) -> Result<Self::Old<'_>, BudgetExceeded> {
        self.pool.0.borrow_mut().take(bytes)?;
        Ok(Share {
            pool: self.pool,
            bytes: std::mem::replace(&mut self.bytes, bytes),
        })
    }

    fn renew_up_to(
        &mut self,
        _: hold::Token,
        least: usize,
        most: usize,
        unit: usize,
    ) -> Result<(Self::Old<'_>, usize), BudgetExceeded> {
        let held = {
            let mut grant = self.pool.0.borrow_mut();
            grant.take(least)?;
            least + grant.take_up_to(most.saturating_sub(least), unit)
        };
        let old = Share {
            pool: self.pool,
            bytes: std::mem::replace(&mut self.bytes, held),
        };
        Ok((old, held))
    }

    fn trim(&mut self, _: hold::Token, bytes: usize) {
        if self.bytes > bytes {
            self.pool.0.borrow_mut().give_back(self.bytes - bytes);
            self.bytes = bytes;
        }
    }

    fn too_large(&self, _: hold::Token) -> BudgetExceeded {
        let grant = self.pool.0.borrow();
        grant.budget.refuse(usize::MAX, grant.budget.used())
    }
}

impl<B: Deref<Target = Budget>> Drop for Share<'_, B> {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.pool.0.borrow_mut().give_back(self.bytes);
        }
    }
}

/// A vector whose memory is charged before it is allocated, to a budget ([`ChargedVec`]) or
/// to a [`Pool`]. Every change of its capacity takes one path ([`Charged::set_capacity`]):
/// the new capacity is charged first, beside the old (a reallocation holds both while it
/// moves the items), then allocated, then the old capacity released; one there is no room
/// for fails without allocating. It never holds more than it was charged, however a call
/// ends: what it lends a writer ([`Charged::fill`]) can't grow it. Dropped, it frees the
/// items, then releases their charge; [`Charged::into_parts`] hands the items out with
/// their charge, whose holder hands the bytes on ([`Reservation::transfer`],
/// [`Share::transfer`]) to a table that counts them, never released and charged again.
#[derive(Debug)]
pub struct Charged<T, H: Hold = Reservation> {
    /// Before `held`: fields drop in order, the items first.
    items: Vec<T>,
    /// Exactly `items`' capacity in bytes.
    held: H,
}

/// A [`Charged`] vector on a budget.
pub type ChargedVec<T, B = Arc<Budget>> = Charged<T, Reservation<B>>;

impl<T, B: Deref<Target = Budget>> Charged<T, Reservation<B>> {
    pub fn new(budget: B) -> Self {
        Self::empty(Reservation::new(budget))
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
}

impl<'p, T, B: Deref<Target = Budget>> Charged<T, Share<'p, B>> {
    /// An empty vector drawing from `pool`.
    pub fn in_pool(pool: &'p Pool<B>) -> Self {
        Self::empty(Share::empty(pool))
    }

    /// Room for `capacity` items, drawn from `pool` first.
    pub fn with_capacity_in(pool: &'p Pool<B>, capacity: usize) -> Result<Self, BudgetExceeded> {
        let mut vec = Self::in_pool(pool);
        vec.reserve_exact(capacity)?;
        Ok(vec)
    }
}

impl<T, H: Hold> Charged<T, H> {
    fn empty(held: H) -> Self {
        debug_assert_eq!(held.held(TOKEN), 0);
        Self {
            items: Vec::new(),
            held,
        }
    }

    /// `items`, made outside any budget (a producer that doesn't charge, such as a
    /// [`crate::graph`] closure), taken by this empty vector and their capacity charged now,
    /// after they were allocated; refused, they are freed and nothing is charged.
    ///
    /// # Panics
    /// If this vector has a capacity.
    pub fn adopt_vec(mut self, items: Vec<T>) -> Result<Self, BudgetExceeded> {
        assert_eq!(self.items.capacity(), 0, "adopt_vec: into an empty vector");
        let bytes = Self::bytes(items.capacity()).expect("a vector's capacity has a layout");
        drop(self.held.renew(TOKEN, bytes)?);
        self.items = items;
        Ok(self)
    }

    /// The bytes of `capacity` items, if a vector can hold them.
    fn bytes(capacity: usize) -> Option<usize> {
        std::alloc::Layout::array::<T>(capacity)
            .ok()
            .map(|layout| layout.size())
    }

    /// Room for `additional` more items: what is asked charged exactly (refused where it
    /// doesn't fit), and up to double the capacity as far as there is room, without refusal
    /// ([`hold::Charge::renew_up_to`]), beside the current capacity (a reallocation holds
    /// both).
    ///
    /// Grown by only what is asked, a vector near its limit reallocated at every push,
    /// copying all its items each time, until it was refused: pushes there cost copies
    /// quadratic in the vector's length (the executor memory stack's second lead, 8 Nov). And
    /// a capacity chosen from a reading of the room, then charged exactly, was refused, and
    /// its budget recorded as refused, where a sibling's growth came between the reading and
    /// the charge although what was asked still fitted (the budget types' owner, 8 Nov). Grown
    /// by what is asked, then doubled where that fits whole, else by no more than half of the
    /// room beyond (not half of the room alone: a vector that could double stopped short and
    /// was refused at its next growth, r5's 65,536 ends under 900 KiB), near the limit it grows
    /// a logarithmic number of times before it is refused, and it is refused only where what is
    /// asked has no room.
    pub fn reserve(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        let needed = self.needed(additional)?;
        if needed <= self.items.capacity() {
            return Ok(());
        }
        let size = std::mem::size_of::<T>();
        // At least std's first capacity (`Vec`'s: 8 items of a byte, 4 up to 1 KiB, else 1),
        // so a small bag grows once, not three times (the executor memory stack's second
        // lead's counts: a path from 2.7 million bound values makes a bag at each).
        let first = match size {
            1 => 8,
            2..=1024 => 4,
            _ => 1,
        };
        let doubled = needed
            .max(self.items.capacity().saturating_mul(2))
            .max(first);
        if size == 0 {
            return self.set_capacity(doubled);
        }
        let least = Self::bytes(needed).ok_or_else(|| self.held.too_large(TOKEN))?;
        let most = Self::bytes(doubled).unwrap_or(least);
        // Released when it drops: after the reallocation, or as an unwinding one ends.
        let (_old, held) = self.held.renew_up_to(TOKEN, least, most, size)?;
        let capacity = held / size;
        self.items.reserve_exact(capacity - self.items.len());
        debug_assert_eq!(Self::bytes(self.items.capacity()), Some(held));
        Ok(())
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
        self.items
            .len()
            .checked_add(additional)
            .ok_or_else(|| self.held.too_large(TOKEN))
    }

    /// The one path that changes the capacity, to `capacity` items (at least the length):
    /// charged beside the old capacity, the vector reallocated, the old capacity released.
    /// A capacity no vector can hold is refused as one there is no room for.
    fn set_capacity(&mut self, capacity: usize) -> Result<(), BudgetExceeded> {
        let bytes = Self::bytes(capacity).ok_or_else(|| self.held.too_large(TOKEN))?;
        // Released when it drops: after the reallocation, or as an unwinding one ends.
        let _old = self.held.renew(TOKEN, bytes)?;
        if capacity > self.items.capacity() {
            self.items.reserve_exact(capacity - self.items.len());
        } else {
            self.items.shrink_to(capacity);
        }
        debug_assert_eq!(Self::bytes(self.items.capacity()), Some(bytes));
        Ok(())
    }

    pub fn push(&mut self, item: T) -> Result<(), BudgetExceeded> {
        if self.items.len() == self.items.capacity() {
            self.reserve(1)?;
        }
        self.items.push(item);
        Ok(())
    }

    /// `item` appended within the capacity already charged, with no budget call and no
    /// refusal to carry; `Err(item)` where there is no room, for the caller to grow first
    /// ([`Self::reserve`]). For a hot loop of one item an iteration: a [`Self::push`]'s
    /// `Result` there cost D16-039's group walk a 5 % rise in instructions on NOT EXISTS.
    #[inline]
    pub fn push_within(&mut self, item: T) -> Result<(), T> {
        if self.items.len() < self.items.capacity() {
            self.items.push(item);
            Ok(())
        } else {
            Err(item)
        }
    }

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
    pub fn extend_bounded(
        &mut self,
        bound: usize,
        items: impl IntoIterator<Item = T>,
    ) -> Result<(), BudgetExceeded> {
        self.fill(bound, |room| {
            items
                .into_iter()
                .take(bound)
                .for_each(|item| room.push(item));
        })
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
        Ok(fill(&mut Room(&mut self.items)))
    }

    pub fn truncate(&mut self, len: usize) {
        self.items.truncate(len);
    }

    /// The capacity cut to the length, through the one path that changes it: the smaller
    /// capacity charged beside the old while the vector moves, then the old released. A
    /// refusal leaves the vector as it was.
    pub fn shrink_to_fit(&mut self) -> Result<(), BudgetExceeded> {
        match self.items.len() < self.items.capacity() {
            true => self.set_capacity(self.items.len()),
            false => Ok(()),
        }
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
        self.held.held(TOKEN)
    }

    /// The items, and the charge of their capacity, for a holder that keeps it until they
    /// are freed or hands it on with them.
    pub fn into_parts(self) -> (Vec<T>, H) {
        let Self { items, held } = self;
        (items, held)
    }
}

impl<T, H: Hold> Deref for Charged<T, H> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<T, H: Hold> std::ops::DerefMut for Charged<T, H> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.items
    }
}

/// The room a [`Charged`] vector lends a writer ([`Charged::fill`]): items are added within
/// the capacity reserved, which it never grows.
#[derive(Debug)]
pub struct Room<'v, T>(&'v mut Vec<T>);

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

    /// `items` appended at once, as many as they say they are: one check of the room, not
    /// one an item ([`Self::push`]; a path's columns, made of millions of copies, paid one
    /// each). Panics where there is no room for them; items past the room, where an iterator
    /// yields more than it said, are cut there, never grow the vector.
    pub fn extend<I: IntoIterator<Item = T, IntoIter: ExactSizeIterator>>(&mut self, items: I) {
        let items = items.into_iter();
        let room = self.0.capacity() - self.0.len();
        assert!(items.len() <= room, "past the room reserved");
        // Taken at most `room`: an iterator longer than it says can't grow the vector.
        self.0.extend(items.take(room));
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
    /// meet first). A panic is handled as in [`Self::par_fill`].
    pub fn try_par_fill<E: Send>(
        &mut self,
        n: usize,
        item: impl Fn(usize) -> Result<T, E> + Sync,
    ) -> Result<(), E>
    where
        T: Send,
    {
        use rayon::prelude::*;
        let len = self.0.len();
        assert!(n <= self.0.capacity() - len, "past the room reserved");
        // As in `par_fill`: the only pointer to the slots until the length is published.
        let slots = Slots(self.0.as_mut_ptr().wrapping_add(len));
        let part = n.div_ceil(4 * rayon::current_num_threads()).max(1);
        let failed = AtomicBool::new(false);
        // One token per range, with the range's error: ranges stopped early aren't adjacent,
        // so the tokens are kept apart, at most a few per worker.
        let ranges: Vec<FillRange<T, E>> = (0..n.div_ceil(part))
            .into_par_iter()
            .map(|p| {
                let mut written = Written::new(slots, p * part);
                for i in p * part..(p * part + part).min(n) {
                    if failed.load(Ordering::Relaxed) {
                        break;
                    }
                    match item(i) {
                        Ok(value) => {
                            // SAFETY: (`Slots`) slot `i` is this range's, and not yet written.
                            unsafe { slots.0.add(i).write(value) };
                            written.len += 1;
                        }
                        Err(error) => {
                            failed.store(true, Ordering::Relaxed);
                            return (written, Some((i, error)));
                        }
                    }
                }
                (written, None)
            })
            .collect();
        let mut first: Option<(usize, E)> = None;
        let mut complete = Vec::with_capacity(ranges.len());
        for (written, error) in ranges {
            complete.push(written);
            if let Some((i, error)) = error
                && first.as_ref().is_none_or(|(at, _)| i < *at)
            {
                first = Some((i, error));
            }
        }
        if let Some((_, error)) = first {
            // Dropping the tokens drops what each range wrote, once.
            drop(complete);
            return Err(error);
        }
        // No error, so no range stopped: one range of every item, in order.
        let written = complete
            .into_iter()
            .fold(Written::new(slots, 0), Written::join);
        assert_eq!((written.start, written.len), (0, n), "every slot written");
        std::mem::forget(written);
        // SAFETY: the `n` slots after the items are written, within the capacity.
        unsafe { self.0.set_len(len + n) };
        Ok(())
    }

    /// Parts of `lengths[i]` items after the room's, in order, part `i` written by
    /// `fill(i, part)`, the parts in parallel, each by one task through a [`Part`] that can't
    /// go past its slots: for a writer whose items come in runs of known lengths (a
    /// closure's pairs, component by component), written in place, with no second copy.
    /// The first error (by part) is returned and nothing is published: the room holds what
    /// it held (items are `Copy`: what was written needs no drop). Panics where the room
    /// has fewer slots than the parts, or a part that returns `Ok` is short, before
    /// publishing. Its bookkeeping, a slot range and a result a part, is uncharged: the
    /// caller bounds the parts (the graph's closure: at most four a thread and one more).
    pub fn try_par_fill_parts<E: Send>(
        &mut self,
        lengths: &[usize],
        fill: impl Fn(usize, &mut Part<'_, T>) -> Result<(), E> + Sync,
    ) -> Result<(), E>
    where
        T: Copy + Send,
    {
        use rayon::prelude::*;
        let total = lengths
            .iter()
            .try_fold(0usize, |n, &len| n.checked_add(len))
            .expect("the parts' length is counted");
        let len = self.0.len();
        let spare = self.0.spare_capacity_mut();
        assert!(total <= spare.len(), "past the room reserved");
        // Disjoint slots, one run a part, tiling the first `total` spare slots in order.
        let mut slots = Vec::with_capacity(lengths.len());
        let mut rest = &mut spare[..total];
        for &part in lengths {
            let (head, tail) = rest.split_at_mut(part);
            slots.push(head);
            rest = tail;
        }
        let written: Vec<Result<usize, E>> = slots
            .into_par_iter()
            .enumerate()
            .map(|(i, slots)| {
                let mut part = Part { slots, len: 0 };
                fill(i, &mut part).map(|()| part.len)
            })
            .collect();
        for (written, &part) in written.into_iter().zip(lengths) {
            assert_eq!(written?, part, "a part left short");
        }
        // SAFETY: the parts' slots tile the `total` spare slots after the room's items, in
        // order (split above), and every part wrote all of its own, slot 0 up (a `Part`
        // writes only its next slot, and each returned exactly its length, asserted above):
        // the `total` slots after the items are initialised, within the capacity.
        unsafe { self.0.set_len(len + total) };
        Ok(())
    }
}

/// The slots of one part of [`Room::try_par_fill_parts`], written in order: it can't write
/// past them.
#[derive(Debug)]
pub struct Part<'s, T> {
    slots: &'s mut [std::mem::MaybeUninit<T>],
    len: usize,
}

impl<T> Part<'_, T> {
    /// Panics where the part is full.
    #[inline]
    pub fn push(&mut self, item: T) {
        assert!(self.len < self.slots.len(), "past the part");
        self.slots[self.len].write(item);
        self.len += 1;
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
    use super::*;

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
                return Err(i);
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
        assert_eq!(fill(1, &[60, 80]), (Err(60), 1));
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
        assert!(matches!(filled, Err(30 | 70)), "{filled:?}");
        assert_eq!(len, 1);
        assert_eq!(
            DROPPED.load(Ordering::SeqCst) - 61,
            MADE.load(Ordering::SeqCst) - made + 1
        );
        assert_eq!(budget.used(), 0, "the items' memory released");
        assert_eq!(fill(4, &[]), (Ok(()), 101));
    }

    /// Parts are written in order, each by its writer, in place; a part that errs publishes
    /// nothing and the room holds what it held; parts past the room, a writer past its part
    /// and a short part panic before anything is published.
    #[test]
    fn parts_are_written_in_order_and_an_error_publishes_nothing() {
        let budget = Budget::unlimited();
        let mut vec: ChargedVec<u64, &Budget> = ChargedVec::with_capacity(&budget, 10).unwrap();
        vec.push(99).unwrap();
        let lengths = [3, 0, 4];
        vec.fill(0, |room| {
            room.try_par_fill_parts(&lengths, |i, part| {
                for k in 0..lengths[i] {
                    part.push((10 * i + k) as u64);
                }
                Ok::<_, ()>(())
            })
        })
        .unwrap()
        .unwrap();
        assert_eq!(&vec[..], &[99, 0, 1, 2, 20, 21, 22, 23]);
        let failed = vec
            .fill(0, |room| {
                room.try_par_fill_parts(&[1, 1], |i, part| {
                    part.push(7);
                    if i == 1 { Err(i) } else { Ok(()) }
                })
            })
            .unwrap();
        assert_eq!(failed, Err(1));
        assert_eq!(vec.len(), 8, "nothing published");
        let mut panics = |lengths: &'static [usize], pushes: usize| {
            let mut vec = std::panic::AssertUnwindSafe(&mut vec);
            std::panic::catch_unwind(move || {
                vec.fill(0, |room| {
                    room.try_par_fill_parts(lengths, |_, part| {
                        (0..pushes).for_each(|_| part.push(1));
                        Ok::<_, ()>(())
                    })
                })
            })
            .is_err()
        };
        assert!(panics(&[5], 5), "parts past the room");
        assert!(panics(&[1], 2), "a writer past its part");
        assert!(panics(&[2], 1), "a short part");
        assert_eq!(&vec[..], &[99, 0, 1, 2, 20, 21, 22, 23]);
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
                            Err(i)
                        }
                        _ => made(),
                    })
                })
            })
            .unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 2, "both failures observed");
        assert_eq!(filled, Err(2), "the lower of the two");
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
                            Err(i)
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

    /// A writer's run of items ([`Room::extend`]) can't grow its vector either: one that says
    /// it has more items than the room left panics before anything is written, and one that
    /// yields more than it says is cut at the room. Within the room, the items come in order.
    #[test]
    fn a_run_of_items_stays_within_the_room() {
        /// Says it has one item, yields `n`.
        struct Lying(usize);
        impl Iterator for Lying {
            type Item = u64;
            fn next(&mut self) -> Option<u64> {
                self.0.checked_sub(1).map(|left| {
                    self.0 = left;
                    left as u64
                })
            }
        }
        impl ExactSizeIterator for Lying {
            fn len(&self) -> usize {
                1
            }
        }
        let budget = Budget::new(1 << 10);
        let mut vec: ChargedVec<u64, &Budget> = ChargedVec::with_capacity(&budget, 4).unwrap();
        let past = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vec.fill(0, |room| room.extend([1, 2, 3, 4, 5]))
        }));
        assert!(past.is_err());
        assert!(vec.is_empty());
        vec.fill(0, |room| room.extend(Lying(10))).unwrap();
        assert_eq!((vec.len(), vec.capacity(), budget.used()), (4, 4, 32));
        vec.truncate(0);
        vec.fill(0, |room| room.extend(std::iter::repeat_n(7, 3)))
            .unwrap();
        assert_eq!(&vec[..], &[7, 7, 7]);
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

    /// Pushes near a budget's limit cost copies linear in what they push, and the vector
    /// never holds more than the limit: where doubling has no room it grows by what is asked
    /// and half of the room beyond, so it is refused after a few growths, not reallocated
    /// (every item copied) at each push. The limit is 90,000 items' bytes, so doubling from
    /// 32,768 items (32,768 beside 65,536) has no room. Grown by what each push asks, that
    /// vector reallocated 12,248 times from 32,768 items on: 475,655,739 items copied for
    /// 45,000 pushes. Doubling's copies are at most two a push; the last growths, by half of
    /// the room left, add at most one more.
    #[test]
    fn pushes_near_the_limit_copy_each_item_a_bounded_number_of_times() {
        const LIMIT: usize = 90_000 * 8;
        let budget = Budget::new(LIMIT);
        let mut vec: ChargedVec<u64, &Budget> = ChargedVec::new(&budget);
        let (mut pushed, mut copied, mut growths) = (0usize, 0usize, 0usize);
        loop {
            let (capacity, len) = (vec.capacity(), vec.len());
            if vec.push(pushed as u64).is_err() {
                break;
            }
            if vec.capacity() != capacity {
                growths += 1;
                copied += len;
            }
            pushed += 1;
            assert!(budget.used() <= LIMIT && vec.capacity() * 8 <= LIMIT);
        }
        assert!(
            copied <= 3 * pushed,
            "{copied} items copied for {pushed} pushes in {growths} growths"
        );
        assert!(budget.peak() <= LIMIT, "peak {}", budget.peak());
        assert_eq!(budget.used(), vec.capacity() * 8);
        assert!(vec.iter().copied().eq(0..pushed as u64));
    }

    /// Spec (the budget types' owner, 8 Nov): two vectors on one budget, growing at once near
    /// its limit, both grow where what each asks fits, and the budget isn't recorded as
    /// refused. The limit leaves room beside their 1,000 items each for both to ask 1,001 more,
    /// and for either alone to double, but not for both to double. The first to grow pauses
    /// between its exact charge and its optional one (`growing`), and the other grows
    /// meanwhile. Chosen from a reading of the room and then charged exactly, the first's
    /// doubled capacity was refused, and the budget recorded as refused, though what it asked
    /// fitted.
    #[test]
    fn vectors_growing_at_once_near_the_limit_both_grow() {
        use std::sync::{Arc, Barrier};
        const ITEMS: usize = 1_000;
        let budget = Budget::new((2 * ITEMS + 2_500) * 8);
        let mut vectors = [(); 2].map(|()| {
            let mut vec: ChargedVec<u64, &Budget> =
                ChargedVec::with_capacity(&budget, ITEMS).unwrap();
            vec.resize(ITEMS, 0).unwrap();
            vec
        });
        let [first, second] = &mut vectors;
        let (paused, resumed) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
        let (grew_first, grew_second) = std::thread::scope(|scope| {
            let (pause, resume) = (Arc::clone(&paused), Arc::clone(&resumed));
            let pausing = scope.spawn(move || {
                growing::pause_with(move || {
                    pause.wait();
                    resume.wait();
                });
                first.push(1)
            });
            paused.wait();
            let grew_second = second.push(1);
            resumed.wait();
            (pausing.join().unwrap(), grew_second)
        });
        assert!(
            grew_first.is_ok() && grew_second.is_ok(),
            "{grew_first:?}, {grew_second:?}"
        );
        assert!(!budget.refused(), "the budget recorded as refused");
        assert_eq!(budget.used(), vectors[0].charged() + vectors[1].charged());
    }

    /// A vector whose doubling fits doubles, however near the limit: 32,768 items under a
    /// limit of three times their bytes (32,768 beside 65,536 while it moves) grow to 65,536
    /// and take them all. Grown by half of the room beyond what is asked alone, it stopped at
    /// 49,152 and its next growth was refused (r5's path column of 65,536 ends under 900 KiB,
    /// ems-r4-c6-bf3699b9).
    #[test]
    fn a_vector_whose_doubling_fits_doubles() {
        const ITEMS: usize = 32_768;
        let budget = Budget::new(3 * ITEMS * 8);
        let mut vec: ChargedVec<u64, &Budget> = ChargedVec::with_capacity(&budget, ITEMS).unwrap();
        vec.resize(ITEMS, 0).unwrap();
        vec.push(1).unwrap();
        assert_eq!(vec.capacity(), 2 * ITEMS);
        vec.resize(2 * ITEMS, 0).unwrap();
        assert!(!budget.refused());
        assert_eq!(budget.used(), 2 * ITEMS * 8);
    }

    /// A pool's vectors draw on its grant: each is charged exactly its capacity while it
    /// lives, and given back to the grant once dropped (freed first); a vector's charge handed
    /// over with its items ([`Share::transfer`]) stays charged at the budget and is the
    /// grant's no more, so the pool's end releases only the rest. Nothing is held beyond its
    /// charge at any moment.
    #[test]
    fn a_pools_vectors_hold_their_charge_and_hand_it_over_whole() {
        if !crate::measure::alone() {
            return;
        }
        let envelope = Envelope::new(Some(1 << 20), None);
        let part = envelope.part();
        let ((), excess) = crate::measure::measure_within(&part, || {
            let pool = Pool::new(&part);
            let mut kept: Charged<u64, Share<'_, &Budget>> = Charged::in_pool(&pool);
            let mut dropped: Charged<u64, Share<'_, &Budget>> = Charged::in_pool(&pool);
            for i in 0..1_000 {
                kept.push(i).unwrap();
                dropped.push(i).unwrap();
            }
            assert_eq!(kept.charged(), kept.capacity() * 8);
            assert_eq!(dropped.charged(), dropped.capacity() * 8);
            assert!(part.used() >= kept.charged() + dropped.charged());
            drop(dropped);
            let (items, share) = kept.into_parts();
            let bytes = share.transfer();
            assert_eq!(bytes, items.capacity() * 8);
            drop(pool);
            assert_eq!(
                part.used(),
                bytes,
                "the handed-over bytes alone stay charged"
            );
            drop(items);
            part.release(bytes);
        });
        assert_eq!((excess, part.used()), (0, 0));
    }

    /// A vector made outside a budget, adopted, is charged its capacity, not its length; one
    /// there is no room for is refused, freed, and charges nothing.
    #[test]
    fn an_adopted_vector_is_charged_its_capacity_or_refused() {
        let budget = Budget::new(1 << 10);
        let mut items: Vec<u64> = Vec::with_capacity(100);
        items.extend(0..10);
        let adopted = ChargedVec::new(&budget).adopt_vec(items).unwrap();
        assert_eq!(
            (adopted.len(), adopted.charged(), budget.used()),
            (10, 800, 800)
        );
        let refused = ChargedVec::new(&budget).adopt_vec(Vec::<u64>::with_capacity(100));
        assert!(refused.is_err());
        assert_eq!(budget.used(), 800, "nothing charged for the refused one");
        drop(adopted);
        assert_eq!(budget.used(), 0);
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

    /// Guard (the executor memory stack's timing, Harness notes/2026-11-02-exec-memory-stack.md
    /// EMS-13: a shared atomic per item cost GROUP_CONCAT x1.94 in parallel workers): an item
    /// within a grant's spare makes no budget call at all, so 10^4 items of 6 bytes after the
    /// first make none; every level is exact again once the grant ends.
    #[test]
    fn an_item_within_a_grants_spare_makes_no_budget_call() {
        let server = SharedBudget::new(1 << 30);
        let request = Envelope::new(None, Some(Arc::clone(&server)));
        let part = request.part();
        let mut grant = Grant::with_workers(&part, 1);
        grant.take(6).unwrap();
        assert_eq!(part.ahead(), 64 << 10, "a refill of one cap");
        let before = calls::made();
        for _ in 0..10_000 {
            grant.take(6).unwrap();
        }
        assert_eq!(calls::made(), before, "no budget call within the spare");
        assert_eq!(grant.bytes(), 6 * 10_001);
        drop(grant);
        assert_eq!((part.used(), request.used(), server.used()), (0, 0, 0));
        assert_eq!(part.ahead(), 0);
    }

    /// Spec (the budget types' owner's ruling, 3 Nov): a grant's refill is never refused and
    /// never recorded as a refusal; an item that fits is never refused for the spare; where
    /// an item's rest doesn't fit, nothing is charged and the refusal is the real one; a
    /// give-back keeps spare up to the cap; no spare below 64 KiB of envelope, nor directly
    /// below the server.
    #[test]
    fn a_grants_refill_never_refuses_and_a_refusal_charges_nothing() {
        let request = Envelope::new(Some(100_000), None);
        let part = request.part();
        let mut grant = Grant::with_workers(&part, 1);
        let cap = part.grant_cap();
        assert!(cap > 0 && cap <= 100_000 / 64, "{cap}");
        grant.take(50_000).unwrap();
        assert_eq!((part.used(), request.used()), (50_000 + cap, 50_000 + cap));
        // The rest of the envelope, exactly: from the spare and the rest.
        grant.take(50_000).unwrap();
        assert_eq!((grant.bytes(), request.used()), (100_000, 100_000));
        assert!(
            !part.refused() && !request.refused(),
            "a refill never refuses"
        );
        let err = grant.take(1).unwrap_err();
        assert_eq!((err.limit, err.used, err.shared), (100_000, 100_000, false));
        assert_eq!(
            (part.used(), request.used(), grant.bytes()),
            (100_000, 100_000, 100_000)
        );
        assert!(part.refused() && request.refused());
        // Given back: kept as spare up to the cap, the rest released.
        grant.give_back(10_000);
        assert_eq!(request.used(), 90_000 + cap);
        drop(grant);
        assert_eq!((part.used(), request.used()), (0, 0));

        let small = Envelope::new(Some(60_000), None);
        let part = small.part();
        let mut grant = Grant::with_workers(&part, 1);
        grant.take(10).unwrap();
        assert_eq!(small.used(), 10, "no spare below 64 KiB");
        let server = SharedBudget::new(1 << 30);
        let below = Budget::unlimited().within(Some(Arc::clone(&server)));
        let mut grant = Grant::with_workers(&below, 1);
        grant.take(10).unwrap();
        assert_eq!(server.used(), 10, "none directly below the server");
    }

    /// Spec (the budget types' owner's ruling): grants of parallel workers over one part,
    /// taking and giving back, are never refused while what they hold fits, record no
    /// refusal, and leave every level as it was.
    #[test]
    fn parallel_grants_over_one_part_never_refuse_what_fits() {
        let request = Envelope::new(Some(4 << 20), None);
        let part = request.part();
        std::thread::scope(|scope| {
            for t in 0..8 {
                let part = &part;
                scope.spawn(move || {
                    let mut grant = Grant::new(part);
                    for i in 0..5_000 {
                        let bytes = 8 + (t * 31 + i) % 97;
                        grant.take(bytes).unwrap();
                        if i % 3 == 0 {
                            grant.give_back(bytes);
                        }
                    }
                });
            }
        });
        assert!(!request.refused() && !part.refused());
        assert_eq!((part.used(), request.used(), part.ahead()), (0, 0, 0));
    }

    /// Spec (the budget types' owner, 3 Nov: the spare per part is one cap): 16 workers'
    /// grants on one part below a 1 MiB envelope, each sized for 16 workers, never hold more
    /// spare together than one cap, and are never refused while what they hold fits.
    #[test]
    fn sixteen_workers_grants_on_one_part_hold_one_cap_ahead() {
        let request = Envelope::new(Some(1 << 20), None);
        let part = request.part();
        let cap = part.grant_cap();
        assert_eq!(cap, (1 << 20) / 64);
        std::thread::scope(|scope| {
            for t in 0..16 {
                let part = &part;
                scope.spawn(move || {
                    let mut grant = Grant::with_workers(part, 16);
                    for i in 0..3_000 {
                        let bytes = 8 + (t * 37 + i) % 89;
                        grant.take(bytes).unwrap();
                        assert!(part.ahead() <= cap, "{} spare, cap {cap}", part.ahead());
                        if i % 2 == 0 {
                            grant.give_back(bytes);
                        }
                    }
                });
            }
        });
        assert!(!request.refused() && !part.refused());
        assert_eq!((part.used(), request.used(), part.ahead()), (0, 0, 0));
    }

    /// Spec (the budget types' owner): a grant hands exactly its items' charge over as a
    /// reservation on the same budget, its spare released: the envelope then holds exactly
    /// that, and nothing once the reservation ends.
    #[test]
    fn a_grant_hands_over_exactly_its_items() {
        let request = Envelope::new(None, None);
        let part = request.part();
        let mut grant = Grant::with_workers(&part, 1);
        for _ in 0..3 {
            grant.take(100).unwrap();
        }
        assert!(request.used() > 300, "spare held");
        let held = grant.into_reservation();
        assert_eq!(held.bytes(), 300);
        assert_eq!((part.used(), request.used(), part.ahead()), (300, 300, 0));
        let mut other = Reservation::new(&part);
        other.charge(20).unwrap();
        other.absorb(held);
        assert_eq!(other.bytes(), 320);
        drop(other);
        assert_eq!((part.used(), request.used()), (0, 0));
    }

    /// Spec (sol-loop r5 G1): a reservation that absorbs another's bytes, while what they
    /// cover is live, leaves a measurement no excess: the bytes stay covered throughout
    /// (ORDER BY's computed keys, a grant's items absorbed into the keys' reservation).
    #[test]
    fn absorbing_live_items_leaves_a_measurement_no_excess() {
        if !crate::measure::alone() {
            return;
        }
        let request = Envelope::new(Some(64 << 20), None);
        let part = request.part();
        let ((), excess) = crate::measure::measure_within(&request, || {
            let mut held = Reservation::new(&part);
            held.charge(8).unwrap();
            let mut grant = Grant::new(&part);
            grant.take(1_000 * 4_096).unwrap();
            let items: Vec<u8> = Vec::with_capacity(1_000 * 4_096);
            held.absorb(grant.into_reservation());
            assert_eq!(held.bytes(), 8 + 1_000 * 4_096);
            drop(items);
            drop(held);
        });
        assert_eq!(excess, 0);
        assert_eq!(request.used(), 0);
    }

    /// Guard (sol-loop r4 N1, N2, ported): no grant draws another's spare. Two grants on one
    /// part, each for two workers, hold half a cap each; one's refill moves nothing of the
    /// other's, whose next item still makes no budget call.
    #[test]
    fn one_grants_refill_never_moves_anothers_spare() {
        let request = Envelope::new(None, None);
        let part = request.part();
        let half = part.grant_cap() / 2;
        let mut first = Grant::with_workers(&part, 2);
        first.take(10).unwrap();
        let mut second = Grant::with_workers(&part, 2);
        second.take(10).unwrap();
        assert_eq!((request.used(), part.ahead()), (20 + 2 * half, 2 * half));
        let before = calls::made();
        first.take(10).unwrap();
        assert_eq!(calls::made(), before, "the first drew on its own spare");
        drop((first, second));
        assert_eq!((request.used(), part.ahead()), (0, 0));
    }

    /// Spec (the budget types' owner, 3 Nov: S2 is the declared slack): below a 4 KiB envelope
    /// no grant holds spare, so two grants charging every item exactly fill the envelope to
    /// the byte without a false refusal.
    #[test]
    fn grants_below_a_small_envelope_hold_no_spare() {
        let request = Envelope::new(Some(4_096), None);
        let part = request.part();
        let (mut first, mut second) = (Grant::new(&part), Grant::new(&part));
        for _ in 0..1_024 {
            first.take(2).unwrap();
            second.take(2).unwrap();
        }
        assert_eq!((request.used(), part.ahead()), (4_096, 0));
        assert!(!request.refused());
        assert!(first.take(1).is_err());
    }

    /// Spec (the budget types' owner's ruling, `measure`): a grant's items are a
    /// reservation's to a measurement, and its refills above are no one's: memory allocated
    /// for items it charged, inside a measurement, holds no more than they reserve.
    #[test]
    fn a_measured_operation_holds_what_its_grant_reserves() {
        if !crate::measure::alone() {
            return;
        }
        let request = Envelope::new(Some(64 << 20), None);
        let part = request.part();
        let ((), excess) = crate::measure::measure_within(&request, || {
            let mut grant = Grant::with_workers(&part, 1);
            grant.take(100 * std::mem::size_of::<Vec<u64>>()).unwrap();
            let mut items: Vec<Vec<u64>> = Vec::with_capacity(100);
            for _ in 0..100 {
                grant.take(100 * 8).unwrap();
                items.push(Vec::with_capacity(100));
            }
            assert!(part.ahead() > 0, "room ahead held");
            drop(items);
            drop(grant);
        });
        assert_eq!(excess, 0);
        assert_eq!(request.used(), 0);
    }
}
