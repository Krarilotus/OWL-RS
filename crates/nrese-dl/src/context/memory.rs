//! The saturation's capacity envelope ([`super::Budget::task_memory`]): what it observes
//! and how (docs/design/owl2-dl.md §13).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

/// One engine's observations: the capacity the engine itself holds (program, arena,
/// registry, cores, inputs and results) and the latest sum of its contexts' publications,
/// reserved from the request's envelope where there is one.
#[derive(Debug)]
pub(super) struct Observer {
    limit: usize,
    owned: AtomicUsize,
    contexts: AtomicUsize,
    peak: AtomicUsize,
    envelope: Option<nrese_exec::Gauge<nrese_exec::Envelope>>,
}

impl Observer {
    pub fn new(limit: usize, envelope: Option<nrese_exec::Envelope>) -> Self {
        Self {
            limit,
            owned: AtomicUsize::new(0),
            contexts: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            envelope: envelope.map(nrese_exec::Gauge::new),
        }
    }

    pub fn used(&self) -> usize {
        self.owned
            .load(Relaxed)
            .saturating_add(self.contexts.load(Relaxed))
    }

    pub fn peak(&self) -> usize {
        self.peak.load(Relaxed)
    }

    /// Replaces `release` bytes the engine holds with `add`; false past the limit.
    pub fn own(&self, add: usize, release: usize) -> bool {
        self.owned.fetch_add(add, Relaxed);
        self.owned.fetch_sub(release, Relaxed);
        self.record()
    }

    /// The contexts' sum; false past the limit.
    pub fn sample(&self, contexts: usize) -> bool {
        self.contexts.store(contexts, Relaxed);
        self.record()
    }

    fn record(&self) -> bool {
        let used = self.used();
        self.peak.fetch_max(used, Relaxed);
        used <= self.limit
            && self
                .envelope
                .as_ref()
                .is_none_or(|envelope| envelope.follow(used))
    }
}

/// Elects one caller per [`nrese_exec::memory::MemoryWatch::INTERVAL_MICROS`] for the
/// engine's periodic readings (process memory, the capacity sample).
#[derive(Debug)]
pub(super) struct Interval {
    started: Instant,
    /// Microseconds after `started` of the last election (0: none yet).
    last: AtomicU64,
}

impl Interval {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            last: AtomicU64::new(0),
        }
    }

    /// The latest election (0: none yet); it changes once per interval at most.
    pub fn last(&self) -> u64 {
        self.last.load(Relaxed)
    }

    pub fn due(&self, now: Instant) -> bool {
        let now = u64::try_from(now.saturating_duration_since(self.started).as_micros())
            .unwrap_or(u64::MAX)
            .max(1);
        let last = self.last.load(Relaxed);
        (last == 0 || now.saturating_sub(last) >= nrese_exec::memory::MemoryWatch::INTERVAL_MICROS)
            && self
                .last
                .compare_exchange(last, now, Relaxed, Relaxed)
                .is_ok()
    }
}

pub(super) fn vec<T>(v: &Vec<T>) -> usize {
    v.capacity() * size_of::<T>()
}

/// Pushes `value`, adding the list's capacity growth to `bytes`.
pub(super) fn push<T>(list: &mut Vec<T>, value: T, bytes: &mut usize) {
    let before = vec(list);
    list.push(value);
    *bytes += vec(list) - before;
}

pub(super) fn nested<T>(v: &Vec<Vec<T>>) -> usize {
    vec(v) + v.iter().map(vec).sum::<usize>()
}

/// Hash table capacity excludes control bytes and spare buckets. Estimate the backing
/// allocation from its 7/8 load factor, rounding up to a power of two, plus a control group.
pub(super) fn table<K, V>(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let buckets = capacity.saturating_mul(8).div_ceil(7).next_power_of_two();
    buckets
        .saturating_mul(size_of::<(K, V)>() + 1)
        .saturating_add(16)
}

pub(super) fn map<K, V, S>(m: &hashbrown::HashMap<K, V, S>) -> usize {
    table::<K, V>(m.capacity())
}

pub(super) fn lists<K, V, S>(m: &hashbrown::HashMap<K, Vec<V>, S>) -> usize {
    map(m) + m.values().map(vec).sum::<usize>()
}

pub(super) fn set<K, S>(s: &hashbrown::HashSet<K, S>) -> usize {
    table::<K, ()>(s.capacity())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_capacity_and_context_samples_share_the_envelope() {
        let observer = Observer::new(100, None);
        assert!(observer.own(20, 0));
        assert!(observer.sample(70));
        assert_eq!((observer.used(), observer.peak()), (90, 90));
        assert!(!observer.sample(90));
        assert_eq!((observer.used(), observer.peak()), (110, 110));
        assert!(observer.own(5, 20));
        assert!(observer.sample(0));
        assert_eq!((observer.used(), observer.peak()), (5, 110));
    }

    #[test]
    fn one_caller_is_elected_per_interval() {
        let interval = Interval::new();
        let now = Instant::now();
        assert!(interval.due(now));
        assert!(!interval.due(now));
        let later = now
            + std::time::Duration::from_micros(
                2 * nrese_exec::memory::MemoryWatch::INTERVAL_MICROS,
            );
        assert!(interval.due(later));
        assert!(!interval.due(later));
    }
}
