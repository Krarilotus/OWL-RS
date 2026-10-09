//! Task-owned saturation capacity. This is checkpoint accounting, not an allocator
//! ceiling: growth between checkpoints may overshoot. No process RSS is attributed to
//! a task. Owners keep their charge when moved and release it when dropped.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};

#[derive(Debug)]
pub(super) struct Task {
    budget: nrese_exec::Budget,
    exhausted: AtomicBool,
}

impl Task {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            budget: nrese_exec::Budget::new(limit),
            exhausted: AtomicBool::new(false),
        })
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted.load(Relaxed)
    }

    pub fn used(&self) -> usize {
        self.budget.used()
    }

    pub fn peak(&self) -> usize {
        self.budget.peak()
    }
}

/// One owner's last accounted capacity. A failed growth leaves its previous reservation
/// intact and marks the whole task exhausted; dropping it releases exactly that amount.
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
        if bytes > self.bytes {
            if task.budget.charge(bytes - self.bytes).is_err() {
                task.exhausted.store(true, Relaxed);
                return false;
            }
        } else if bytes < self.bytes {
            task.budget.release(self.bytes - bytes);
        }
        self.bytes = bytes;
        !task.exhausted()
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
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
    fn charges_move_release_and_isolate_tasks_even_after_failed_growth() {
        let first = Task::new(100);
        let second = Task::new(100);
        let mut owner = Charge::new(Some(&first));
        assert!(owner.set(60));
        let mut moved = owner;
        assert_eq!(first.used(), 60);
        assert!(moved.set(40));
        let mut other = Charge::new(Some(&second));
        assert!(other.set(100));
        assert!(!moved.set(101));
        assert!(first.exhausted());
        assert!(!second.exhausted());
        drop(moved);
        drop(other);
        assert_eq!((first.used(), second.used()), (0, 0));
        assert_eq!((first.peak(), second.peak()), (60, 100));
    }

    #[test]
    fn concurrent_owners_share_one_task_and_return_all_reservations() {
        let task = Task::new(400);
        let barrier = Arc::new(std::sync::Barrier::new(4));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let task = Arc::clone(&task);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    let mut owner = Charge::new(Some(&task));
                    assert!(owner.set(100));
                    barrier.wait();
                    assert_eq!(task.used(), 400);
                    barrier.wait();
                });
            }
        });
        assert_eq!((task.used(), task.peak()), (0, 400));
    }
}
