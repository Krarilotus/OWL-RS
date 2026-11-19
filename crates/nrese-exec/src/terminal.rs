//! One operation's end, recorded once ([`terminal`]). The [`Guard`] goes with the work (a
//! `spawn_blocking` task, a store's build thread); its first [`Guard::end`] is the end, and a
//! guard dropped before any end records its declared fallback, so a panic, a cancelled task
//! or an early return still ends the operation. Readers ([`Terminal`]) read it or wait for it,
//! always with a deadline of their own.
//!
//! It owns no state machine: each owner keeps its own steps; the guard only makes the end
//! total. A panic while the end is recorded or read leaves it readable (the lock recovers).

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

type Shared<O> = Arc<(Mutex<Option<O>>, Condvar)>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One operation's end, recorded once: its reader and its guard, which records `fallback` if
/// it is dropped before any end.
pub fn terminal<O: Clone + Send + 'static>(fallback: O) -> (Terminal<O>, Guard<O>) {
    let guard = Guard {
        shared: Arc::new((Mutex::new(None), Condvar::new())),
        fallback: Some(fallback),
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
    fallback: Option<O>,
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
    /// If no end was recorded, records the fallback and wakes the waiters.
    fn drop(&mut self) {
        if let Some(fallback) = self.fallback.take() {
            self.end(fallback);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::terminal;

    #[derive(Debug, Clone, PartialEq)]
    enum Outcome {
        Committed,
        RolledBack,
        Unknown,
    }

    /// The first end is the end: a later `end` returns `false` and changes nothing, and
    /// neither does the drop's fallback after it.
    #[test]
    fn the_first_end_wins_and_later_ends_change_nothing() {
        let (reader, guard) = terminal(Outcome::Unknown);
        assert!(guard.end(Outcome::Committed));
        assert!(!guard.end(Outcome::RolledBack));
        assert_eq!(reader.get(), Some(Outcome::Committed));
        drop(guard);
        assert_eq!(reader.get(), Some(Outcome::Committed));
    }

    /// A guard dropped before any end records its fallback; until then there is no end.
    #[test]
    fn a_drop_with_no_end_records_the_fallback() {
        let (reader, guard) = terminal(Outcome::Unknown);
        assert_eq!(reader.get(), None);
        drop(guard);
        assert_eq!(reader.get(), Some(Outcome::Unknown));
    }

    /// A panic in the thread that holds the guard records the fallback, and a waiter in
    /// another thread wakes with it long before its own deadline.
    #[test]
    fn a_panic_holding_the_guard_records_the_fallback_and_wakes_the_waiter() {
        let (reader, guard) = terminal(Outcome::Unknown);
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
        let (reader, guard) = terminal(Outcome::Unknown);
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
        let (reader, guard) = terminal(Outcome::Unknown);
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
