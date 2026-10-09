//! Reusable physical workers and bounded batches. The caller owns when work may run;
//! this module owns the pool, its actual width and order-preserving batch execution.
//! A clone shares threads. A smaller allowance never creates another pool.

use std::sync::Arc;

use rayon::prelude::*;

#[derive(Clone)]
pub struct Workers {
    pool: Option<Arc<rayon::ThreadPool>>,
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
            .field("pool_width", &self.pool_width())
            .finish()
    }
}

impl Workers {
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
    /// narrow its parent but cannot increase it. No threads are started here.
    pub fn limited(&self, requested: usize) -> Self {
        Self {
            pool: self.pool.clone(),
            width: if requested == 0 {
                self.width
            } else {
                requested.min(self.width).max(1)
            },
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn pool_width(&self) -> usize {
        self.pool
            .as_ref()
            .map_or(1, |pool| pool.current_num_threads())
    }

    /// The largest number of batch bodies that can execute concurrently for `items`.
    pub fn for_items(&self, items: usize) -> usize {
        self.width.min(items).max(1)
    }

    /// Enters the physical pool. The body is responsible for respecting its allowance
    /// when spawning work; use [`Self::map`] for independent bounded tasks. In particular,
    /// arbitrary nested Rayon iterators do not inherit a smaller allowance automatically.
    pub fn install<F: FnOnce() -> R + Send, R: Send>(&self, f: F) -> R {
        match &self.pool {
            Some(pool) => pool.install(f),
            _ => f(),
        }
    }

    /// O(n) dispatch, retaining input order and at most `width` concurrent bodies.
    /// Task bodies must keep nested work within their own allowance. Contiguous chunks
    /// amortise dispatch and avoid blocking pool threads on an admission semaphore.
    pub fn map<T: Sync, R: Send>(&self, items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
        self.map_range(0..items.len(), |i| f(&items[i]))
    }

    /// As [`Self::map`], without allocating a vector of indices for a batch range.
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
