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

// The explored model: one key and `N` claimers, every schedule of their moves to a depth,
// each move one atomic section of the cell (a claim, a builder's finish, end or drop, a
// waiter's deadline or look), run against the real `step` and `event_of`. Every new world is
// checked (`check`), then drained by the server's moves alone (a live builder dropped, then
// each waiter looks) until no builder lives and no claimer waits. What it covers, and what
// the cell's own tests cover instead: the doc of the test at the end.

use crate::lifecycle_explorer::{Model, Stats, explore};

/// A value given as a build's: the build it belongs to (renumbered with the builds; `GONE`
/// once that build is forgotten), the claimer that named it, and which naming of its build
/// it was (0 the first, 1 any later), so an end delivered from the wrong build, named by
/// another, or replaced by a later naming, is told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Value {
    build: u8,
    by: u8,
    seq: u8,
}

const GONE: u8 = u8::MAX;

/// An end: as named (never `Abandoned`), as recorded, and as told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Heard {
    Kept(Value),
    Empty,
    Later,
    Unkept(Value),
    Abandoned,
}

impl Heard {
    /// Its counter, in `counter`'s order.
    fn counter(self) -> usize {
        match self {
            Self::Kept(_) => 0,
            Self::Empty => 1,
            Self::Later => 2,
            Self::Unkept(_) => 3,
            Self::Abandoned => 4,
        }
    }

    /// The real end a builder names, for the real `event_of` (`None`: none named).
    fn real(self) -> Option<End<()>> {
        match self {
            Self::Kept(_) => Some(End::Kept(Arc::new(()))),
            Self::Empty => Some(End::Empty),
            Self::Later => Some(End::Later),
            Self::Unkept(_) => Some(End::Unkept(Some(Arc::new(())))),
            Self::Abandoned => None,
        }
    }

    fn value(self) -> Option<Value> {
        match self {
            Self::Kept(value) | Self::Unkept(value) => Some(value),
            Self::Empty | Self::Later | Self::Abandoned => None,
        }
    }

    /// The same end, its value's build renumbered by `new`.
    fn renumbered(self, new: impl Fn(u8) -> u8) -> Self {
        let moved = |value: Value| Value {
            build: new(value.build),
            ..value
        };
        match self {
            Self::Kept(value) => Self::Kept(moved(value)),
            Self::Unkept(value) => Self::Unkept(moved(value)),
            Self::Empty | Self::Later | Self::Abandoned => self,
        }
    }
}

/// The counter `step` adds an end to, in `Heard::counter`'s order.
fn counter(kind: EndKind) -> usize {
    match kind {
        EndKind::Kept => 0,
        EndKind::Empty => 1,
        EndKind::Later => 2,
        EndKind::Unkept => 3,
        EndKind::Abandoned => 4,
    }
}

/// One claimer: a build named by its index in the world's `builds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Claimer {
    Idle,
    /// A builder; whether it named its end by the consuming `Builder::end` (then its only
    /// next move is its drop).
    Builder(u8, bool),
    /// Joined to a build; whether its deadline has passed.
    Waiting(u8, bool),
    Told(u8, Heard),
    Left(u8),
}

impl Claimer {
    fn build(self) -> Option<u8> {
        match self {
            Self::Idle => None,
            Self::Builder(b, _) | Self::Waiting(b, _) | Self::Told(b, _) | Self::Left(b) => Some(b),
        }
    }
}

/// One build: the first end its builder asked for (the record check 4 reads, kept apart
/// from `pending`), the end pending at its drop, its terminal, whether it was dropped and
/// `step` counted it, and whether it is a claim told at once (no builder, never counted).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ModelBuild {
    asked: Option<Heard>,
    pending: Option<Heard>,
    terminal: Option<Heard>,
    dropped: bool,
    counted: bool,
    at_once: bool,
}

impl ModelBuild {
    fn running() -> Self {
        Self {
            asked: None,
            pending: None,
            terminal: None,
            dropped: false,
            counted: false,
            at_once: false,
        }
    }

    /// A claim told `heard` at once, where `expected` is what its lookup had.
    fn told(expected: Heard, heard: Heard) -> Self {
        Self {
            asked: Some(expected),
            terminal: Some(heard),
            at_once: true,
            ..Self::running()
        }
    }
}

