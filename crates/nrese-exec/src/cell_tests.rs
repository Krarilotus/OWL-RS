//! The build cell's step: its table, its rows at the cell with fake builds (`BuildCell<u64,
//! u64>`, builders ending each way), and a waiter's own deadline. Spec: the module docs of
//! `cell.rs`: every build ends once, in one step that frees its key before any waiter hears
//! the end, counted by how it ended; a builder that names no end ends `Abandoned`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;

const PATIENCE: Duration = Duration::from_secs(30);

/// Holds the caller until `event`; past `PATIENCE` the test fails with `what`.
fn until(event: impl Fn() -> bool, what: &str) {
    let started = Instant::now();
    while !event() {
        assert!(started.elapsed() < PATIENCE, "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// What `waiters` threads, each joined to key 7's build under way with no deadline, are told
/// when `end` ends its builder, and whether the key was free when each woke. Each joins only
/// once it was answered the running build (`building.claim`), and `end` runs only once all
/// sleep on it.
fn told(
    cell: &Arc<BuildCell<u64, u64>>,
    waiters: usize,
    end: impl FnOnce(Builder<'_, u64, u64>),
) -> Vec<(Ended<u64>, bool)> {
    let Claim::Build(builder) = cell.claim(7, Kind::Opaque, || None) else {
        panic!("absent.claim builds");
    };
    let (tell, heard) = mpsc::channel();
    for _ in 0..waiters {
        let (cell, tell) = (Arc::clone(cell), tell.clone());
        std::thread::spawn(move || {
            let Claim::Wait(build) = cell.claim(7, Kind::Opaque, || None) else {
                panic!("building.claim joins");
            };
            let Joined::Told(ended) = build.join(None, &|| false) else {
                panic!("a waiter with no deadline or stop off the pool is told");
            };
            let _ = tell.send((ended, cell.is_idle()));
        });
    }
    until(
        || cell.waiting(&7) >= waiters,
        "the waiters never slept on the build",
    );
    end(builder);
    (0..waiters)
        .map(|_| heard.recv_timeout(PATIENCE).expect("a waiter was told"))
        .collect()
}

/// The one waiter's end, and whether the key was free at its wake.
fn one(mut woken: Vec<(Ended<u64>, bool)>) -> (Ended<u64>, bool) {
    assert_eq!(woken.len(), 1, "one waiter");
    woken.pop().expect("one waiter")
}

fn value(end: &Ended<u64>) -> Option<u64> {
    match end {
        Ended::End(End::Kept(value) | End::Unkept(Some(value))) => Some(**value),
        Ended::End(End::Empty | End::Later | End::Unkept(None)) | Ended::Abandoned => None,
    }
}

/// Every pair of a phase and an event: a row, or rejected. 2 phases × 7 events, 14 pairs,
/// each named as its row.
#[test]
fn every_pair_of_a_phase_and_an_event_is_a_row_or_rejected() {
    use Phase::{Absent, Building};
    let row = |next, answer, counts| {
        Ok(Step {
            next,
            answer,
            counts,
        })
    };
    let ended = |kind| row(Absent, None, Some(kind));
    let rejected = |phase, event| Err(Rejected { phase, event });
    let table = [
        (
            "absent.claim",
            Absent,
            Event::Claim,
            row(Building, Some(Answer::Build), None),
        ),
        (
            "absent.claim_found",
            Absent,
            Event::ClaimFound,
            row(Absent, Some(Answer::Told), None),
        ),
        (
            "building.claim",
            Building,
            Event::Claim,
            row(Building, Some(Answer::Join), None),
        ),
        (
            "building.end_kept",
            Building,
            Event::EndKept,
            ended(EndKind::Kept),
        ),
        (
            "building.end_empty",
            Building,
            Event::EndEmpty,
            ended(EndKind::Empty),
        ),
        (
            "building.end_later",
            Building,
            Event::EndLater,
            ended(EndKind::Later),
        ),
        (
            "building.end_unkept",
            Building,
            Event::EndUnkept,
            ended(EndKind::Unkept),
        ),
        (
            "building.abandoned",
            Building,
            Event::Abandoned,
            ended(EndKind::Abandoned),
        ),
        (
            "absent.end_kept",
            Absent,
            Event::EndKept,
            rejected(Absent, Event::EndKept),
        ),
        (
            "absent.end_empty",
            Absent,
            Event::EndEmpty,
            rejected(Absent, Event::EndEmpty),
        ),
        (
            "absent.end_later",
            Absent,
            Event::EndLater,
            rejected(Absent, Event::EndLater),
        ),
        (
            "absent.end_unkept",
            Absent,
            Event::EndUnkept,
            rejected(Absent, Event::EndUnkept),
        ),
        (
            "absent.abandoned",
            Absent,
            Event::Abandoned,
            rejected(Absent, Event::Abandoned),
        ),
        (
            "building.claim_found",
            Building,
            Event::ClaimFound,
            rejected(Building, Event::ClaimFound),
        ),
    ];
    for (name, phase, event, expected) in table {
        assert_eq!(step(phase, event), expected, "{name}");
    }
}

/// `building.end_kept` and `building.end_empty` at the cell: a joined waiter is told the
/// end its builder named, the key free when it wakes, the end counted, nothing rejected.
#[test]
fn a_build_s_named_end_is_told_to_its_waiter_and_counted() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let (kept, free) = one(told(&cell, 1, |builder| {
        builder.end(End::Kept(Arc::new(42)))
    }));
    assert_eq!(value(&kept), Some(42));
    assert!(matches!(kept, Ended::End(End::Kept(_))));
    assert!(free, "the key was free at the wake");
    let (empty, free) = one(told(&cell, 1, |builder| builder.end(End::Empty)));
    assert!(matches!(empty, Ended::End(End::Empty)));
    assert!(free, "the key was free at the wake");
    let counted = Ends {
        kept: 1,
        empty: 1,
        ..Ends::default()
    };
    assert_eq!(cell.ends(), counted);
    assert_eq!(cell.rejected(), 0);
}

/// `absent.claim_found`: after a build ended, a claim whose owner kept its value is told it
/// at once (a kept failure: `Empty`), and starts no build and counts no end.
#[test]
fn a_claim_whose_owner_kept_a_value_after_the_build_ended_is_told_it_and_builds_nothing() {
    let cell: BuildCell<u64, u64> = BuildCell::default();
    let Claim::Build(builder) = cell.claim(7, Kind::Opaque, || None) else {
        panic!("absent.claim builds");
    };
    builder.end(End::Kept(Arc::new(3)));
    let before = cell.ends();
    let Claim::Wait(build) = cell.claim(7, Kind::Opaque, || Some(Some(Arc::new(3)))) else {
        panic!("a kept value builds nothing");
    };
    assert!(
        matches!(build.join(None, &|| false), Joined::Told(Ended::End(End::Kept(v))) if *v == 3)
    );
    let Claim::Wait(build) = cell.claim(7, Kind::Opaque, || Some(None)) else {
        panic!("a kept failure builds nothing");
    };
    assert!(matches!(
        build.join(None, &|| false),
        Joined::Told(Ended::End(End::Empty))
    ));
    assert!(cell.is_idle());
    assert_eq!(cell.ends(), before, "no build ended");
    assert_eq!(cell.rejected(), 0);
}

/// `building.end_kept` through `finish`, statistics' way: a value held elsewhere (its old
/// sets) is what the waiters are told, at the builder's drop; the first end named is the end.
#[test]
fn a_finish_with_a_value_held_elsewhere_tells_the_waiters_that_value() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let old = Arc::new(5);
    let held = Arc::clone(&old);
    let (ended, free) = one(told(&cell, 1, move |builder| {
        builder.finish(Some(held));
        builder.finish(None);
        drop(builder);
    }));
    assert!(matches!(&ended, Ended::End(End::Kept(v)) if Arc::ptr_eq(v, &old)));
    assert!(free, "the key was free at the wake");
    assert_eq!(cell.ends().kept, 1);
    assert_eq!(cell.rejected(), 0);
}

/// Spec: a waiter that wakes to an end claims again and builds: the ended build is out of
/// the map before any waiter hears its end. Three waiters each find the key free at their
/// wake, and the next claim builds.
#[test]
fn a_waiter_woken_by_an_end_never_claims_the_ended_build() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let woken = told(&cell, 3, |builder| builder.end(End::Empty));
    assert_eq!(woken.len(), 3);
    assert!(
        woken.iter().all(|(_, free)| *free),
        "a waiter woke while the ended build was the key's entry"
    );
    assert!(matches!(
        cell.claim(7, Kind::Opaque, || None),
        Claim::Build(_)
    ));
}

/// Spec: a builder that panics names no end: its waiters are told `Abandoned`, the end is
/// counted, and the next claim builds again.
#[test]
fn a_builder_that_panics_is_counted_and_its_waiters_are_told_abandoned() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let (ended, free) = one(told(&cell, 1, |builder| {
        let panicked = std::panic::catch_unwind::<_, ()>(std::panic::AssertUnwindSafe(move || {
            let _builder = builder;
            panic!("the builder's injected panic");
        }));
        assert!(panicked.is_err());
    }));
    assert!(matches!(ended, Ended::Abandoned));
    assert!(free, "the key was free at the wake");
    assert_eq!(cell.ends().abandoned, 1);
    assert_eq!(cell.rejected(), 0);
    assert!(matches!(
        cell.claim(7, Kind::Opaque, || None),
        Claim::Build(_)
    ));
}

