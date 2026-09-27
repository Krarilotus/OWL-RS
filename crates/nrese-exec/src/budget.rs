//! Per-query memory accounting.
//!
//! Operators charge the bytes of the tables they produce and release them when a table is
//! consumed. When a charge would exceed the limit, the query fails with [`BudgetExceeded`]
//! instead of growing the process: the scorecard showed server memory growing from 16 to
//! 24 GiB under 8 concurrent clients without any bound.

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
pub struct Budget {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub limit: usize,
    pub requested: usize,
    pub used: usize,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "query memory budget exceeded: {} MiB in use, {} MiB more requested, limit {} MiB",
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
        }
    }

    pub fn unlimited() -> Self {
        Self::new(usize::MAX)
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
                });
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

    pub fn release(&self, bytes: usize) {
        self.used
            .fetch_sub(bytes.min(self.used()), Ordering::Relaxed);
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// The highest amount in use at any time.
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
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
        assert_eq!((err.used, err.requested), (60, 50));
        budget.release(60);
        budget.charge(100).unwrap();
        assert_eq!(budget.peak(), 100);
    }
}