/// A check a move broke, judged against the world before the move (so before renumbering
/// forgets a build): kept in the world, reported by `check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Fault {
    /// 8: a claim's `step.next` is not the phase its answer leaves.
    Next,
    /// 9: a drop recorded no end.
    NoEnd,
    /// 10: a look's outcome is not what the end held and the deadline before it say.
    Look,
    /// 11: a drop recorded an end other than the first its builder named.
    Recorded,
    /// 12: a claim's outcome is not what the entry before it and its lookup say.
    Claim,
}

/// The key, its builds still named by someone (renumbered in order of mention, so worlds
/// that differ only in old builds' ids are one), the counters, and the claimers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CellWorld<const N: usize> {
    /// The key's entry: the build under way, as `step` leaves it.
    entry: Option<u8>,
    builds: Vec<ModelBuild>,
    /// The counters, as `step` counts: kept, empty, later, unkept, abandoned.
    ends: [u8; 5],
    /// The model's own: each build dropped, by the end its terminal holds after the drop.
    dropped: [u8; 5],
    rejected: u8,
    /// The first check a move broke.
    fault: Option<Fault>,
    claimers: [Claimer; N],
}

impl<const N: usize> CellWorld<N> {
    fn build(&self, b: u8) -> &ModelBuild {
        &self.builds[usize::from(b)]
    }

    fn build_mut(&mut self, b: u8) -> &mut ModelBuild {
        &mut self.builds[usize::from(b)]
    }

    /// The index the next added build takes.
    fn next_build(&self) -> u8 {
        u8::try_from(self.builds.len()).expect("a few builds")
    }

    fn add(&mut self, build: ModelBuild) -> u8 {
        let b = self.next_build();
        self.builds.push(build);
        b
    }

    fn broke(&mut self, fault: Fault) {
        self.fault.get_or_insert(fault);
    }

    /// The value `c` names for build `b`: its first naming, or a later one (a build not yet
    /// added has none before).
    fn value(&self, b: u8, c: usize) -> Value {
        let named = self
            .builds
            .get(usize::from(b))
            .is_some_and(|build| build.asked.is_some());
        Value {
            build: b,
            by: u8::try_from(c).expect("a few claimers"),
            seq: u8::from(named),
        }
    }

    /// A builder names `named`: the record of the first end asked, and the cell's pending
    /// end (`Builder::name`), each first-wins.
    fn name(&mut self, b: u8, named: Heard) {
        let build = self.build_mut(b);
        build.asked.get_or_insert(named);
        build.pending = build.pending.or(Some(named));
    }

    /// Forgets builds no one names, and numbers the rest in order of mention.
    fn renumber(&mut self) {
        let mut order: Vec<u8> = Vec::new();
        for b in self
            .entry
            .into_iter()
            .chain(self.claimers.iter().filter_map(|c| c.build()))
        {
            if !order.contains(&b) {
                order.push(b);
            }
        }
        let new = |b: u8| {
            order
                .iter()
                .position(|&o| o == b)
                .map_or(GONE, |at| u8::try_from(at).expect("a few builds"))
        };
        let moved = |heard: Option<Heard>| heard.map(|heard| heard.renumbered(new));
        self.builds = order
            .iter()
            .map(|&b| {
                let build = self.build(b);
                ModelBuild {
                    asked: moved(build.asked),
                    pending: moved(build.pending),
                    terminal: moved(build.terminal),
                    ..build.clone()
                }
            })
            .collect();
        self.entry = self.entry.map(new);
        for c in &mut self.claimers {
            *c = match *c {
                Claimer::Idle => Claimer::Idle,
                Claimer::Builder(b, consumed) => Claimer::Builder(new(b), consumed),
                Claimer::Waiting(b, passed) => Claimer::Waiting(new(b), passed),
                Claimer::Told(b, heard) => Claimer::Told(new(b), heard.renumbered(new)),
                Claimer::Left(b) => Claimer::Left(new(b)),
            };
        }
    }
}

/// What the owner's `kept` finds for a claim where no build is under way: nothing, a value
/// kept, or a failure kept (`Some(None)`). Free, on a fresh cell too.
#[derive(Debug, Clone, Copy)]
enum Lookup {
    Nothing,
    Value,
    Failure,
}

/// An end named by the consuming `Builder::end`.
#[derive(Debug, Clone, Copy)]
enum Ending {
    Kept,
    Empty,
    Later,
    Unkept,
}

