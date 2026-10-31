use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

use super::*;
use crate::{ChargedVec, Reservation};

const BIG: usize = 1 << 21;

/// What a case of the table must count.
#[derive(Debug, Clone, Copy)]
enum Counted {
    Nothing,
    AtLeast(usize),
}

/// The most a perfectly charged operation is measured beyond its charges over 50
/// measurements: vectors charged before they grow and released after they are freed, on 8
/// threads started before, whose events interleave between any two of each other's.
fn charged_on_threads() -> usize {
    static GO: AtomicUsize = AtomicUsize::new(0);
    static DONE: AtomicUsize = AtomicUsize::new(0);
    static STOP: AtomicBool = AtomicBool::new(false);
    static BUDGET: std::sync::OnceLock<Budget> = std::sync::OnceLock::new();
    let budget = BUDGET.get_or_init(Budget::unlimited);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(move || {
                let mut round = 0;
                while !STOP.load(SeqCst) {
                    if GO.load(SeqCst) == round {
                        spin_loop();
                        continue;
                    }
                    round += 1;
                    for i in 0..2_000usize {
                        let mut v: ChargedVec<u64, &Budget> = ChargedVec::new(budget);
                        for j in 0..(i % 300) {
                            v.push(j as u64).unwrap();
                        }
                    }
                    DONE.fetch_add(1, SeqCst);
                }
            })
        })
        .collect();
    let mut worst = 0;
    for round in 1..=50 {
        let ((), excess) = measure_within(budget, || {
            GO.store(round, SeqCst);
            while DONE.load(SeqCst) < round * 8 {
                spin_loop();
            }
        });
        worst = worst.max(excess);
    }
    STOP.store(true, SeqCst);
    GO.store(usize::MAX, SeqCst);
    threads.into_iter().for_each(|t| t.join().unwrap());
    worst
}

/// [`measure_within`] without its preparation ([`prepare`]): the counting alone, cheap
/// enough to repeat 100,000 times.
fn counted_within<T>(budget: &Budget, work: impl FnOnce() -> T) -> usize {
    let running = Running::start(Some(budget));
    black_box(work());
    reported(running.end().excess)
}

/// A pool thread's first wait, in parking_lot (which makes its waiter table then).
fn parks_on_the_pool() -> usize {
    let park = || {
        // SAFETY: parks nowhere: its validation fails.
        unsafe {
            parking_lot_core::park(
                0x10,
                || false,
                || {},
                |_, _| {},
                parking_lot_core::DEFAULT_PARK_TOKEN,
                None,
            );
        }
    };
    measure_within(&Budget::unlimited(), || rayon::join(park, park)).1
}

/// The most of 3,000 measurements of an empty parallel loop entered from outside the pool,
/// half of them within another measurement.
fn enters_the_pool() -> usize {
    let entered = || measure_within(&Budget::unlimited(), || rayon::join(|| (), || ())).1;
    let top = (0..1_500).map(|_| entered()).max().unwrap();
    let (nested, _) = measure(|| (0..1_500).map(|_| entered()).max().unwrap());
    top.max(nested)
}

/// The least of 100,000 measurements of an uncharged kilobyte each, while another thread
/// allocates and frees without pause: a block made as a measurement starts is counted at
/// its allocation and its free, or at neither.
fn at_the_start() -> usize {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(SeqCst) {
                drop(black_box(Vec::<u8>::with_capacity(BIG)));
            }
        });
        let budget = Budget::unlimited();
        let least = (0..100_000)
            .map(|_| counted_within(&budget, || black_box(Vec::<u8>::with_capacity(1024))))
            .min();
        stop.store(true, SeqCst);
        least.unwrap()
    })
}

