//! Execution handles and bounded batches. The policy owner chooses current execution
//! or an owned pool. A clone shares that choice; a smaller allowance creates no pool.

use std::sync::Arc;

use rayon::prelude::*;

#[derive(Clone)]
pub struct Workers {
    pool: Option<Arc<rayon::ThreadPool>>,
    // Zero inherits the current Rayon width lazily; one without a pool is serial.
    width: usize,
}

impl PartialEq for Workers {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && match (&self.pool, &other.pool) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

impl Eq for Workers {}

impl std::fmt::Debug for Workers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workers")
            .field("width", &self.width)
            .field(
                "pool_width",
                &self.pool.as_ref().map(|p| p.current_num_threads()),
            )
            .finish()
    }
}

impl Workers {
    /// Keeps serial work on its caller and parallel batches in the current Rayon
    /// registry (the global registry outside a worker). Creates no pool or admission
    /// queue. This is an operation allowance, not a cap across concurrent callers.
    pub fn current() -> Self {
        Self {
            pool: None,
            width: 0,
        }
    }

    /// Creates a reusable pool; zero requests the machine's available parallelism.
    /// Failure is returned to the policy owner, never redirected to an ambient pool.
    pub fn new(threads: usize) -> Result<Self, rayon::ThreadPoolBuildError> {
        if threads == 1 {
            return Ok(Self::serial());
        }
        Self::pooled(threads)
    }

    /// A physical execution owner, including a one-thread pool. Serial tasks submitted
    /// by different callers then share that thread instead of running on every caller.
    pub fn pooled(threads: usize) -> Result<Self, rayon::ThreadPoolBuildError> {
        let width = match threads {
            0 => std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
            n => n,
        };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(width).build()?;
        Ok(Self {
            width: pool.current_num_threads(),
            pool: Some(Arc::new(pool)),
        })
    }

    /// Runs serially on the caller, including the explicit pool-failure fallback.
    pub fn serial() -> Self {
        Self {
            pool: None,
            width: 1,
        }
    }

    /// An allowance within this pool. Zero keeps the current allowance; a child may
    /// narrow its parent but cannot increase it. No owned pool is created; resolving
    /// a wider current allowance may initialise Rayon's global registry.
    pub fn limited(&self, requested: usize) -> Self {
        Self {
            pool: self.pool.clone(),
            width: if requested == 0 {
                self.width
            } else if requested == 1 {
                1
            } else {
                requested.min(self.width()).max(1)
            },
        }
    }

    pub fn width(&self) -> usize {
        if self.pool.is_none() && self.width != 1 {
            let available = rayon::current_num_threads();
            if self.width == 0 {
                available
            } else {
                self.width.min(available)
            }
        } else {
            self.width
        }
    }

    pub fn pool_width(&self) -> usize {
        match &self.pool {
            Some(pool) => pool.current_num_threads(),
            None if self.width == 1 => 1,
            None => rayon::current_num_threads(),
        }
    }

    /// The largest number of batch bodies that can execute concurrently for `items`.
    pub fn for_items(&self, items: usize) -> usize {
        self.width().min(items).max(1)
    }

    /// Enters an owned pool, or runs inline for current/serial execution. The body
    /// is responsible for respecting its allowance
    /// when spawning work; use [`Self::map`] for independent bounded tasks. In particular,
    /// arbitrary nested Rayon iterators do not inherit a smaller allowance automatically.
    pub fn install<F: FnOnce() -> R + Send, R: Send>(&self, f: F) -> R {
        match &self.pool {
            Some(pool) => pool.install(f),
            _ => f(),
        }
    }

    /// O(n) dispatch of irregular tasks, retaining input order and at most `width`
    /// concurrent bodies. Narrow lanes claim the next item rather than keeping a
    /// static share of possibly skewed search work. Task bodies must constrain children.
    pub fn map<T: Sync, R: Send>(&self, items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
        let width = self.for_items(items.len());
        if width == 1 || width == self.pool_width() || items.len() <= width {
            return self.map_range(0..items.len(), |i| f(&items[i]));
        }
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        let next = AtomicUsize::new(0);
        let lanes: Vec<Vec<(usize, R)>> = self.install(|| {
            (0..width)
                .into_par_iter()
                .map(|_| {
                    std::iter::from_fn(|| {
                        let i = next.fetch_add(1, Relaxed);
                        items.get(i).map(|item| (i, f(item)))
                    })
                    .collect()
                })
                .collect()
        });
        // Dense input indices avoid sorting and unsafe shared result writes. The extra
        // O(n) slots are used only by narrowed irregular batches, never regular kernels.
        let mut ordered: Vec<Option<R>> =
            std::iter::repeat_with(|| None).take(items.len()).collect();
        for (i, value) in lanes.into_iter().flatten() {
            ordered[i] = Some(value);
        }
        ordered
            .into_iter()
            .map(|value| value.expect("every input is claimed once"))
            .collect()
    }