#[derive(Debug, Clone, Copy)]
enum CellMove {
    Claim(usize, Lookup),
    /// `Builder::finish`: a value (`Kept`) or none (`Empty`); first named wins, and the
    /// builder goes on.
    Finish(usize, bool),
    /// `Builder::end`: names the end (first named wins) and consumes the builder.
    End(usize, Ending),
    /// The builder's drop: the one transition.
    Drop(usize),
    Deadline(usize),
    Look(usize),
}

impl CellMove {
    fn claimer(self) -> usize {
        match self {
            Self::Claim(c, _)
            | Self::Finish(c, _)
            | Self::End(c, _)
            | Self::Drop(c)
            | Self::Deadline(c)
            | Self::Look(c) => c,
        }
    }
}

struct CellModel<const N: usize>;

impl<const N: usize> Model for CellModel<N> {
    type World = CellWorld<N>;
    type Move = CellMove;
    const DRAIN_BOUND: usize = 2 * N;

    fn start() -> CellWorld<N> {
        CellWorld {
            entry: None,
            builds: Vec::new(),
            ends: [0; 5],
            dropped: [0; 5],
            rejected: 0,
            fault: None,
            claimers: [Claimer::Idle; N],
        }
    }

    fn moves(world: &CellWorld<N>) -> Vec<CellMove> {
        let mut moves = Vec::new();
        for (c, claimer) in world.claimers.iter().enumerate() {
            match *claimer {
                Claimer::Idle | Claimer::Told(..) => match world.entry {
                    // `kept` is asked only where no build is under way.
                    None => {
                        for lookup in [Lookup::Nothing, Lookup::Value, Lookup::Failure] {
                            moves.push(CellMove::Claim(c, lookup));
                        }
                    }
                    Some(_) => moves.push(CellMove::Claim(c, Lookup::Nothing)),
                },
                Claimer::Builder(_, consumed) => {
                    if !consumed {
                        moves.push(CellMove::Finish(c, true));
                        moves.push(CellMove::Finish(c, false));
                        for ending in [Ending::Kept, Ending::Empty, Ending::Later, Ending::Unkept] {
                            moves.push(CellMove::End(c, ending));
                        }
                    }
                    moves.push(CellMove::Drop(c));
                }
                Claimer::Waiting(_, passed) => {
                    if !passed {
                        moves.push(CellMove::Deadline(c));
                    }
                    // Every waiter may look; a look that changes nothing is the same world.
                    moves.push(CellMove::Look(c));
                }
                Claimer::Left(_) => {}
            }
        }
        moves
    }

