//! One operation's end, recorded once ([`terminal`]). The [`Guard`] goes with the work (a
//! `spawn_blocking` task, a store's build thread); its first [`Guard::end`] is the end, and a
//! guard dropped before any end records what its fallback returns then, so a panic, a
//! cancelled task or an early return still ends the operation. Readers ([`Terminal`]) read it
//! or wait for it, on a thread or in a task, always with a deadline of their own.
//!
//! It owns no state machine: each owner keeps its own steps; the guard only makes the end
//! total. No code of the caller's runs under the lock: the end is kept as an `Arc`, `O` is
//! cloned after the lock is released, and wakers are woken after it.

use std::future::poll_fn;
use std::pin::pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Poll, Waker, ready};
use std::time::{Duration, Instant};

type Shared<O> = Arc<(Mutex<State<O>>, Condvar)>;

/// The end, once recorded, and until then the async waiters' wakers, each under the id of the
/// `wait_or` future that registered it.
struct State<O> {
    end: Option<Arc<O>>,
    wakers: Vec<(u64, Waker)>,
    next: u64,
}

/// A `wait_or` future's place in the waker list, emptied when the future completes or is
/// dropped, so waits given up leave nothing behind.
struct Slot<'a, O> {
    shared: &'a Shared<O>,
    id: Option<u64>,
}

impl<O> Drop for Slot<'_, O> {
    fn drop(&mut self) {
        let Some(id) = self.id else { return };
        let mut state = lock(&self.shared.0);
        let at = state.wakers.iter().position(|(slot, _)| *slot == id);
        let _removed = at.map(|at| state.wakers.swap_remove(at));
        drop(state); // The waker is dropped after the lock is released.
    }
}

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
    O: Clone + Send + Sync + 'static,
    F: FnOnce() -> O + Send + 'static,
{
    let guard = Guard {
        shared: Arc::new((
            Mutex::new(State {
                end: None,
                wakers: Vec::new(),
                next: 0,
            }),
            Condvar::new(),
        )),
        fallback: Some(Box::new(fallback)),
    };
    (guard.terminal(), guard)
}

/// The reading side: cheap to clone, `Send + Sync + 'static` (the end is shared as an
/// `Arc<O>`, so `O` is `Send + Sync`).
#[derive(Clone)]
pub struct Terminal<O> {
    shared: Shared<O>,
}

impl<O: Clone> Terminal<O> {
    /// The recorded end, if there is one.
    pub fn get(&self) -> Option<O> {
        let end = lock(&self.shared.0).end.clone();
        end.as_deref().cloned()
    }

