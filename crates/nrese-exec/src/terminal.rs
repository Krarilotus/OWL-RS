//! One operation's end, recorded once ([`terminal`]). The [`Guard`] goes with the work (a
//! `spawn_blocking` task, a store's build thread); its first [`Guard::end`] is the end, and a
//! guard dropped before any end records what its fallback returns then, so a panic, a
//! cancelled task or an early return still ends the operation. Readers ([`Terminal`]) read it
//! or wait for it, always with a deadline of their own.
//!
//! It owns no state machine: each owner keeps its own steps; the guard only makes the end
//! total. No code of the caller's runs under the lock: the end is kept as an `Arc`, and `O`
//! is cloned after the lock is released.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

type Shared<O> = Arc<(Mutex<Option<Arc<O>>>, Condvar)>;

/// The longest single wait underneath `wait_until`: a far deadline becomes several finite
/// waits, never one the platform reads as untimed (Windows does above about 49.7 days).
const WAIT_CAP: Duration = Duration::from_secs(3600);

type Fallback<O> = Box<dyn FnOnce() -> O + Send>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One operation's end, recorded once: its reader and its guard, which records what
/// `fallback` returns if it is dropped before any end.
///
/// `fallback` runs at most once: on the dropping thread, only if no end was recorded, before
/// the waiters wake. It decides by state it captures, such as which side passed the point of
/// no return. It must not panic (during an unwind that aborts), and it must not take a lock
/// that a waiter of this cell may hold. A fixed fallback is `move || value`.
pub fn terminal<O, F>(fallback: F) -> (Terminal<O>, Guard<O>)
where
    O: Clone + Send + 'static,
    F: FnOnce() -> O + Send + 'static,
{
    let guard = Guard {
        shared: Arc::new((Mutex::new(None), Condvar::new())),
        fallback: Some(Box::new(fallback)),
    };
    (guard.terminal(), guard)
}

/// The reading side: cheap to clone, `Send + Sync + 'static` for an `O` that is `Send + Sync`
/// (the end is shared as an `Arc<O>`).
#[derive(Clone)]
pub struct Terminal<O> {
    shared: Shared<O>,
}

impl<O: Clone> Terminal<O> {
    /// The recorded end, if there is one.
    pub fn get(&self) -> Option<O> {
        let end = lock(&self.shared.0).clone();
        end.as_deref().cloned()
    }

    /// Waits for the end until `deadline`. Returns the end if one is recorded when the wait
    /// stops, whether at or after its deadline; `None` only if none is recorded by then. The
    /// deadline bounds the waiting, not the end's age, and covers readiness only: `O::clone`
    /// runs after the wait, on the caller's thread. It never waits without a deadline.
    pub fn wait_until(&self, deadline: Instant) -> Option<O> {
        let (end, ended) = &*self.shared;
        let mut held = lock(end);
        while held.is_none() && Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            let waited = ended.wait_timeout(held, left.min(WAIT_CAP));
            held = waited.unwrap_or_else(PoisonError::into_inner).0;
        }
        let end = held.clone();
        drop(held);
        end.as_deref().cloned()
    }
}

/// The writing side: one per operation, owned and movable into a `'static` task. It is not
/// `Clone`.
pub struct Guard<O> {
    shared: Shared<O>,
    /// Taken when the guard is dropped.
    fallback: Option<Fallback<O>>,
}

impl<O> Guard<O> {
    /// Records `outcome` if nothing is recorded yet, and wakes the waiters. Returns whether
    /// this end was the one recorded.
    pub fn end(&self, outcome: O) -> bool {
        let (end, ended) = &*self.shared;
        let mut end = lock(end);
        if end.is_some() {
            return false;
        }
        *end = Some(Arc::new(outcome));
        drop(end);
        ended.notify_all();
        true
    }