    fn apply(world: &CellWorld<N>, action: &CellMove) -> CellWorld<N> {
        let mut w = world.clone();
        match *action {
            CellMove::Claim(c, lookup) => {
                let before = w.entry;
                // What `claim` makes of its owner's `kept`: a value kept is `Kept`, a failure
                // kept `Empty` (a claim told at once is the build `next_build` names).
                let found = match (w.entry, lookup) {
                    (Some(_), _) | (None, Lookup::Nothing) => None,
                    (None, Lookup::Value) => Some(Heard::Kept(w.value(w.next_build(), c))),
                    (None, Lookup::Failure) => Some(Heard::Empty),
                };
                let phase = match w.entry {
                    Some(_) => Phase::Building,
                    None => Phase::Absent,
                };
                let event = match found {
                    Some(_) => Event::ClaimFound,
                    None => Event::Claim,
                };
                let step = step(phase, event).ok();
                // The phase a claim's answer leaves: a build under way, or as it was.
                let leaves = match step.and_then(|step| step.answer) {
                    Some(Answer::Build | Answer::Join) => Some(Phase::Building),
                    Some(Answer::Told) => Some(phase),
                    None => None,
                };
                if let (Some(step), Some(leaves)) = (step, leaves)
                    && step.next != leaves
                {
                    w.broke(Fault::Next);
                }
                w.claimers[c] = match (step.and_then(|step| step.answer), w.entry, found) {
                    (Some(Answer::Build), _, _) => {
                        let b = w.add(ModelBuild::running());
                        w.entry = Some(b);
                        Claimer::Builder(b, false)
                    }
                    (Some(Answer::Join), Some(b), _) => Claimer::Waiting(b, false),
                    (Some(Answer::Told), _, Some(heard)) => {
                        // What the owner had, read from the lookup by a mapping of its own,
                        // for check 4.
                        let had = match lookup {
                            Lookup::Value => Heard::Kept(w.value(w.next_build(), c)),
                            Lookup::Failure => Heard::Empty,
                            Lookup::Nothing => Heard::Abandoned,
                        };
                        Claimer::Told(w.add(ModelBuild::told(had, heard)), heard)
                    }
                    // Rejected (or an answer with nothing to give): counted, and told
                    // `Abandoned` by a build of its own, as `claim` does.
                    (Some(Answer::Join), None, _)
                    | (Some(Answer::Told), _, None)
                    | (None, _, _) => {
                        w.rejected += 1;
                        let abandoned = ModelBuild::told(Heard::Abandoned, Heard::Abandoned);
                        Claimer::Told(w.add(abandoned), Heard::Abandoned)
                    }
                };
                // 12. Judged by the entry before the claim and its lookup: an entry is joined;
                // with none, a lookup that found something is told at once (its end is check
                // 4's), and one that found nothing builds.
                let required = match (before, lookup) {
                    (Some(e), _) => {
                        w.claimers[c] == Claimer::Waiting(e, false) && w.entry == before
                    }
                    (None, Lookup::Value | Lookup::Failure) => {
                        matches!(w.claimers[c], Claimer::Told(b, _) if w.build(b).at_once)
                            && w.entry.is_none()
                    }
                    (None, Lookup::Nothing) => {
                        matches!(w.claimers[c], Claimer::Builder(b, false) if w.entry == Some(b))
                    }
                };
                if !required {
                    w.broke(Fault::Claim);
                }
            }
            CellMove::Finish(c, with_value) => {
                if let Claimer::Builder(b, false) = w.claimers[c] {
                    let named = match with_value {
                        true => Heard::Kept(w.value(b, c)),
                        false => Heard::Empty,
                    };
                    w.name(b, named);
                }
            }
            CellMove::End(c, ending) => {
                if let Claimer::Builder(b, false) = w.claimers[c] {
                    let value = w.value(b, c);
                    let named = match ending {
                        Ending::Kept => Heard::Kept(value),
                        Ending::Empty => Heard::Empty,
                        Ending::Later => Heard::Later,
                        Ending::Unkept => Heard::Unkept(value),
                    };
                    w.name(b, named);
                    w.claimers[c] = Claimer::Builder(b, true);
                }
            }
            CellMove::Drop(c) => {
                if let Claimer::Builder(b, _) = w.claimers[c] {
                    // `Builder::drop`, in one section: the step, the entry, the count, the
                    // end recorded, the waiters woken (they hear it at their next look).
                    let phase = match w.entry == Some(b) {
                        true => Phase::Building,
                        false => Phase::Absent,
                    };
                    let pending = w.build(b).pending;
                    let event = event_of(pending.and_then(Heard::real).as_ref());
                    match step(phase, event) {
                        Ok(step) => {
                            if step.next == Phase::Absent {
                                w.entry = None;
                            }
                            if let Some(kind) = step.counts {
                                w.ends[counter(kind)] += 1;
                                w.build_mut(b).counted = true;
                            }
                        }
                        Err(_) => w.rejected += 1,
                    }
                    let recorded = pending.unwrap_or(Heard::Abandoned);
                    let build = w.build_mut(b);
                    build.terminal.get_or_insert(recorded);
                    build.dropped = true;
                    // Before renumbering can forget the build: the end its terminal holds,
                    // tallied for check 2 and judged by check 11 against the first end its
                    // builder named (else `Abandoned`); or none held (check 9).
                    match w.build(b).terminal {
                        Some(held) => {
                            w.dropped[held.counter()] += 1;
                            if held != w.build(b).asked.unwrap_or(Heard::Abandoned) {
                                w.broke(Fault::Recorded);
                            }
                        }
                        None => w.broke(Fault::NoEnd),
                    }
                    w.claimers[c] = Claimer::Idle;
                }
            }
            CellMove::Deadline(c) => {
                if let Claimer::Waiting(b, false) = w.claimers[c] {
                    w.claimers[c] = Claimer::Waiting(b, true);
                }
            }
            CellMove::Look(c) => {
                if let Claimer::Waiting(b, passed) = w.claimers[c] {
                    let held = w.build(b).terminal;
                    // `join`: an end recorded is told before a passed deadline is left.
                    let looked = match (held, passed) {
                        (Some(heard), _) => Claimer::Told(b, heard),
                        (None, true) => Claimer::Left(b),
                        (None, false) => Claimer::Waiting(b, false),
                    };
                    // 10. Judged by the world before the look: told the end held, if any;
                    // else left only past its deadline.
                    let expected = if let Some(heard) = held {
                        Claimer::Told(b, heard)
                    } else if passed {
                        Claimer::Left(b)
                    } else {
                        Claimer::Waiting(b, false)
                    };
                    if looked != expected {
                        w.broke(Fault::Look);
                    }
                    w.claimers[c] = looked;
                }
            }
        }
        w.renumber();
        w
    }

