//! Parallel work under a lock that a Rayon pool's jobs may wait for: slices, run by the caller
//! and by threads of no pool, which the caller joins.
//!
//! A thread that holds a lock and waits on Rayon work is a deadlock in waiting. A pool thread
//! running a part of that work, while it waits in a join, takes on any other job of its pool,
//! one that wants the lock among them. A fair lock (parking_lot's) makes even a read wait behind
//! a queued writer, so that job waits for the writer, the writer for the holder, and the holder
//! for the part under that job. (Rayon issue #592.)
//!
//! [`map`] and [`map_mut`] run a function on every item. The caller claims items one at a time
//! until none is left, beside up to `lanes` threads that belong to no pool and claim them too.
//! Then it joins those threads: a plain wait, in which it runs no job. A lane runs only these
//! items, so nothing it runs waits for the lock; every item ends, and the caller with them. If
//! the system starts no lane, the caller runs every item alone.
//!
//! **The rule for the function** (the lock-span rule's third form): it takes no lock that a
//! pool job may hold while it waits, and it calls into **no Rayon at all**. That means no
//! `par_*` iterator, `join`, `scope`, `install`, `spawn` or `broadcast`, and no helper that
//! reaches one ([`crate::sort`], [`crate::workers`]). Called from a lane, Rayon work would go
//! to a pool whose threads may all be waiting for the lock: it would never run, and the
//! caller would wait for it forever. The function reads what the caller's guard lends it, and
//! its own item.
//!
//! The one lock here is the claim's: held to take the next item, never across the function.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// `f(i, &items[i])` for every item, in item order: run by the caller and by up to `lanes`
/// threads of no pool (the module docs: `f` calls into no Rayon). A panic in `f` is raised
/// again here once every running item is over.
pub fn map<T: Sync, R: Send>(
    items: &[T],
    lanes: usize,
    f: impl Fn(usize, &T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    run(items.len(), lanes, || {
        let i = next.fetch_add(1, Ordering::Relaxed);
        items.get(i).map(|item| (i, f(i, item)))
    })
}

/// `f(i, &mut items[i])` for every item, in item order: [`map`] for items each slice changes.
pub fn map_mut<T: Send, R: Send>(
    items: &mut [T],
    lanes: usize,
    f: impl Fn(usize, &mut T) -> R + Sync,
) -> Vec<R> {
    let len = items.len();
    let claims = Mutex::new(items.iter_mut().enumerate());
    run(len, lanes, || {
        // The claim's lock is let go before `f` runs.
        let claimed = claims.lock().unwrap_or_else(PoisonError::into_inner).next();
        claimed.map(|(i, item)| (i, f(i, item)))
    })
}

/// Runs `next` until it claims nothing, on the caller and on up to `lanes` scoped threads; the
/// results in claim order.
fn run<R: Send>(len: usize, lanes: usize, next: impl Fn() -> Option<(usize, R)> + Sync) -> Vec<R> {
    let work = || std::iter::from_fn(&next).collect::<Vec<_>>();
    let lanes = lanes.min(len.saturating_sub(1));
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let started: Vec<_> = (0..lanes)
            .map_while(|_| {
                std::thread::Builder::new()
                    .name("nrese-slices".into())
                    .spawn_scoped(scope, work)
                    .ok()
            })
            .collect();
        let mut done = work();
        for lane in started {
            match lane.join() {
                Ok(part) => done.extend(part),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        done
    });
    done.sort_unstable_by_key(|&(i, _)| i);
    debug_assert_eq!(done.len(), len, "every item claimed once");
    done.into_iter().map(|(_, result)| result).collect()
}

/// The lanes worth starting for `work` units of `per_lane` each, beside the caller: none for
/// small work (a lane's start costs tens of microseconds), at most the caller's pool width
/// less one.
pub fn lanes_for(work: usize, per_lane: usize) -> usize {
    (work / per_lane.max(1)).min(rayon::current_num_threads().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    /// Every item runs once and its result keeps its place, with no lane, some, and more lanes
    /// than items; changes made through `map_mut` stay.
    #[test]
    fn every_item_runs_once_in_order() {
        for (len, lanes) in [(0, 3), (1, 0), (1, 4), (97, 0), (97, 3), (97, 200)] {
            let items: Vec<usize> = (0..len).collect();
            assert_eq!(
                map(&items, lanes, |i, &x| (i, x * 2)),
                (0..len).map(|i| (i, 2 * i)).collect::<Vec<_>>()
            );
            let mut items: Vec<usize> = (0..len).collect();
            let seen = map_mut(&mut items, lanes, |i, x| {
                *x += 1;
                i
            });
            assert_eq!(seen, (0..len).collect::<Vec<_>>());
            assert_eq!(items, (1..=len).collect::<Vec<_>>());
        }
    }

    /// The lanes run beside the caller (here all four items at once, each waiting for the
    /// others), on threads of no Rayon pool.
    #[test]
    fn lanes_run_beside_the_caller_off_every_pool() {
        let all = Barrier::new(4);
        let items = [(); 4];
        let off_pool = map(&items, 3, |_, ()| {
            all.wait();
            rayon::current_thread_index().is_none()
        });
        assert_eq!(off_pool, [true; 4]);
    }

    /// Called on a Rayon pool's thread whose every thread waits for a lock the caller holds,
    /// the work still ends: the caller and its lanes run it, and the pool runs none of it.
    #[test]
    fn work_ends_while_every_pool_thread_waits_for_the_callers_lock() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let lock = std::sync::Arc::new(std::sync::RwLock::new(()));
        let held = lock.write().unwrap();
        // The pool's other thread blocks on the lock; the caller is the pool's first thread.
        let waiting = std::sync::Arc::new(AtomicUsize::new(0));
        let (lock2, waiting2) = (
            std::sync::Arc::clone(&lock),
            std::sync::Arc::clone(&waiting),
        );
        pool.spawn(move || {
            waiting2.fetch_add(1, Ordering::SeqCst);
            drop(lock2.read());
            // (A poisoned lock is fine here: only the wait matters.)
        });
        while waiting.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        let items: Vec<u64> = (0..64).collect();
        let sum: u64 = pool.install(|| map(&items, 3, |_, &x| x).into_iter().sum());
        assert_eq!(sum, (0..64).sum());
        drop(held);
    }

    /// A panic in an item, on a lane or the caller, reaches the caller once the others are
    /// over; the other items still ran.
    #[test]
    fn a_panic_reaches_the_caller() {
        let ran = AtomicUsize::new(0);
        let items: Vec<usize> = (0..32).collect();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            map(&items, 3, |i, _| {
                ran.fetch_add(1, Ordering::SeqCst);
                assert!(i != 7, "the injected item panic");
            })
        }));
        assert!(outcome.is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 32);
    }

    #[test]
    fn small_work_starts_no_lane() {
        assert_eq!(lanes_for(10, 1 << 14), 0);
        assert!(lanes_for(usize::MAX, 1) < rayon::current_num_threads());
    }
}
