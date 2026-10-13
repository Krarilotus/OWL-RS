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

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The memory all running queries may hold together.
#[derive(Debug)]
pub struct SharedBudget {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
}

impl SharedBudget {
    pub fn new(limit_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: limit_bytes,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
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

    fn charge(&self, bytes: usize) -> Result<(), usize> {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let next = used.saturating_add(bytes);
            if next > self.limit {
                return Err(used);
            }
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

    fn release(&self, bytes: usize) {
        let _ = self
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                Some(used.saturating_sub(bytes))
            });
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
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let next = used.saturating_add(bytes);
            if next > self.limit {
                self.refused.store(true, Ordering::Relaxed);
                return Err(BudgetExceeded {
                    limit: self.limit,
                    requested: bytes,
                    used,
                    shared: false,
                });
            }
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
            Some(Parent::Server(shared)) => shared.charge(bytes).map_err(|in_use| BudgetExceeded {
                limit: shared.limit,
                requested: bytes,
                used: in_use,
                shared: true,
            }),
            Some(Parent::Envelope(envelope)) => envelope.charge(bytes),
            None => Ok(()),
        };
        if above.is_err() {
            self.used.fetch_sub(bytes, Ordering::Relaxed);
            self.refused.store(true, Ordering::Relaxed);
        }
        above
    }

    /// Whether a charge ever failed, here or above: what was computed under this budget
    /// may be limited by it (a request's own limit is no fact about the data, so a result
    /// it limited isn't published beyond the request).
    pub fn refused(&self) -> bool {
        self.refused.load(Ordering::Relaxed)
    }

    pub fn release(&self, bytes: usize) {
        let bytes = bytes.min(self.used());
        self.used.fetch_sub(bytes, Ordering::Relaxed);
        self.release_above(bytes);
    }

    fn release_above(&self, bytes: usize) {
        match &self.parent {
            Some(Parent::Server(shared)) => shared.release(bytes),
            Some(Parent::Envelope(envelope)) => envelope.release(bytes),
            None => {}
        }
    }

    /// Bytes that may still be charged: the own rest, and no more than the levels above
    /// have left.
    pub fn remaining(&self) -> usize {
        let own = self.limit.saturating_sub(self.used());
        match &self.parent {
            Some(Parent::Server(shared)) => own.min(shared.limit.saturating_sub(shared.used())),
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
            Some(Parent::Server(shared)) => shared.limit.saturating_sub(shared.used()) < own,
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
        self.release_above(self.used());
    }
}

/// Bytes charged to a budget that outlive the operation which charged them: released when
/// the reservation drops, however its holder ends. `B` borrows or shares the budget.
#[derive(Debug)]
pub struct Reservation<B: Deref<Target = Budget> = Arc<Budget>> {
    budget: B,
    bytes: usize,
}

impl<B: Deref<Target = Budget>> Reservation<B> {
    pub fn new(budget: B) -> Self {
        Self { budget, bytes: 0 }
    }

    /// Takes over `bytes` already charged to `budget`.
    pub fn adopt(budget: B, bytes: usize) -> Self {
        Self { budget, bytes }
    }

    /// Reserves `bytes` more, or fails without reserving anything.
    pub fn charge(&mut self, bytes: usize) -> Result<(), BudgetExceeded> {
        self.budget.charge(bytes)?;
        self.bytes += bytes;
        Ok(())
    }

    /// Returns up to `bytes` of this reservation to the budget.
    pub fn release(&mut self, bytes: usize) {
        let bytes = bytes.min(self.bytes);
        self.budget.release(bytes);
        self.bytes -= bytes;
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

    /// Hands the bytes to a holder that releases them itself; nothing is released.
    pub fn transfer(mut self) -> usize {
        std::mem::take(&mut self.bytes)
    }
}

impl<B: Deref<Target = Budget>> Drop for Reservation<B> {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
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