    fn check(w: &CellWorld<N>) -> Result<(), String> {
        // Checks 8-12 are judged at the move, against the world before it; reported in this
        // order: a drop with no end (9; the counts can't agree then), the counts (2), the
        // other moves' faults, then the rest.
        if w.fault == Some(Fault::NoEnd) {
            return Err("9: a build dropped with no end recorded".into());
        }
        // 2. Each dropped build counted by `step` exactly once, by the end its terminal holds.
        if w.ends != w.dropped {
            return Err(format!("2: counted {:?}, dropped {:?}", w.ends, w.dropped));
        }
        match w.fault {
            Some(Fault::Next) => {
                return Err("8: a claim's next phase is not the phase its answer leaves".into());
            }
            Some(Fault::Look) => {
                return Err(
                    "10: a look's outcome is not what the end held and the deadline before it say"
                        .into(),
                );
            }
            Some(Fault::Recorded) => {
                return Err("11: a drop recorded an end other than its builder's first".into());
            }
            Some(Fault::Claim) => {
                return Err(
                    "12: a claim's outcome is not what the entry before it and its lookup say"
                        .into(),
                );
            }
            Some(Fault::NoEnd) | None => {}
        }
        // 1. At most one entry for the key: a builder lives iff its build is the entry.
        let builders: Vec<u8> = w
            .claimers
            .iter()
            .filter_map(|c| match *c {
                Claimer::Builder(b, _) => Some(b),
                Claimer::Idle | Claimer::Waiting(..) | Claimer::Told(..) | Claimer::Left(_) => None,
            })
            .collect();
        if builders.len() > 1 || builders.iter().any(|&b| w.entry != Some(b)) {
            return Err(format!("1: builders {builders:?}, entry {:?}", w.entry));
        }
        if w.entry.is_some() && builders.is_empty() {
            return Err(format!("1: entry {:?} with no builder", w.entry));
        }
        // 3. Nothing rejected.
        if w.rejected != 0 {
            return Err(format!("3: rejected {}", w.rejected));
        }
        for (c, claimer) in w.claimers.iter().enumerate() {
            match *claimer {
                Claimer::Told(b, heard) => {
                    let build = w.build(b);
                    // 4. Told its own build's end: the first its builder asked for (else
                    // `Abandoned`), or for a claim told at once what its owner found; a
                    // value only of its own build.
                    let asked = match build.at_once {
                        true => build.asked,
                        false => Some(build.asked.unwrap_or(Heard::Abandoned)),
                    };
                    if asked != Some(heard) || heard.value().is_some_and(|v| v.build != b) {
                        return Err(format!(
                            "4: claimer {c} told {heard:?}, its build asked {asked:?}"
                        ));
                    }
                    // 5. At a told waiter, the ended build is no longer the key's entry.
                    if !build.at_once && w.entry == Some(b) {
                        return Err(format!(
                            "5: claimer {c} told while its ended build is still the key's entry"
                        ));
                    }
                    // 6. Counted before the wake.
                    if !build.at_once && !build.counted {
                        return Err(format!("6: claimer {c} told an end not counted"));
                    }
                }
                Claimer::Waiting(b, _) => {
                    // 6. A recorded end a waiter can hear is counted.
                    let build = w.build(b);
                    if build.terminal.is_some() && !build.counted {
                        return Err(format!("6: claimer {c} waits on an end not counted"));
                    }
                }
                Claimer::Left(b) => {
                    // 7. A left waiter's build goes on: the entry until its builder drops it.
                    if !w.build(b).dropped && w.entry != Some(b) {
                        return Err(format!("7: claimer {c} left a build that is gone"));
                    }
                }
                Claimer::Idle | Claimer::Builder(..) => {}
            }
        }
        Ok(())
    }

    fn quiet(w: &CellWorld<N>) -> bool {
        w.claimers
            .iter()
            .all(|c| !matches!(c, Claimer::Builder(..) | Claimer::Waiting(..)))
    }

