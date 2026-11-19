//! One operation's end, recorded once ([`terminal`]). The [`Guard`] goes with the work (a
//! `spawn_blocking` task, a store's build thread); its first [`Guard::end`] is the end, and a
//! guard dropped before any end records what its fallback returns then, so a panic, a
//! cancelled task or an early return still ends the operation. Readers ([`Terminal`]) read it
//! or wait for it, always with a deadline of their own.
//!
//! It owns no state machine: each owner keeps its own steps; the guard only makes the end
//! total. A panic while the end is recorded or read leaves it readable (the lock recovers).

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

type Shared<O> = Arc<(Mutex<Option<O>>, Condvar)>;

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

/// The reading side: cheap to clone, `Send + Sync + 'static`.
#[derive(Clone)]
pub struct Terminal<O> {
    shared: Shared<O>,
}

impl<O: Clone> Terminal<O> {
    /// The recorded end, if there is one.
    pub fn get(&self) -> Option<O> {
        lock(&self.shared.0).clone()
    }

    /// Waits for the end until `deadline`. Returns `None` if the deadline passed first.
    /// It never waits without a deadline.
    pub fn wait_until(&self, deadline: Instant) -> Option<O> {
        let (end, ended) = &*self.shared;
        let wait = deadline.saturating_duration_since(Instant::now());
        let waited = ended.wait_timeout_while(lock(end), wait, |end| end.is_none());
        let (end, _) = waited.unwrap_or_else(PoisonError::into_inner);
        end.clone()
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
        *end = Some(outcome);
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
