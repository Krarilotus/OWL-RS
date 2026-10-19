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
    let cases: [(&str, &dyn Fn() -> usize, Counted); 14] = [
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