/// Every finding of the reviews of this guard, a case each: what is allocated and not
/// charged counts wherever it is made, whatever was there before, and what was charged
/// first counts for nothing, on one thread or many.
#[test]
fn a_measurement_counts_what_was_made_while_it_ran() {
    if !alone() {
        return;
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    // Its threads started, not starting while a case runs.
    pool.broadcast(|_| ());
    let uncharged = || black_box(vec![1u8; BIG]).len();
    let cases: [(&str, &dyn Fn() -> usize, Counted); 15] = [
        (
            "mapped outside the allocator, uncharged",
            &|| {
                let budget = Budget::unlimited();
                measure_within(&budget, || {
                    // A region made, its pages committed without a charge, then unmapped.
                    let region = tag();
                    mapped(BIG, region);
                    unmapped(BIG, region);
                })
                .1
            },
            Counted::AtLeast(BIG),
        ),
        (
            "a pool thread's first wait",
            &parks_on_the_pool,
            Counted::Nothing,
        ),
        (
            "entering the global pool from outside, nested or not",
            &enters_the_pool,
            Counted::Nothing,
        ),
        (
            "a block made as a measurement starts",
            &at_the_start,
            Counted::AtLeast(1024),
        ),
        (
            "after freeing a block from before",
            &|| {
                let earlier = black_box(vec![1u8; 2 * BIG]);
                measure_within(&Budget::unlimited(), move || {
                    drop(earlier);
                    uncharged()
                })
                .1
            },
            Counted::AtLeast(BIG),
        ),
        (
            "beside charges from before",
            &|| {
                let budget = Budget::unlimited();
                budget.charge(2 * BIG).unwrap();
                let mut held = Reservation::new(&budget);
                held.charge(2 * BIG).unwrap();
                measure_within(&budget, uncharged).1
            },
            Counted::AtLeast(BIG),
        ),
        (
            "freeing a charged vector from before",
            &|| {
                let budget = Budget::unlimited();
                let earlier = ChargedVec::<u8, &Budget>::with_capacity(&budget, BIG).unwrap();
                measure_within(&budget, || drop(earlier)).1
            },
            Counted::Nothing,
        ),
        (
            "beside a charge of every byte",
            &|| {
                let budget = Budget::unlimited();
                measure_within(&budget, || {
                    budget.charge(usize::MAX).unwrap();
                    uncharged()
                })
                .1
            },
            Counted::Nothing,
        ),
        (
            "charged first on 8 threads",
            &charged_on_threads,
            Counted::Nothing,
        ),
        (
            "on a thread it starts",
            &|| {
                measure_within(&Budget::unlimited(), || {
                    std::thread::spawn(uncharged).join().unwrap()
                })
                .1
            },
            Counted::AtLeast(BIG),
        ),
        (
            "on a pool made before",
            &|| measure_within(&Budget::unlimited(), || pool.install(uncharged)).1,
            Counted::AtLeast(BIG),
        ),
        (
            "while a realloc moves the bytes",
            &|| {
                let budget = Budget::unlimited();
                measure_within(&budget, || {
                    budget.charge(2 * BIG).unwrap();
                    let mut grown = black_box(Vec::<u8>::with_capacity(BIG));
                    grown.reserve_exact(2 * BIG);
                    grown.capacity()
                })
                .1
            },
            Counted::AtLeast(BIG),
        ),
        (
            "released while it lives",
            &|| {
                let budget = Budget::unlimited();
                let (kept, excess) = measure_within(&budget, || {
                    let mut held = Reservation::new(&budget);
                    held.charge(BIG).unwrap();
                    let kept = black_box(vec![1u8; BIG]);
                    drop(held);
                    kept
                });
                excess.min(kept.len())
            },
            Counted::AtLeast(BIG),
        ),
        (
            "in a measurement within",
            &|| {
                let (inner, excess) = measure_within(&Budget::unlimited(), || measure(uncharged).1);
                inner.min(excess)
            },
            Counted::AtLeast(BIG),
        ),
        (
            "after one that unwound out of a pool",
            &|| {
                let budget = Budget::unlimited();
                let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    measure_within(&budget, || pool.install(|| panic!("unwinds")))
                }));
                assert!(unwound.is_err());
                let running = RUNNING.iter().filter(|s| s.taken.load(SeqCst));
                running.count() + ACTIVE.load(SeqCst)
            },
            Counted::Nothing,
        ),
    ];
    let mut failures = Vec::new();
    for (case, run, expected) in cases {
        let counted = run();
        let right = match expected {
            Counted::Nothing => counted == 0,
            Counted::AtLeast(least) => counted >= least,
        };
        if !right {
            failures.push(format!("{case}: {counted}, expected {expected:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// What the one running measurement counts as live now.
fn live_now() -> i128 {
    RUNNING
        .iter()
        .find(|slot| slot.since.load(SeqCst) != 0)
        .map_or(0, |slot| slot.with(|_, c| c.live))
}

/// The global pool's threads have started before a measurement on another pool opens:
/// one starting in it would be counted (its crossbeam-epoch handle, 2,304 bytes, made as
/// it first looks for work). They are held at their start, after the pool is built (which
/// waits for each thread's queue) and before that first look, until the measurement has
/// opened, or for a second if the measurement waits for them first. The measurement then
/// waits for their handles to be counted, for at most 2 seconds: where they were made
/// before it, as they must be, it waits all of that.
#[test]
fn the_global_pool_has_started_before_a_measurement_on_another() {
    use std::time::{Duration, Instant};
    static OPEN: AtomicBool = AtomicBool::new(false);
    static STARTED: AtomicUsize = AtomicUsize::new(0);
    const THREADS: usize = 4;
    const HANDLE: i128 = 2_304;
    if !alone() {
        return;
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(THREADS)
        .start_handler(|_| {
            while !OPEN.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            STARTED.fetch_add(1, SeqCst);
        })
        .build_global()
        .unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    pool.broadcast(|_| ());
    let opener = std::thread::spawn(|| {
        let since = Instant::now();
        while ACTIVE.load(SeqCst) == 0 && since.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(1));
        }
        OPEN.store(true, SeqCst);
    });
    let ((), counted) = pool.install(|| {
        measure(|| {
            while STARTED.load(SeqCst) < THREADS {
                spin_loop();
            }
            let since = Instant::now();
            while live_now() < THREADS as i128 * HANDLE && since.elapsed() < Duration::from_secs(2)
            {
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    });
    opener.join().unwrap();
    assert_eq!(counted, 0, "global threads started in the measurement");
}

/// A first measurement made on a thread of the global pool returns: preparing it waits for
/// a broadcast to every thread of that pool, its own included (Sol, measure-pool r1: with
/// one thread, it waited for ever).
#[test]
fn a_first_measurement_on_a_global_thread_returns() {
    use std::time::Duration;
    if !alone() {
        return;
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    rayon::spawn(move || sent.send(measure(|| ()).1).unwrap());
    assert_eq!(received.recv_timeout(Duration::from_secs(10)), Ok(0));
}

/// Preparing a first measurement on a thread of the global pool runs none of the jobs that
/// wait there (Sol, measure-pool r2): not one that measures too, which would enter the
/// preparation still under way, nor one that takes a lock the measuring thread holds. Both
/// measurements return, and the lock is taken after it is let go.
#[test]
fn preparing_a_measurement_runs_no_job_waiting_on_its_thread() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    if !alone() {
        return;
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let held = Arc::new(Mutex::new(()));
    rayon::spawn(move || {
        let inner = sent.clone();
        rayon::spawn(move || {
            inner
                .send(("queued measurement", measure(|| ()).1))
                .unwrap()
        });
        let lock = held.lock().unwrap();
        let waiting = Arc::clone(&held);
        let locked = sent.clone();
        rayon::spawn(move || {
            drop(waiting.lock().unwrap());
            locked.send(("queued lock", 0)).unwrap();
        });
        let measured = measure(|| ()).1;
        drop(lock);
        sent.send(("measurement", measured)).unwrap();
    });
    let mut done: Vec<_> = (0..3)
        .map(|_| received.recv_timeout(Duration::from_secs(20)))
        .collect();
    done.sort_by_key(|r| format!("{r:?}"));
    assert_eq!(
        done,
        [
            Ok(("measurement", 0)),
            Ok(("queued lock", 0)),
            Ok(("queued measurement", 0)),
        ]
    );
}

/// A measured test runs alone under cargo test too when it is ignored, or should panic.
#[test]
#[ignore = "runs with --ignored"]
fn an_ignored_measurement_runs_alone() {
    if !alone() {
        return;
    }
    assert!(measure(|| black_box(vec![1u8; BIG]).len()).1 >= BIG);
}

#[test]
#[should_panic(expected = "measured and panicked")]
fn a_measurement_that_should_panic_panics_alone() {
    if !alone() {
        return;
    }
    measure(|| panic!("measured and panicked"));
}

/// How long [`hangs_alone`] sleeps in its child: far past the deadline it runs under.
const HANG_SECS: u64 = 30;

/// Run by [`a_child_past_its_deadline_is_stopped`] under a deadline of one second.
#[test]
#[ignore = "run by a_child_past_its_deadline_is_stopped"]
fn hangs_alone() {
    if !alone() {
        return;
    }
    std::thread::sleep(std::time::Duration::from_secs(HANG_SECS));
}

/// Spec: a call to another process has a deadline and kills its child when it expires
/// (project rule; tests audit E14, A-904): a test whose child runs past [`alone`]'s
/// deadline fails at the deadline, naming it, and its child is gone.
#[test]
fn a_child_past_its_deadline_is_stopped() {
    let started = std::time::Instant::now();
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "measure::tests::hangs_alone",
            "--exact",
            "--include-ignored",
        ])
        .args(["--test-threads=1", "--nocapture"])
        .env("NRESE_MEASURE_ALONE_SECS", "1")
        .env_remove("NRESE_MEASURE_ALONE")
        .output()
        .expect("the test binary runs");
    let elapsed = started.elapsed();
    let printed = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the child ran its {HANG_SECS} s to the end, in {elapsed:?}: {printed}"
    );
    assert!(
        elapsed.as_secs() < HANG_SECS / 2,
        "stopped after {elapsed:?}, not at its deadline"
    );
    let message = printed
        .split_once("measure::tests::hangs_alone, run alone (process ")
        .unwrap_or_else(|| panic!("no deadline named: {printed}"))
        .1;
    let (process, rest) = message
        .split_once(')')
        .expect("the process, then the deadline");
    assert!(
        rest.starts_with(", passed its deadline of 1 s"),
        "{printed}"
    );
    assert!(process.parse::<u32>().is_ok(), "{printed}");
    // Linux: a process that runs has its directory; a stopped one, waited for, has none.
    #[cfg(target_os = "linux")]
    assert!(
        !std::path::Path::new(&format!("/proc/{process}")).exists(),
        "process {process} still runs"
    );
}

/// Spec: no hard-coded limits, every budget a setting with an unlimited value and a
/// documented default (owner's design principles): unset, ten minutes; `0` and
/// `unlimited`, none; whole seconds as given.
#[test]
fn the_deadline_is_whole_seconds_or_none() {
    use std::time::Duration;
    let cases = [
        (None, Some(Duration::from_secs(600))),
        (Some("0"), None),
        (Some("unlimited"), None),
        (Some(" Unlimited "), None),
        (Some("1"), Some(Duration::from_secs(1))),
        (
            Some("18446744073709551615"),
            Some(Duration::from_secs(u64::MAX)),
        ),
    ];
    for (value, deadline) in cases {
        assert_eq!(alone_deadline(value), deadline, "{value:?}");
    }
    for refused in ["", "-1", "1.5", "10s", "18446744073709551616"] {
        let refusal = std::panic::catch_unwind(|| alone_deadline(Some(refused)));
        assert!(refusal.is_err(), "{refused:?} taken");
    }
}