    /// A reader of the same end.
    pub fn terminal(&self) -> Terminal<O> {
        Terminal {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<O> Drop for Guard<O> {
    /// If no end was recorded, records what the fallback returns and wakes the waiters. No
    /// other end can come meanwhile: this guard is the only writer, and it is being dropped.
    fn drop(&mut self) {
        let Some(fallback) = self.fallback.take() else {
            return;
        };
        if lock(&self.shared.0).is_none() {
            self.end(fallback());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::terminal;

    #[derive(Debug, Clone, PartialEq)]
    enum Outcome {
        Committed,
        RolledBack,
        NotRun,
        Unknown,
    }

    /// The first end is the end: a later `end` returns `false` and changes nothing, and the
    /// drop after it neither records nor runs the fallback.
    #[test]
    fn the_first_end_wins_and_later_ends_change_nothing() {
        let fallback_ran = Arc::new(AtomicBool::new(false));
        let ran = Arc::clone(&fallback_ran);
        let (reader, guard) = terminal(move || {
            ran.store(true, Ordering::SeqCst);
            Outcome::Unknown
        });
        assert!(guard.end(Outcome::Committed));
        assert!(!guard.end(Outcome::RolledBack));
        assert_eq!(reader.get(), Some(Outcome::Committed));
        drop(guard);
        assert_eq!(reader.get(), Some(Outcome::Committed));
        assert!(
            !fallback_ran.load(Ordering::SeqCst),
            "the fallback ran after an end"
        );
    }

    /// A guard dropped before any end records its fallback; until then there is no end.
    #[test]
    fn a_drop_with_no_end_records_the_fallback() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        assert_eq!(reader.get(), None);
        drop(guard);
        assert_eq!(reader.get(), Some(Outcome::Unknown));
    }

    /// The fallback decides at drop, by what it reads then: a flag set during the run (the
    /// point of no return passed) after the guard was made is what the end records.
    #[test]
    fn a_fallback_records_the_flag_as_it_is_at_drop() {
        let fallback = |passed: Arc<AtomicBool>| {
            move || {
                if passed.load(Ordering::SeqCst) {
                    Outcome::Unknown
                } else {
                    Outcome::NotRun
                }
            }
        };
        let passed = Arc::new(AtomicBool::new(false));
        let (reader, guard) = terminal(fallback(Arc::clone(&passed)));
        passed.store(true, Ordering::SeqCst);
        drop(guard);
        assert_eq!(reader.get(), Some(Outcome::Unknown));

        let not_passed = Arc::new(AtomicBool::new(false));
        let (reader, guard) = terminal(fallback(Arc::clone(&not_passed)));
        drop(guard);
        assert_eq!(reader.get(), Some(Outcome::NotRun));
    }

    /// A panic in the thread that holds the guard records the fallback, and a waiter in
    /// another thread wakes with it long before its own deadline.
    #[test]
    fn a_panic_holding_the_guard_records_the_fallback_and_wakes_the_waiter() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let both_started = Arc::new(Barrier::new(2));
        let waiter = {
            let both_started = Arc::clone(&both_started);
            thread::spawn(move || {
                both_started.wait();
                let started = Instant::now();
                let end = reader.wait_until(started + Duration::from_secs(20));
                (end, started.elapsed())
            })
        };
        let holder = thread::spawn(move || {
            let _guard = guard;
            both_started.wait();
            thread::sleep(Duration::from_millis(50));
            panic!("the task holding the guard panics");
        });
        assert!(holder.join().is_err());
        let (end, waited) = waiter.join().expect("the waiter does not panic");
        assert_eq!(end, Some(Outcome::Unknown));
        assert!(
            waited < Duration::from_secs(10),
            "the waiter slept to its deadline: {waited:?}"
        );
    }

    /// An end wakes every waiter, not one: two waiters enter the wait together, the end comes
    /// once both are past the barrier and have had time to park, and both wake long before
    /// their deadlines.
    #[test]
    fn an_end_wakes_every_waiter() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let all_started = Arc::new(Barrier::new(3));
        let waiters: Vec<_> = (0..2)
            .map(|_| {
                let (reader, all_started) = (reader.clone(), Arc::clone(&all_started));
                thread::spawn(move || {
                    all_started.wait();
                    let started = Instant::now();
                    let end = reader.wait_until(started + Duration::from_secs(20));
                    (end, started.elapsed())
                })
            })
            .collect();
        all_started.wait();
        thread::sleep(Duration::from_millis(200));
        assert!(guard.end(Outcome::Committed));
        for waiter in waiters {
            let (end, waited) = waiter.join().expect("a waiter does not panic");
            assert_eq!(end, Some(Outcome::Committed));
            assert!(
                waited < Duration::from_secs(10),
                "a waiter slept to its deadline: {waited:?}"
            );
        }
    }

    /// `wait_until` gives up at its deadline, not before, when no end comes; and returns the
    /// end, before its deadline, when the end comes first.
    #[test]
    fn wait_until_returns_none_at_its_deadline_and_the_end_when_it_comes_first() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let started = Instant::now();
        let deadline = started + Duration::from_millis(100);
        assert_eq!(reader.wait_until(deadline), None);
        assert!(Instant::now() >= deadline, "it gave up before its deadline");

        let ender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            guard.end(Outcome::Committed)
        });
        let started = Instant::now();
        let end = reader.wait_until(started + Duration::from_secs(20));
        let waited = started.elapsed();
        assert_eq!(end, Some(Outcome::Committed));
        assert!(
            waited < Duration::from_secs(10),
            "it slept to its deadline: {waited:?}"
        );
        assert!(ender.join().expect("the ender does not panic"));
    }

    /// The guard and a reader move into `std::thread::spawn`, which takes only `Send +
    /// 'static` values: this test compiling is the check.
    #[test]
    fn the_guard_and_its_reader_move_into_a_spawned_thread() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let second_reader = guard.terminal();
        let ender = thread::spawn(move || guard.end(Outcome::Committed));
        let watcher = thread::spawn(move || {
            second_reader.wait_until(Instant::now() + Duration::from_secs(20))
        });
        assert!(ender.join().expect("the ender does not panic"));
        assert_eq!(
            watcher.join().expect("the watcher does not panic"),
            Some(Outcome::Committed)
        );
        assert_eq!(reader.get(), Some(Outcome::Committed));
    }
}