    /// Waits for the end until `deadline`. Returns the end if one is recorded when the wait
    /// stops, whether at or after its deadline; `None` only if none is recorded by then. The
    /// deadline bounds the waiting, not the end's age, and covers readiness only: `O::clone`
    /// runs after the wait, on the caller's thread. It never waits without a deadline.
    pub fn wait_until(&self, deadline: Instant) -> Option<O> {
        let (end, ended) = &*self.shared;
        let mut held = lock(end);
        while held.end.is_none() && Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            let waited = ended.wait_timeout(held, left.min(WAIT_CAP));
            held = waited.unwrap_or_else(PoisonError::into_inner).0;
        }
        let end = held.end.clone();
        drop(held);
        end.as_deref().cloned()
    }

    /// Waits for the end without holding a thread, until `timer` completes: the caller's
    /// deadline, for example `tokio::time::sleep_until(deadline.into())`. The same answer rule
    /// as `wait_until`: the end if one is recorded when the wait stops, `None` only if none is
    /// recorded by then. A wait dropped before it completes takes its waker out of the list.
    pub async fn wait_or<T: Future<Output = ()>>(&self, timer: T) -> Option<O> {
        let mut timer = pin!(timer);
        let mut slot = Slot {
            shared: &self.shared,
            id: None,
        };
        let end = poll_fn(|cx| {
            // A waker is the runtime's code: it is cloned and dropped outside the lock.
            let waker = cx.waker().clone();
            let mut state = lock(&self.shared.0);
            if state.end.is_some() {
                let recorded = state.end.clone();
                drop(state); // The unused clone is dropped after the lock is released.
                return Poll::Ready(recorded);
            }
            // The first poll takes a fresh id; later ones replace that id's waker.
            let id = *slot.id.get_or_insert(state.next);
            let replaced = match state.wakers.iter_mut().find(|(at, _)| *at == id) {
                Some((_, old)) => Some(std::mem::replace(old, waker)),
                None => {
                    state.next += 1;
                    state.wakers.push((id, waker));
                    None
                }
            };
            drop(state);
            drop(replaced);
            ready!(timer.as_mut().poll(cx));
            Poll::Ready(lock(&self.shared.0).end.clone())
        })
        .await;
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
        let (state, ended) = &*self.shared;
        let mut state = lock(state);
        if state.end.is_some() {
            return false;
        }
        state.end = Some(Arc::new(outcome));
        let wakers = std::mem::take(&mut state.wakers);
        drop(state);
        ended.notify_all();
        wakers.into_iter().for_each(|(_, waker)| waker.wake());
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
        if lock(&self.shared.0).end.is_none() {
            self.end(fallback());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready as ready_now};
    use std::pin::pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, mpsc};
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{Terminal, lock, terminal};

    /// The waker of the tests' single-threaded executor (a task polled by hand): it counts
    /// its wakes.
    struct Wakes(AtomicUsize);

    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting_waker() -> (Arc<Wakes>, Waker) {
        let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
        (Arc::clone(&wakes), Waker::from(wakes))
    }

    /// How many wakers wait on `reader`'s end.
    fn wakers_of(reader: &Terminal<Outcome>) -> usize {
        lock(&reader.shared.0).wakers.len()
    }

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

    // Spec: the worker's final end wakes a waiter after its fallback captures are released.
    #[test]
    fn request_release_releases_fallback_captures_before_waking_a_waiter() {
        struct AtWake {
            capture: Arc<()>,
            count: AtomicUsize,
        }

        impl Wake for AtWake {
            fn wake(self: Arc<Self>) {
                self.count
                    .store(Arc::strong_count(&self.capture), Ordering::SeqCst);
            }
        }

        let witness = Arc::new(AtWake {
            capture: Arc::new(()),
            count: AtomicUsize::new(0),
        });
        let captured = Arc::clone(&witness.capture);
        let (reader, guard) = terminal(move || {
            drop(captured);
            Outcome::Unknown
        });
        let waker = Waker::from(Arc::clone(&witness));
        let mut cx = Context::from_waker(&waker);
        let mut waiter = pin!(reader.wait_or(pending()));
        assert!(waiter.as_mut().poll(&mut cx).is_pending());

        assert!(guard.end(Outcome::Committed));
        assert_eq!(
            waiter.as_mut().poll(&mut cx),
            Poll::Ready(Some(Outcome::Committed))
        );
        assert_eq!(witness.count.load(Ordering::SeqCst), 1);
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

    /// An end wakes a task waiting in `wait_or` with a timer that never fires, and the task's
    /// next poll returns the end.
    #[test]
    fn an_end_wakes_an_async_waiter() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let (wakes, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let mut wait = pin!(reader.wait_or(pending()));
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        assert!(guard.end(Outcome::Committed));
        assert_eq!(
            wakes.0.load(Ordering::SeqCst),
            1,
            "the end did not wake the task"
        );
        assert_eq!(
            wait.as_mut().poll(&mut cx),
            Poll::Ready(Some(Outcome::Committed))
        );
    }

    /// A timer that fires first ends the wait with `None`; an end recorded before the final
    /// read, here by the timer itself as it fires, is returned.
    #[test]
    fn a_timer_first_returns_none_and_an_end_before_the_final_read() {
        let (_wakes, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let (reader, _guard) = terminal(|| Outcome::Unknown);
        let mut wait = pin!(reader.wait_or(ready_now(())));
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Ready(None));

        let (late_reader, late_guard) = terminal(|| Outcome::Unknown);
        let mut late = pin!(late_reader.wait_or(async move {
            late_guard.end(Outcome::Committed);
        }));
        assert_eq!(
            late.as_mut().poll(&mut cx),
            Poll::Ready(Some(Outcome::Committed))
        );
    }

    /// A guard dropped before any end wakes a task waiting in `wait_or`, with its fallback.
    #[test]
    fn a_drops_fallback_wakes_an_async_waiter() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let (wakes, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let mut wait = pin!(reader.wait_or(pending()));
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        drop(guard);
        assert_eq!(
            wakes.0.load(Ordering::SeqCst),
            1,
            "the drop did not wake the task"
        );
        assert_eq!(
            wait.as_mut().poll(&mut cx),
            Poll::Ready(Some(Outcome::Unknown))
        );
    }

    /// Waits polled once and then dropped take their wakers with them, so given-up waits don't
    /// pile up on a long operation's end; the end still wakes a live waiter afterwards.
    #[test]
    fn dropped_async_waits_leave_no_waker_and_a_live_one_still_wakes() {
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let (wakes, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..100 {
            let mut wait = pin!(reader.wait_or(pending()));
            assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        }
        assert_eq!(wakers_of(&reader), 0, "dropped waits left their wakers");
        let mut wait = pin!(reader.wait_or(pending()));
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wakers_of(&reader), 1);
        assert!(guard.end(Outcome::Committed));
        assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            wait.as_mut().poll(&mut cx),
            Poll::Ready(Some(Outcome::Committed))
        );
    }

    /// A waker is the runtime's code: one whose drop reads the terminal, replaced by a second
    /// poll with another waker, is dropped after the lock is released, so the read completes.
    /// The polls run on their own thread, with the guard, so a deadlock fails the test at its
    /// bound and nothing on the test's thread waits for the lock after it. The second poll
    /// itself must drop the replaced waker, not the wait's cleanup after it.
    #[test]
    fn a_replaced_wakers_drop_may_read_the_terminal() {
        struct Reads(Terminal<Outcome>, Arc<AtomicBool>);
        impl Wake for Reads {
            fn wake(self: Arc<Self>) {
                let _ = self.0.get();
            }
        }
        impl Drop for Reads {
            fn drop(&mut self) {
                let _ = self.0.get();
                self.1.store(true, Ordering::SeqCst);
            }
        }
        let (reader, guard) = terminal(|| Outcome::Unknown);
        let (polled, done) = mpsc::channel();
        let polls = thread::spawn(move || {
            let _guard = guard;
            let read_at_drop = Arc::new(AtomicBool::new(false));
            let reads = Reads(reader.clone(), Arc::clone(&read_at_drop));
            let reading = Waker::from(Arc::new(reads));
            let mut wait = pin!(reader.wait_or(pending()));
            let first = wait.as_mut().poll(&mut Context::from_waker(&reading));
            drop(reading); // The list now holds its last reference.
            let (_wakes, other) = counting_waker();
            let second = wait.as_mut().poll(&mut Context::from_waker(&other));
            assert!(
                read_at_drop.load(Ordering::SeqCst),
                "the second poll did not drop the replaced waker"
            );
            polled
                .send((first, second))
                .expect("the test waits for the polls");
        });
        let polls_ended = done.recv_timeout(Duration::from_secs(5));
        assert_eq!(
            polls_ended,
            Ok((Poll::Pending, Poll::Pending)),
            "the replaced waker's drop did not complete"
        );
        polls.join().expect("the polls do not panic");
    }
}