/// A spawn that fails as std's does: it drops the closure it was given, unrun.
fn failing_spawn(run: impl FnOnce() + Send) -> std::io::Result<()> {
    drop(run);
    Err(std::io::Error::other("the test's failed spawn"))
}

/// Spec: a builder moved into a spawn that fails is dropped unrun: it ends `Abandoned`, is
/// counted, and the next claim builds again.
#[test]
fn a_builder_moved_into_a_spawn_that_fails_ends_abandoned_and_is_counted() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let (ended, free) = one(told(&cell, 1, |builder| {
        assert!(failing_spawn(move || builder.end(End::Kept(Arc::new(1)))).is_err());
    }));
    assert!(matches!(ended, Ended::Abandoned));
    assert!(free, "the key was free at the wake");
    assert_eq!(cell.ends().abandoned, 1);
    assert!(matches!(
        cell.claim(7, Kind::Opaque, || None),
        Claim::Build(_)
    ));
}

/// Spec: a build that finds its key out of date (the bounds describe a later revision) tells
/// its own waiters `Later` and keeps nothing for the key: counted, and the next claim builds.
#[test]
fn a_build_that_ends_for_a_later_revision_tells_only_its_waiters_later_and_is_counted() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let (ended, _) = one(told(&cell, 1, |builder| builder.end(End::Later)));
    assert!(matches!(ended, Ended::End(End::Later)));
    assert_eq!(cell.ends().later, 1);
    assert!(matches!(
        cell.claim(7, Kind::Opaque, || None),
        Claim::Build(_)
    ));
}

