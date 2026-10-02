//! Memory accounting for queries.
//!
//! Operators charge the bytes of the tables they produce and release them when a table is
//! consumed. When a charge would exceed the limit, the query fails with [`BudgetExceeded`]
//! instead of growing the process: the scorecard showed server memory growing from 16 to
//! 24 GiB under 8 concurrent clients without any bound.
//!
//! Two limits apply. A [`Budget`] is one query's. A [`SharedBudget`] is the server's, for
//! all queries that run at the same time: each query's budget charges it too, and returns
//! what it holds when the query ends, however it ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    shared: Option<Arc<SharedBudget>>,
}

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
            shared: None,
        }
    }

    pub fn unlimited() -> Self {
        Self::new(usize::MAX)
    }

    /// Also charges `shared`, the server's budget for all queries.
    #[must_use]
    pub fn within(mut self, shared: Option<Arc<SharedBudget>>) -> Self {
        self.shared = shared;
        self
    }

    /// Reserves `bytes`, or fails without reserving anything.
    pub fn charge(&self, bytes: usize) -> Result<(), BudgetExceeded> {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let next = used.saturating_add(bytes);
            if next > self.limit {
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
        if let Some(shared) = &self.shared
            && let Err(in_use) = shared.charge(bytes)
        {
            self.used.fetch_sub(bytes, Ordering::Relaxed);
            return Err(BudgetExceeded {
                limit: shared.limit,
                requested: bytes,
                used: in_use,
                shared: true,
            });
        }
        Ok(())
    }

    pub fn release(&self, bytes: usize) {
        let bytes = bytes.min(self.used());
        self.used.fetch_sub(bytes, Ordering::Relaxed);
        if let Some(shared) = &self.shared {
            shared.release(bytes);
        }
    }

    /// Bytes that may still be charged: the query's own rest, and no more than the
    /// server has left.
    pub fn remaining(&self) -> usize {
        let own = self.limit.saturating_sub(self.used());
        match &self.shared {
            Some(shared) => own.min(shared.limit.saturating_sub(shared.used())),
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

    /// Whether the rest is the server's and not the query's own (for the error message).
    pub fn bounded_by_shared(&self) -> bool {
        self.shared.as_ref().is_some_and(|shared| {
            shared.limit.saturating_sub(shared.used()) < self.limit.saturating_sub(self.used())
        })
    }
}

impl Drop for Budget {
    /// A query that ends, with a result or an error, holds nothing of the server's budget.
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.release(self.used());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
