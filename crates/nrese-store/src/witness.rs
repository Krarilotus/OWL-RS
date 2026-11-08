//! Test-only counters of the work producers do (REAS-2 fix check r1, attack list B10). A
//! stopped poller doesn't ask its stop again, so the work a read does after a stop can't be
//! seen from the stop; each counter counts it at the producer that does it, on the thread
//! that does it. The module and every use of it are `#[cfg(test)]`: a use outside a test
//! doesn't compile in the library, and release builds carry none.

use std::cell::Cell;

/// What a counter counts.
#[derive(Clone, Copy)]
pub(crate) enum Site {
    /// U1's lookups of a fact in its view (`UpperBase::in_view`).
    InView,
}

thread_local! {
    static COUNTS: [Cell<u64>; 1] = const { [Cell::new(0)] };
}

/// Counts `n` units at `site` on this thread.
pub(crate) fn add(site: Site, n: u64) {
    COUNTS.with(|counts| {
        let count = &counts[site as usize];
        count.set(count.get() + n);
    });
}

/// The units counted at `site` on this thread so far.
pub(crate) fn get(site: Site) -> u64 {
    COUNTS.with(|counts| counts[site as usize].get())
}

/// A stop that fires at its first ask after `site` has counted work on the asking thread
/// (the stop firing during the first real probe), remembering how much it had counted.
pub(crate) struct FirstWork {
    site: Site,
    before: u64,
    fired_at: std::sync::atomic::AtomicU64,
}

impl FirstWork {
    pub(crate) fn new(site: Site) -> Self {
        Self {
            site,
            before: get(site),
            fired_at: std::sync::atomic::AtomicU64::new(u64::MAX),
        }
    }

    /// The stop's answer; on another thread it sees that thread's count.
    pub(crate) fn ask(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let seen = get(self.site).saturating_sub(self.before);
        if seen > 0 {
            let _ = self
                .fired_at
                .compare_exchange(u64::MAX, seen, Relaxed, Relaxed);
        }
        self.fired_at.load(Relaxed) != u64::MAX
    }

    /// The units counted at `site` on this thread after the stop fired.
    pub(crate) fn after(&self) -> u64 {
        let fired = self.fired_at.load(std::sync::atomic::Ordering::Relaxed);
        assert_ne!(fired, u64::MAX, "the stop fired");
        get(self.site) - self.before - fired
    }
}