/// Spec: a value its owner could not keep (over its budget) still goes to the build's
/// waiters, and the end is counted as unkept.
#[test]
fn a_value_that_cannot_be_kept_goes_to_the_waiters_and_is_counted() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let woken = told(&cell, 2, |builder| {
        builder.end(End::Unkept(Some(Arc::new(11))));
    });
    assert!(
        woken
            .iter()
            .all(|(ended, _)| matches!(ended, Ended::End(End::Unkept(Some(v))) if **v == 11))
    );
    assert_eq!(cell.ends().unkept, 1);
    assert_eq!(cell.rejected(), 0);
}

/// Spec: a waiter leaves at its own deadline (`Left`) while the build runs on; the build
/// ends later and is recorded and counted. Counted, not timed: the builder holds until the
/// waiter has left.
#[test]
fn a_waiter_with_a_deadline_leaves_at_it_while_the_build_runs_on() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let Claim::Build(builder) = cell.claim(7, Kind::Opaque, || None) else {
        panic!("absent.claim builds");
    };
    let (tell, heard) = mpsc::channel();
    let waiter = Arc::clone(&cell);
    std::thread::spawn(move || {
        let Claim::Wait(build) = waiter.claim(7, Kind::Opaque, || None) else {
            panic!("building.claim joins");
        };
        let deadline = Instant::now() + Duration::from_millis(100);
        let left = matches!(build.join(Some(deadline), &|| false), Joined::Left);
        let _ = tell.send((left, Instant::now() >= deadline, build));
    });
    let (left, at_deadline, build) = heard.recv_timeout(PATIENCE).expect("the waiter left");
    assert!(left, "the waiter was told an end it can't have had");
    assert!(at_deadline, "the waiter left before its deadline");
    assert!(!cell.is_idle(), "the build runs on");
    builder.end(End::Kept(Arc::new(9)));
    assert_eq!(build.wait(), Ok(Some(Arc::new(9))));
    assert_eq!(cell.ends().kept, 1);
}