    /// A live builder dropped, then each waiter looks. The drain moves no deadline, so a
    /// waiter whose deadline hasn't passed can only end `Told`: its build's end must reach
    /// it within the bound (check 9 holds every dropped build to a recorded end).
    fn drain_move(w: &CellWorld<N>) -> Option<CellMove> {
        let first = |wanted: fn(&Claimer) -> bool| w.claimers.iter().position(wanted);
        first(|c| matches!(c, Claimer::Builder(..)))
            .map(CellMove::Drop)
            .or_else(|| first(|c| matches!(c, Claimer::Waiting(..))).map(CellMove::Look))
    }

    fn answer(_: &CellWorld<N>, action: &CellMove, next: &CellWorld<N>) -> String {
        format!("{:?}", next.claimers[action.claimer()])
    }
}

/// Explores `N` claimers to `depth` within `worlds`: the depth completed, the worlds and the
/// time on stderr; a failed check panics with its schedule.
fn explored<const N: usize>(depth: usize, worlds: usize) -> Stats {
    let started = Instant::now();
    let outcome = explore::<CellModel<N>>(depth, worlds);
    let took = started.elapsed();
    match outcome {
        Ok(stats) => {
            eprintln!(
                "the cell explored: {N} claimers, depth {depth} asked, {} completed, {} worlds, collision bound {:.1e}, {took:?} (drain {:?})",
                stats.completed_depth,
                stats.worlds,
                stats.collision_bound(),
                stats.drain_time,
            );
            assert!(!stats.budget_hit, "the world budget ran out");
            assert_eq!(stats.completed_depth, depth);
            stats
        }
        Err(trace) => panic!(
            "the cell explored: {N} claimers: check {}\nschedule: {:#?}\ndrain: {:#?}\n{} worlds, depth {} completed, {took:?}",
            trace.violation,
            trace.schedule,
            trace.drain,
            trace.stats.worlds,
            trace.stats.completed_depth,
        ),
    }
}

/// The fingerprints each run may keep (the explorer allocates them up front): four times
/// the worlds measured, 12,690 for two claimers to depth 12 and 55,891 for three to 10.
const WORLDS_TWO: usize = 4 * 12_700;
const WORLDS_THREE: usize = 4 * 56_000;

/// Spec (the module docs): every schedule of two claimers' moves to depth 12, and of three
/// to depth 10, holds checks 1-12 in every world, and the server's moves alone bring each
/// world to rest, every waiter whose deadline hasn't passed told an end.
///
/// What it covers: the table's consequences under every interleaving of the claimers, each
/// section of the cell taken as one atomic move as the model writes it. Checks 1, 3, 5, 7,
/// 8 and 12 follow from `step`'s rows (12: each claim joins, is told at once or builds as
/// the entry before it and its lookup say), and 2 from its counted kinds against the
/// model's own tally of the ends the terminals hold. Checks 4, 6, 9, 10 and 11 follow from
/// the model's sections: first-wins naming, an end recorded and counted at the drop (the
/// first its builder named), a claim told at once what its owner had, and a waiter told the
/// end held before it leaves. Each expected value is read from the move's input and the
/// world before the move.
///
/// What it does not cover: it runs no `BuildCell`, so nothing here shows the production
/// cell's order inside its drop, its counters, or its `join`. The cell's own tests observe
/// these, each from outside the drop's critical section:
/// - `a_waiter_woken_by_an_end_never_claims_the_ended_build`: a waiter told an end then
///   finds the key free (`is_idle`, which takes the map's lock);
/// - `a_build_s_named_end_is_told_to_its_waiter_and_counted`: the end told, and the counts
///   once the drop has returned;
/// - `a_builder_that_panics_is_counted_and_its_waiters_are_told_abandoned`: `Abandoned`
///   told, and the counts once the drop has returned;
/// - `a_waiter_whose_stop_fires_once_the_build_has_ended_is_told_the_end`: a waiter whose
///   stop fires once the build has ended is told the end, not left.
///
/// None of them shows that the entry is removed, or the end counted, before the end can be
/// read. A waiter that reads it inside the drop's section still claims only once the map's
/// lock is free.
#[test]
fn the_explored_cell_holds_its_checks_for_every_schedule() {
    explored::<2>(12, WORLDS_TWO);
    explored::<3>(10, WORLDS_THREE);
}
