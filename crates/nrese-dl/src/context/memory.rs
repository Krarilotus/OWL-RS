//! Checkpoint observations of saturation capacity. Contexts publish locally; a sampled
//! sum guides the configured task envelope without synchronising every inference.
//! Observations can overlap or lag at handoffs, so their peak is an estimate, not an
//! allocation ceiling or process RSS. Auxiliary owners report their live capacities.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed},
};
use std::time::Instant;

#[derive(Debug)]
pub(super) struct Task {
    budget: nrese_exec::Budget,
    limit: usize,
    contexts: AtomicUsize,
    peak: AtomicUsize,
    started: Instant,
    last: AtomicU64,
    exhausted: Arc<AtomicBool>,
}

impl Task {
    pub fn new(limit: usize, exhausted: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            budget: nrese_exec::Budget::new(usize::MAX),
            limit,
            contexts: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: Instant::now(),
            last: AtomicU64::new(0),
            exhausted,
        })
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted.load(Relaxed)
    }

    pub fn used(&self) -> usize {
        self.budget
            .used()
            .saturating_add(self.contexts.load(Relaxed))
    }

    pub fn peak(&self) -> usize {
        self.peak.load(Relaxed)
    }

    /// Elect at most one aggregate scan per process-watch interval. This is a sampling
    /// cadence, not a memory allowance. A quiescent run forces its final observation.
    pub fn sample_due(&self, force: bool) -> bool {
        let now = u64::try_from(self.started.elapsed().as_micros())
            .unwrap_or(u64::MAX)
            .max(1);
        let last = self.last.load(Relaxed);
        if !force
            && last != 0
            && now.saturating_sub(last) < nrese_exec::memory::MemoryWatch::INTERVAL_MICROS
        {
            return false;
        }
        self.last
            .compare_exchange(last, now, Relaxed, Relaxed)
            .is_ok()
    }

    pub fn observe(&self, contexts: usize) {
        self.contexts.store(contexts, Relaxed);
        self.record();
    }

    fn record(&self) -> bool {
        let used = self.used();
        self.peak.fetch_max(used, Relaxed);
        if used > self.limit {
            self.exhausted.store(true, Relaxed);
        }
        !self.exhausted()
    }
}

/// One auxiliary owner's capacity (preparation, result or scheduler buffers). Context
/// inference uses local observations instead. Over-limit observations remain visible.
#[derive(Debug, Default)]
pub(super) struct Charge {
    task: Option<Arc<Task>>,
    bytes: usize,
}

impl Charge {
    pub fn new(task: Option<&Arc<Task>>) -> Self {
        Self {
            task: task.cloned(),
            bytes: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.task.is_some()
    }

    pub fn set(&mut self, bytes: usize) -> bool {
        let Some(task) = &self.task else {
            return true;
        };
        if bytes == self.bytes {
            return !task.exhausted();
        }
        if bytes > self.bytes {
            if task.budget.charge(bytes - self.bytes).is_err() {
                task.exhausted.store(true, Relaxed);
                return false;
            }
        } else if bytes < self.bytes {
            task.budget.release(self.bytes - bytes);
        }
        self.bytes = bytes;
        task.record()
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        if self.bytes != 0
            && let Some(task) = &self.task
        {
            task.budget.release(self.bytes);
        }
    }
}

pub(super) fn vec<T>(v: &Vec<T>) -> usize {
    v.capacity() * size_of::<T>()
}

pub(super) fn nested<T>(v: &Vec<Vec<T>>) -> usize {
    vec(v) + v.iter().map(vec).sum::<usize>()
}

/// Hash table capacity excludes control bytes and spare buckets. Estimate the backing
/// allocation from its 7/8 load factor, rounding up to a power of two, plus a control group.
/// Allocator bookkeeping and thread stacks are outside this capacity contract.
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
    fn sampled_contexts_and_auxiliary_owners_share_the_envelope() {
        let task = Task::new(100, Arc::new(AtomicBool::new(false)));
        let mut owner = Charge::new(Some(&task));
        assert!(owner.set(20));
        task.observe(70);
        assert_eq!((task.used(), task.peak()), (90, 90));
        assert!(!task.exhausted());
        task.observe(90);
        assert_eq!((task.used(), task.peak()), (110, 110));
        assert!(task.exhausted());
        drop(owner);
        task.observe(0);
        assert_eq!((task.used(), task.peak()), (0, 110));
        assert!(
            task.exhausted(),
            "a stopped run cannot become complete again"
        );
    }

    #[test]
    fn stopping_still_reconciles_owned_capacity() {
        let stopped = Arc::new(AtomicBool::new(false));
        let task = Task::new(100, Arc::clone(&stopped));
        let mut owner = Charge::new(Some(&task));
        assert!(owner.set(60));
        stopped.store(true, Relaxed);
        assert!(!owner.set(60));
        assert!(!owner.set(20));
        assert_eq!((task.used(), task.peak()), (20, 60));
        drop(owner);
        assert_eq!(task.used(), 0);
        assert!(task.exhausted());
    }

    #[test]
    fn auxiliary_owners_move_release_and_keep_tasks_independent() {
        let first = Task::new(100, Arc::new(AtomicBool::new(false)));
        let second = Task::new(100, Arc::new(AtomicBool::new(false)));
        let mut owner = Charge::new(Some(&first));
        assert!(owner.set(60));
        let mut moved = owner;
        assert!(moved.set(40));
        let mut other = Charge::new(Some(&second));
        assert!(other.set(100));
        assert!(!moved.set(101));
        assert_eq!((first.used(), first.peak()), (101, 101));
        assert!(!second.exhausted());
        drop(moved);
        drop(other);
        assert_eq!((first.used(), second.used()), (0, 0));
    }
}