/// Spec (`Build::join`): an end recorded by the time a waiter's stop has fired is told, not
/// left, and the stop is asked with no lock of the build's held. Here the stop itself ends
/// the build (the end lands between the waiter's first read of it and its stop), and the
/// waiter is told that end, for an opaque build and for a sliced one. Each case runs on a
/// thread of its own, its answer bounded: a stop asked under the build's `state` never
/// returns once it ends the build.
#[test]
fn a_waiter_whose_stop_fires_once_the_build_has_ended_is_told_the_end() {
    let case = |kind: Kind| {
        let (tell, heard) = mpsc::channel();
        std::thread::spawn(move || {
            let cell: BuildCell<u64, u64> = BuildCell::default();
            let Claim::Build(builder) = cell.claim(7, kind, || None) else {
                panic!("absent.claim builds");
            };
            let Claim::Wait(build) = cell.claim(7, kind, || None) else {
                panic!("building.claim joins");
            };
            let builder = Mutex::new(Some(builder));
            let stop = || {
                let taken = lock(&builder).take();
                if let Some(builder) = taken {
                    builder.end(End::Kept(Arc::new(5)));
                }
                true
            };
            let told = match build.join(None, &stop) {
                Joined::Told(end) => value(&end),
                Joined::Busy | Joined::Left => None,
            };
            let _ = tell.send(told);
        });
        (kind, heard.recv_timeout(PATIENCE).ok().flatten())
    };
    let told: Vec<_> = [Kind::Opaque, Kind::Sliced].into_iter().map(case).collect();
    assert_eq!(
        told,
        [(Kind::Opaque, Some(5)), (Kind::Sliced, Some(5))],
        "a waiter left (or hung) though the build had ended when its stop fired"
    );
}

/// Spec (`Build::join`): a waiter whose stop or deadline has fired leaves (`Left`), on a
/// pool thread too: it is not answered [`Busy`], which would send it to build the value
/// itself. Two cases on a one-thread pool, an opaque build held: the stop fired, and the
/// deadline passed.
#[test]
fn a_pool_waiter_whose_stop_or_deadline_fired_leaves_rather_than_builds() {
    let cell: BuildCell<u64, u64> = BuildCell::default();
    let Claim::Build(_held) = cell.claim(7, Kind::Opaque, || None) else {
        panic!("absent.claim builds");
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let answered = |deadline: Option<Instant>, stop: &(dyn Fn() -> bool + Sync)| {
        pool.install(|| match cell.claim(7, Kind::Opaque, || None) {
            Claim::Wait(build) => match build.join(deadline, stop) {
                Joined::Left => "left",
                Joined::Busy => "busy",
                Joined::Told(_) => "told",
            },
            Claim::Build(_) => panic!("one build at a time"),
        })
    };
    let (fired, unfired): (&(dyn Fn() -> bool + Sync), &(dyn Fn() -> bool + Sync)) =
        (&|| true, &|| false);
    let told = [
        ("stop fired", answered(None, fired)),
        ("deadline passed", answered(Some(Instant::now()), unfired)),
    ];
    assert_eq!(
        told,
        [("stop fired", "left"), ("deadline passed", "left")],
        "a pool waiter past its stop or deadline was not told to leave"
    );
}

/// An opaque build in progress: a pool thread is turned away at once, a thread off the
/// pool sleeps until the builder finishes, and a builder that ends without a value
/// wakes it with none. What's kept is charged until it's dropped.
#[test]
fn only_threads_off_the_pool_wait_for_an_opaque_build() {
    let cell: Arc<BuildCell<u64, u64>> = Arc::default();
    let Claim::Build(builder) = cell.claim(7, Kind::Opaque, || None) else {
        panic!("the first claim builds");
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let on_pool = pool.install(|| match cell.claim(7, Kind::Opaque, || None) {
        Claim::Wait(build) => build.wait().map(|_| ()),
        Claim::Build(_) => panic!("one build at a time"),
    });
    assert_eq!(on_pool, Err(Busy));
    let (got, woke) = std::sync::mpsc::channel();
    let waiter = Arc::clone(&cell);
    std::thread::spawn(move || {
        let Claim::Wait(build) = waiter.claim(7, Kind::Opaque, || None) else {
            panic!("one build at a time");
        };
        let _ = got.send(build.wait());
    });
    let started = std::time::Instant::now();
    while lock(&cell.building)[&7].waiting() == 0 {
        assert!(started.elapsed().as_secs() < 30, "the waiter never slept");
        std::thread::yield_now();
    }
    builder.finish(Some(Arc::new(42)));
    drop(builder);
    let value = woke.recv_timeout(std::time::Duration::from_secs(30));
    assert_eq!(value.unwrap().unwrap().as_deref(), Some(&42));
    assert!(cell.is_idle());
    let budget = Arc::new(Budget::new(100));
    let kept = Kept::charge(Arc::new(1u64), 60, Arc::clone(&budget)).unwrap();
    assert_eq!(budget.used(), 60);
    assert!(Kept::charge(Arc::new(2u64), 60, Arc::clone(&budget)).is_err());
    drop(kept);
    assert_eq!(budget.used(), 0);
}