    /// O(n) dispatch of regular ranges, without an allocated vector of indices.
    /// Narrow allowances use contiguous chunks to amortise dispatch. Prefer [`Self::map`]
    /// for irregular search tasks; encoding windows have regular, bounded block work.
    pub fn map_range<R: Send>(
        &self,
        items: std::ops::Range<usize>,
        f: impl Fn(usize) -> R + Sync + Send,
    ) -> Vec<R> {
        let width = self.for_items(items.len());
        if width == 1 {
            return self.install(|| items.map(f).collect());
        }
        self.install(|| {
            if width == self.pool_width() {
                items.into_par_iter().map(f).collect()
            } else {
                let chunk = items.len().div_ceil(width);
                (0..items.len().div_ceil(chunk))
                    .into_par_iter()
                    .flat_map_iter(|part| {
                        let start = items.start + part * chunk;
                        (start..start.saturating_add(chunk).min(items.end)).map(&f)
                    })
                    .collect()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn current_execution_keeps_inline_work_and_reuses_parallel_workers() {
        let current = Workers::current();
        let caller = std::thread::current().id();
        assert_eq!(current.install(|| std::thread::current().id()), caller);
        let owner = Workers::pooled(4).unwrap();
        let narrowed = owner.install(|| {
            assert_eq!(current.width(), 4);
            let narrowed = current.limited(2);
            assert_eq!(narrowed.pool_width(), 4);
            let active = AtomicUsize::new(0);
            let peak = AtomicUsize::new(0);
            let barrier = std::sync::Barrier::new(2);
            let values = narrowed.map(&[0, 1, 2, 3], |&i| {
                assert_eq!(rayon::current_num_threads(), 4);
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                if i < 2 {
                    barrier.wait();
                }
                active.fetch_sub(1, Ordering::SeqCst);
                i * 3
            });
            assert_eq!(values, [0, 3, 6, 9]);
            assert_eq!(peak.load(Ordering::SeqCst), 2);
            assert_eq!(
                current.map_range(3..19, |i| i * 2),
                (3..19).map(|i| i * 2).collect::<Vec<_>>()
            );
            narrowed
        });
        Workers::pooled(3).unwrap().install(|| {
            assert_eq!(current.width(), 3);
            assert_eq!(narrowed.width(), 2);
            assert_eq!(narrowed.limited(3).width(), 2);
        });
        Workers::pooled(1).unwrap().install(|| {
            assert_eq!(narrowed.width(), 1);
            assert_eq!(narrowed.map(&[1, 2], |n| n * 2), [2, 4]);
        });
    }

    #[test]
    fn narrowed_batches_keep_order_and_do_not_start_more_workers() {
        let physical = Workers::new(4).unwrap();
        let workers = physical.limited(2);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let input: Vec<_> = (0..128).collect();
        let output = workers.map(&input, |n| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            for _ in 0..64 {
                std::thread::yield_now();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            n * 3
        });
        assert_eq!(output, input.iter().map(|n| n * 3).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert_eq!(workers.pool_width(), 4);
        assert_eq!(workers.limited(8).width(), 2);
        assert!(Arc::ptr_eq(
            physical.pool.as_ref().unwrap(),
            workers.pool.as_ref().unwrap()
        ));
    }

    #[test]
    fn a_free_narrow_lane_can_take_work_behind_a_busy_lane() {
        use std::sync::{Condvar, Mutex};
        let workers = Workers::pooled(4).unwrap().limited(2);
        let progress = (Mutex::new(false), Condvar::new());
        let items: Vec<_> = (0..8).collect();
        let output = workers.map(&items, |&i| {
            if i == 0 {
                let (done, wait) = progress
                    .1
                    .wait_timeout_while(
                        progress.0.lock().unwrap(),
                        std::time::Duration::from_secs(5),
                        |done| !*done,
                    )
                    .unwrap();
                assert!(*done && !wait.timed_out(), "an idle lane must claim item 1");
            } else if i == 1 {
                *progress.0.lock().unwrap() = true;
                progress.1.notify_all();
            }
            i
        });
        assert_eq!(output, items);
    }

    #[test]
    fn serial_allowances_use_the_owner_and_standalone_work_stays_on_the_caller() {
        let workers = Workers::new(2).unwrap().limited(1);
        let caller = std::thread::current().id();
        let threads = workers.map(&[0, 1], |_| std::thread::current().id());
        assert_eq!(threads[0], threads[1]);
        assert_ne!(threads[0], caller);
        assert_eq!(
            Workers::serial().install(|| std::thread::current().id()),
            caller
        );
        assert_ne!(
            Workers::pooled(1)
                .unwrap()
                .install(|| std::thread::current().id()),
            caller
        );
        assert!(workers.map::<u8, u8>(&[], |_| unreachable!()).is_empty());
        assert_eq!(workers.for_items(0), 1);
    }
}
