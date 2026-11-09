//! Test-only counters of the work producers do (REAS-2 fix check r1, attack list B10). A
//! stopped poller doesn't ask its stop again, so the work a read does after a stop can't be
//! seen from the stop; each counter counts it at the producer that does it, on the thread
//! that does it. The module and every use of it are `#[cfg(test)]`: a use outside a test
//! doesn't compile in the library, and release builds carry none.

use std::cell::Cell;

/// What a counter counts.
#[derive(Clone, Copy)]
pub(crate) enum Site {
    /// Maintenance's lookups of a fact in the state before its transaction
    /// (`EngineBase::known_before`).
    KnownBefore,
    /// Candidates a morsel's keep looks at (`Store::retain_new`).
    RetainStep,
    /// Job constructions started (`Job::new`).
    JobConstruction,
    /// `owl:sameAs` partner reads started for a dependency key (`same_as_partners`).
    PartnerRead,
    /// Morsel tasks started (`run_morsels`).
    MorselTask,
    /// Facts an expanded listing's fold takes (`batch::list`).
    ExpandedFact,
    /// Candidates `rederived`'s one-step pass takes.
    OneStepCandidate,
    /// Head groups the support round's per-head pass takes.
    HeadGroup,
    /// Merges of a fact's derivations into its sets started (`merge`).
    Merge,
}

thread_local! {
    static COUNTS: [Cell<u64>; 9] = const { [const { Cell::new(0) }; 9] };
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

/// A stop that never fires and records, at each ask, the work an engine producer did on the
/// asking thread since the ask before (REAS-3: the work a read does in the engine, below the
/// adapter that counts what it is handed). The largest such gap is what a stop arriving just
/// after an ask can't see; the invariant bounds it by `POLL_UNITS` plus one run. The work after
/// the last ask counts too ([`Self::largest`]): a stop could fire there as well.
pub(crate) struct EngineGaps {
    site: nrese_engine::witness::Site,
    last: std::sync::atomic::AtomicU64,
    largest: std::sync::atomic::AtomicU64,
}

impl EngineGaps {
    pub(crate) fn new(site: nrese_engine::witness::Site) -> Self {
        Self {
            site,
            last: std::sync::atomic::AtomicU64::new(nrese_engine::witness::get(site)),
            largest: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The stop's answer, always "go on"; asked on the thread that does the work.
    pub(crate) fn ask(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let now = nrese_engine::witness::get(self.site);
        let gap = now - self.last.swap(now, Relaxed);
        self.largest.fetch_max(gap, Relaxed);
        false
    }

    /// The largest gap so far, the one since the last ask included.
    pub(crate) fn largest(&self) -> u64 {
        self.ask();
        self.largest.load(std::sync::atomic::Ordering::Relaxed)
    }
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
