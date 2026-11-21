//! A value built once per key while those who need it meanwhile wait for it
//! ([`BuildCell`]): the engine's characteristic sets per read model, U1's read view per
//! revision. No lock is held while it builds.
//!
//! Waiting is the hazard ([execution-core.md](../../../docs/design/execution-core.md), "A
//! Rayon worker never waits on a cache flight"): a pool thread that waits may hold, under
//! it, a part of the build's own parallel work, and then neither ends. A build says what
//! kind it is:
//!
//! - [`Kind::Sliced`]: all its parallel work is slices of a [`Scan`], each run alone with
//!   no Rayon inside it, by whoever takes it: the builder, lanes it spawns, and every
//!   waiter. A waiter, on a pool thread too, helps until no slice is left to take and only
//!   then sleeps, for slices others are running; those wait for nothing, so it is woken.
//! - [`Kind::Opaque`]: Rayon work of its own inside. A waiter off the pool sleeps until it
//!   is done (it holds no pool work); a pool thread never waits for it ([`Busy`]).
//!
//! Every build ends once, through its [`Builder`]'s drop, in one step under the cell's lock
//! ([`step`]): the end its builder named ([`End`]), or [`Ended::Abandoned`] where it named
//! none (a panic, a slice's panic raised again in the builder, an early return, a spawn
//! that failed). The key is free before any waiter hears the end, so the next caller builds
//! again, and every end is counted by its kind ([`BuildCell::ends`]). A waiter waits with
//! its own deadline and stop ([`Build::join`]). What the owner keeps of a build is charged
//! to a budget while it is kept ([`Kept`]).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use crate::budget::{Budget, BudgetExceeded, Reservation};
use crate::terminal::{Guard, Terminal, terminal};

/// How a build runs its parallel work, and so who may wait for it (the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Every parallel part a slice of a [`Scan`]: anyone may wait, helping.
    Sliced,
    /// Rayon work of its own: only threads off the pool wait.
    Opaque,
}

/// A pool thread asked to wait for an [`Kind::Opaque`] build: it must go on without the
/// value (or build it some other way that waits for nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

/// The longest a waiter sleeps before it asks its stop and its deadline again: the stores'
/// stops are flags, polled, not signalled.
const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a build ended, named by its builder ([`Builder::end`], [`Builder::finish`]).
pub enum End<V> {
    /// A value given to the waiters as the build's, whether or not its owner newly kept it.
    Kept(Arc<V>),
    /// Nothing to share: the owner's normal exit without a value.
    Empty,
    /// The key is out of date: the build describes a later revision than it was asked for.
    Later,
    /// A value the owner could not keep: given to the waiters where `Some`.
    Unkept(Option<Arc<V>>),
}

/// What a waiter is told of a build's end: the end its builder named, or `Abandoned`, the
/// end of a builder dropped without naming one.
pub enum Ended<V> {
    End(End<V>),
    Abandoned,
}

// By hand: they clone the `Arc`s, so they need no `V: Clone`.
impl<V> Clone for End<V> {
    fn clone(&self) -> Self {
        match self {
            Self::Kept(value) => Self::Kept(Arc::clone(value)),
            Self::Empty => Self::Empty,
            Self::Later => Self::Later,
            Self::Unkept(value) => Self::Unkept(value.clone()),
        }
    }
}

impl<V> Clone for Ended<V> {
    fn clone(&self) -> Self {
        match self {
            Self::End(end) => Self::End(end.clone()),
            Self::Abandoned => Self::Abandoned,
        }
    }
}

/// A key's place in the cell: no entry, or a build's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Absent,
    Building,
}

/// What happens to a key: a claim (its owner's `kept` found nothing, or found its value),
/// or a build's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Claim,
    ClaimFound,
    EndKept,
    EndEmpty,
    EndLater,
    EndUnkept,
    Abandoned,
}

/// Which counter an end adds to ([`Ends`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndKind {
    Kept,
    Empty,
    Later,
    Unkept,
    Abandoned,
}

/// What a claimer is answered: it builds, it joins the build under way, or it is told an
/// end at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Build,
    Join,
    Told,
}

/// One step of a key: its next phase, the claimer's answer, the counter an end adds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Step {
    next: Phase,
    answer: Option<Answer>,
    counts: Option<EndKind>,
}

/// A pair that can't happen: only a builder ends, its entry stays until its own end
/// removes it, and a claim asks its owner's `kept` only where no build is under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rejected {
    // Read by the table test; named for whoever debugs a count of them.
    #[cfg_attr(not(test), allow(dead_code))]
    phase: Phase,
    #[cfg_attr(not(test), allow(dead_code))]
    event: Event,
}

/// The cell's one transition table: every pair of a phase and an event is a row or
/// rejected, each named, with no catch-all arm.
#[deny(clippy::wildcard_enum_match_arm)]
fn step(phase: Phase, event: Event) -> Result<Step, Rejected> {
    let ended = |kind| {
        Ok(Step {
            next: Phase::Absent,
            answer: None,
            counts: Some(kind),
        })
    };
    match phase {
        Phase::Absent => match event {
            Event::Claim => Ok(Step {
                next: Phase::Building,
                answer: Some(Answer::Build),
                counts: None,
            }),
            Event::ClaimFound => Ok(Step {
                next: Phase::Absent,
                answer: Some(Answer::Told),
                counts: None,
            }),
            Event::EndKept
            | Event::EndEmpty
            | Event::EndLater
            | Event::EndUnkept
            | Event::Abandoned => Err(Rejected { phase, event }),
        },
        Phase::Building => match event {
            Event::Claim => Ok(Step {
                next: Phase::Building,
                answer: Some(Answer::Join),
                counts: None,
            }),
            Event::EndKept => ended(EndKind::Kept),
            Event::EndEmpty => ended(EndKind::Empty),
            Event::EndLater => ended(EndKind::Later),
            Event::EndUnkept => ended(EndKind::Unkept),
            Event::Abandoned => ended(EndKind::Abandoned),
            Event::ClaimFound => Err(Rejected { phase, event }),
        },
    }
}

/// The event of a builder's pending end at its drop: none named is `Abandoned`.
#[deny(clippy::wildcard_enum_match_arm)]
fn event_of<V>(pending: Option<&End<V>>) -> Event {
    match pending {
        Some(End::Kept(_)) => Event::EndKept,
        Some(End::Empty) => Event::EndEmpty,
        Some(End::Later) => Event::EndLater,
        Some(End::Unkept(_)) => Event::EndUnkept,
        None => Event::Abandoned,
    }
}

/// The end of a sleep that starts now: `STOP_POLL` from now, or `deadline` where it comes
/// first. Never unbounded: at `Instant`'s far edge it is now.
fn slice_end(deadline: Option<Instant>) -> Instant {
    let now = Instant::now();
    let polled = now.checked_add(STOP_POLL).unwrap_or(now);
    deadline.map_or(polled, |deadline| deadline.min(polled))
}

/// What [`Build::join`] gives: the build's end, [`Busy`] (a pool thread and an opaque build
/// under way), or `Left` (the waiter's deadline passed or its stop fired; the build goes on).
pub enum Joined<V> {
    Told(Ended<V>),
    Busy,
    Left,
}

/// One build of a key's value: its current [`Scan`], which waiters help with, and its end
/// once recorded.
pub struct Build<V> {
    kind: Kind,
    state: Mutex<State<V>>,
    changed: Condvar,
    /// Waiters asleep on it (they have helped with what there was).
    waiting: AtomicUsize,
    /// Recorded once, by the builder's drop: its end, or `Abandoned`.
    end: Terminal<Ended<V>>,
}

struct State<V> {
    work: Option<Arc<dyn Help>>,
    /// The end the builder named, first named first ([`Builder::finish`], [`Builder::end`]).
    pending: Option<End<V>>,
}

impl<V> Build<V> {
    fn new(kind: Kind, end: Terminal<Ended<V>>) -> Arc<Self> {
        Arc::new(Self {
            kind,
            state: Mutex::new(State {
                work: None,
                pending: None,
            }),
            changed: Condvar::new(),
            waiting: AtomicUsize::new(0),
            end,
        })
    }

    fn publish(&self, work: Option<Arc<dyn Help>>) {
        lock(&self.state).work = work;
        self.changed.notify_all();
    }

    /// The build's value once it is over (`None`: it gave none, or failed), its slices
    /// helped with until then; [`Busy`] at once on a pool thread for an opaque build that
    /// isn't over. [`Self::join`] with no deadline and no stop.
    pub fn wait(&self) -> Result<Option<Arc<V>>, Busy> {
        match self.join(None, &|| false) {
            Joined::Told(Ended::End(End::Kept(value) | End::Unkept(Some(value)))) => {
                Ok(Some(value))
            }
            Joined::Told(
                Ended::End(End::Empty | End::Later | End::Unkept(None)) | Ended::Abandoned,
            ) => Ok(None),
            Joined::Busy => Err(Busy),
            // Never: no deadline, and a stop that never fires.
            Joined::Left => Ok(None),
        }
    }

    /// The build's end, waited for until `deadline` or until `stop` fires (`Left`; the build
    /// goes on). An end already recorded is told first. Every sleep ends within `STOP_POLL`,
    /// and `stop` and `deadline` are asked after it; a waiter helping with a sliced build asks
    /// them before each slice it takes, so it leaves at most one slice past them. They are
    /// asked with no lock of the build's held (`stop` may read the cell), and the end is read
    /// after them: an end recorded by the time they fired is told, not left. [`Busy`] at once
    /// on a pool thread for an opaque build.
    pub fn join(&self, deadline: Option<Instant>, stop: &dyn Fn() -> bool) -> Joined<V> {
        let left = || deadline.is_some_and(|at| Instant::now() >= at) || stop();
        loop {
            if let Some(end) = self.end.get() {
                return Joined::Told(end);
            }
            match self.kind {
                Kind::Opaque => {
                    let gone = left();
                    if let Some(end) = self.end.get() {
                        return Joined::Told(end);
                    }
                    if rayon::current_thread_index().is_some() {
                        return Joined::Busy;
                    }
                    if gone {
                        return Joined::Left;
                    }
                    self.waiting.fetch_add(1, Ordering::SeqCst);
                    let end = self.end.wait_until(slice_end(deadline));
                    self.waiting.fetch_sub(1, Ordering::SeqCst);
                    if let Some(end) = end {
                        return Joined::Told(end);
                    }
                }
                Kind::Sliced => {
                    let work = lock(&self.state).work.clone();
                    if let Some(work) = &work {
                        work.help(&|| !left());
                    }
                    let gone = left();
                    // The end is recorded under `state`, then `changed` notified: read under it,
                    // no wake is missed.
                    let state = lock(&self.state);
                    if let Some(end) = self.end.get() {
                        return Joined::Told(end);
                    }
                    if gone {
                        return Joined::Left;
                    }
                    let same = match (&state.work, &work) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                    if same {
                        self.waiting.fetch_add(1, Ordering::SeqCst);
                        let sleep = slice_end(deadline).saturating_duration_since(Instant::now());
                        let slept = self.changed.wait_timeout(state, sleep);
                        self.waiting.fetch_sub(1, Ordering::SeqCst);
                        drop(slept);
                    }
                }
            }
        }
    }

    /// Waiters asleep on it: what the tests hold a build for. A waiter's count dips between
    /// its sleeps: poll it, never read it once.
    #[doc(hidden)]
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

/// Every build a cell ended, by how it ended ([`BuildCell::ends`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ends {
    /// Values given to the waiters as [`End::Kept`]: not a promise that each was newly kept.
    pub kept: u64,
    pub empty: u64,
    pub later: u64,
    pub unkept: u64,
    pub abandoned: u64,
}

#[derive(Default)]
struct Counts {
    kept: AtomicU64,
    empty: AtomicU64,
    later: AtomicU64,
    unkept: AtomicU64,
    abandoned: AtomicU64,
    rejected: AtomicU64,
}

impl Counts {
    fn add(&self, kind: EndKind) {
        let counter = match kind {
            EndKind::Kept => &self.kept,
            EndKind::Empty => &self.empty,
            EndKind::Later => &self.later,
            EndKind::Unkept => &self.unkept,
            EndKind::Abandoned => &self.abandoned,
        };
        counter.fetch_add(1, Ordering::SeqCst);
    }
}

/// The builds under way, by key: one at a time per key.
pub struct BuildCell<K, V> {
    building: Mutex<HashMap<K, Arc<Build<V>>>>,
    counts: Counts,
}

impl<K, V> Default for BuildCell<K, V> {
    fn default() -> Self {
        Self {
            building: Mutex::new(HashMap::new()),
            counts: Counts::default(),
        }
    }
}

/// What a [`BuildCell::claim`] gives: the build to run, or one to wait for (under way, or
/// already ended).
pub enum Claim<'c, K: Eq + Hash + Clone, V> {
    Build(Builder<'c, K, V>),
    Wait(Arc<Build<V>>),
}

impl<K: Eq + Hash + Clone, V> BuildCell<K, V> {
    /// No build under way, for any key.
    pub fn is_idle(&self) -> bool {
        lock(&self.building).is_empty()
    }

    /// Every build ended so far, by how it ended. An end is counted before its waiters are
    /// told it.
    pub fn ends(&self) -> Ends {
        let read = |counter: &AtomicU64| counter.load(Ordering::SeqCst);
        let c = &self.counts;
        Ends {
            kept: read(&c.kept),
            empty: read(&c.empty),
            later: read(&c.later),
            unkept: read(&c.unkept),
            abandoned: read(&c.abandoned),
        }
    }

    /// Pairs of a phase and an event that can't happen, met nonetheless: 0.
    pub fn rejected(&self) -> u64 {
        self.counts.rejected.load(Ordering::SeqCst)
    }

    /// Waiters asleep on `key`'s build under way (0 with none): what the tests of the cell's
    /// users hold a build for. A waiter's count dips between its sleeps: poll it.
    #[doc(hidden)]
    pub fn waiting(&self, key: &K) -> usize {
        lock(&self.building)
            .get(key)
            .map_or(0, |build| build.waiting())
    }
}

impl<K: Eq + Hash + Clone, V: Send + Sync + 'static> BuildCell<K, V> {
    /// The build of `key`'s value for this caller to run, unless one is under way (then
    /// that one), or `kept` (called under the cell's lock, briefly) finds the value kept
    /// meanwhile (then a build ended with it: [`End::Kept`], or [`End::Empty`] for a kept
    /// failure): a build that keeps its value before it ends is never run twice. `kept` must
    /// not drop a [`Builder`] of this cell.
    #[deny(clippy::wildcard_enum_match_arm)]
    pub fn claim(
        &self,
        key: K,
        kind: Kind,
        kept: impl FnOnce() -> Option<Option<Arc<V>>>,
    ) -> Claim<'_, K, V> {
        // Made before the lock: its fallback takes none, and nothing of it runs under it.
        let (end, guard) = terminal(|| Ended::Abandoned);
        let mut building = lock(&self.building);
        let running = building.get(&key).map(Arc::clone);
        let found = match running {
            Some(_) => None,
            None => kept().map(|found| match found {
                Some(value) => End::Kept(value),
                None => End::Empty,
            }),
        };
        let phase = match running {
            Some(_) => Phase::Building,
            None => Phase::Absent,
        };
        let event = match found {
            Some(_) => Event::ClaimFound,
            None => Event::Claim,
        };
        let answer = match step(phase, event) {
            Ok(step) => step.answer,
            Err(_) => None,
        };
        let told = match answer {
            Some(Answer::Build) => {
                let build = Build::new(kind, end);
                building.insert(key.clone(), Arc::clone(&build));
                drop(building);
                return Claim::Build(Builder {
                    cell: self,
                    key,
                    build,
                    guard: Some(guard),
                });
            }
            Some(Answer::Join) => match running {
                Some(running) => {
                    drop(building);
                    return Claim::Wait(running);
                }
                None => None,
            },
            Some(Answer::Told) => found,
            None => None,
        };
        drop(building);
        match told {
            Some(told) => {
                guard.end(Ended::End(told));
            }
            // Never: a rejected pair, answered as a build ended `Abandoned` (the guard's
            // fallback, as it drops), and counted.
            None => {
                self.counts.rejected.fetch_add(1, Ordering::SeqCst);
            }
        }
        drop(guard);
        Claim::Wait(Build::new(kind, end))
    }

    /// The build of `key`'s value for a caller that must build whatever is kept: a build
    /// under way is waited for first ([`Busy`]: on a pool thread, for an opaque one).
    pub fn claim_anyway(&self, key: K, kind: Kind) -> Result<Builder<'_, K, V>, Busy> {
        loop {
            match self.claim(key.clone(), kind, || None) {
                Claim::Build(builder) => return Ok(builder),
                Claim::Wait(build) => {
                    build.wait()?;
                }
            }
        }
    }
}

/// The claim on a key's build: over when dropped, its end then recorded and told to those
/// who wait: the end it was given ([`Builder::finish`], [`Builder::end`]), else `Abandoned`
/// (a builder that panics, returns early or never ran names none).
pub struct Builder<'c, K: Eq + Hash + Clone, V> {
    cell: &'c BuildCell<K, V>,
    key: K,
    build: Arc<Build<V>>,
    /// Records the end; taken by the drop.
    guard: Option<Guard<Ended<V>>>,
}

impl<K: Eq + Hash + Clone, V> Builder<'_, K, V> {
    /// The build, which [`Scan::run`] publishes its slices on.
    pub fn build(&self) -> &Build<V> {
        &self.build
    }

    /// The value built (`None`: none), for those who wait: [`End::Kept`] or [`End::Empty`],
    /// told at the drop. It takes no lock of the cell's, so it may be called under the
    /// owner's own locks. The first end named is the end.
    pub fn finish(&self, value: Option<Arc<V>>) {
        self.name(match value {
            Some(value) => End::Kept(value),
            None => End::Empty,
        });
    }

    /// Ends the build with `end` (unless one was named before) and drops the builder.
    pub fn end(self, end: End<V>) {
        self.name(end);
    }

    fn name(&self, end: End<V>) {
        let mut state = lock(&self.build.state);
        if state.pending.is_none() {
            state.pending = Some(end);
        }
    }
}

impl<K: Eq + Hash + Clone, V> Drop for Builder<'_, K, V> {
    /// The one transition of a build's end, under the cell's lock, then the build's `state`
    /// (then the end's own): the entry removed, the end counted, the work let go, the end
    /// recorded and the waiters woken, all in one critical section. So a waiter hears the
    /// end only once the key is free, and the end is counted before it.
    fn drop(&mut self) {
        let mut building = lock(&self.cell.building);
        let ours = building
            .get(&self.key)
            .is_some_and(|build| Arc::ptr_eq(build, &self.build));
        let phase = match ours {
            true => Phase::Building,
            false => Phase::Absent,
        };
        let mut state = lock(&self.build.state);
        let pending = state.pending.take();
        match step(phase, event_of(pending.as_ref())) {
            Ok(step) => {
                if step.next == Phase::Absent {
                    building.remove(&self.key);
                }
                if let Some(kind) = step.counts {
                    self.cell.counts.add(kind);
                }
            }
            Err(_) => {
                self.cell.counts.rejected.fetch_add(1, Ordering::SeqCst);
            }
        }
        state.work = None;
        // A named end recorded, else the guard's fallback (`Abandoned`) as it drops.
        let guard = self.guard.take();
        if let (Some(end), Some(guard)) = (pending, &guard) {
            guard.end(Ended::End(end));
        }
        drop(guard);
        self.build.changed.notify_all();
        drop(state);
        drop(building);
    }
}

/// A scan's slices, as a queue: whoever helps claims the next and runs it alone, while
/// `go_on` says so (asked before each slice it claims).
trait Help: Send + Sync {
    fn help(&self, go_on: &dyn Fn() -> bool);
}

/// One scan of a sliced build: `run(i)` for every slice `i`, its results kept in slice
/// order; a slice that finds too much (`None`) or panics stops the rest.
pub struct Scan<R> {
    run: Box<dyn Fn(usize) -> Option<R> + Send + Sync>,
    len: usize,
    next: AtomicUsize,
    failed: AtomicBool,
    /// The first slice's panic, raised again in the builder ([`Scan::run`]): a slice that
    /// panics on a lane or in a waiter leaves the queue to the others, never stuck.
    panic: Mutex<Option<Box<dyn std::any::Any + Send>>>,
    results: Mutex<Vec<Option<R>>>,
    /// Slices not yet over (claimed or not); `over` is notified when none are left.
    left: Mutex<usize>,
    over: Condvar,
}

impl<R: Send + 'static> Scan<R> {
    pub fn new(len: usize, run: impl Fn(usize) -> Option<R> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            run: Box::new(run),
            len,
            next: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            panic: Mutex::new(None),
            results: Mutex::new(std::iter::repeat_with(|| None).take(len).collect()),
            left: Mutex::new(len),
            over: Condvar::new(),
        })
    }

    /// Publishes the scan on `build` for its waiters to help with before [`Scan::run`] (which
    /// publishes it too): what the tests hold a build with.
    #[doc(hidden)]
    pub fn publish_on<V>(self: &Arc<Self>, build: &Build<V>) {
        build.publish(Some(Arc::clone(self) as Arc<dyn Help>));
    }

    /// Every slice's result in order, once all are over: helped with by `lanes` jobs on
    /// the current Rayon pool, by this thread, and by `build`'s waiters where `publish`
    /// puts it there (a sliced build's). `None` if a slice found too much; a slice's
    /// panic is raised again here, once every slice is over.
    pub fn run<V>(
        self: Arc<Self>,
        build: &Build<V>,
        lanes: usize,
        publish: bool,
    ) -> Option<Vec<R>> {
        debug_assert!(
            !publish || build.kind == Kind::Sliced,
            "only a sliced build's waiters help"
        );
        if publish {
            build.publish(Some(Arc::clone(&self) as Arc<dyn Help>));
        }
        for _ in 0..lanes.min(self.len) {
            let scan = Arc::clone(&self);
            rayon::spawn(move || scan.help(&|| true));
        }
        // A builder outside the pool waits for the lanes, as for a parallel loop, and
        // allocates nothing for the scan: its heap is the queries' after a load. With no
        // lanes, or on a pool thread, it scans too.
        if lanes == 0 || rayon::current_thread_index().is_some() {
            self.help(&|| true);
        }
        let mut left = lock(&self.left);
        while *left > 0 {
            left = self.over.wait(left).unwrap_or_else(PoisonError::into_inner);
        }
        drop(left);
        if publish {
            build.publish(None);
        }
        if let Some(panic) = lock(&self.panic).take() {
            std::panic::resume_unwind(panic);
        }
        if self.failed.load(Ordering::SeqCst) {
            return None;
        }
        std::mem::take(&mut *lock(&self.results))
            .into_iter()
            .collect()
    }
}

impl<R: Send> Help for Scan<R> {
    fn help(&self, go_on: &dyn Fn() -> bool) {
        /// A slice over, whether it ran, failed, or panicked.
        struct Over<'s, R>(&'s Scan<R>);
        impl<R> Drop for Over<'_, R> {
            fn drop(&mut self) {
                let mut left = lock(&self.0.left);
                *left -= 1;
                if *left == 0 {
                    self.0.over.notify_all();
                }
            }
        }
        // Asked before a slice is claimed: one claimed is always run (or skipped) and over.
        while go_on() {
            let i = self.next.fetch_add(1, Ordering::SeqCst);
            if i >= self.len {
                return;
            }
            let _over = Over(self);
            if self.failed.load(Ordering::SeqCst) {
                continue;
            }
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.run)(i)));
            match ran {
                Ok(Some(result)) => lock(&self.results)[i] = Some(result),
                Ok(None) => self.failed.store(true, Ordering::SeqCst),
                Err(panic) => {
                    self.failed.store(true, Ordering::SeqCst);
                    lock(&self.panic).get_or_insert(panic);
                }
            }
        }
    }
}

/// A built value as its owner keeps it, charged to the owner's budget. [`Kept::new`]: its
/// `Arc`'s block for as long as it is kept, its own memory by its charged containers for as
/// long as any `Arc` of it lives. [`Kept::charge`]: the bytes given, for as long as it is kept
/// (a waiter's own `Arc` isn't charged after the owner lets go).
pub struct Kept<V> {
    value: Arc<V>,
    held: Reservation,
}

/// The block an `Arc<V>` allocates: std's `ArcInner` (two counts, then the value; `repr(C)`
/// there too), so that what [`Kept::new`] charges is its layout.
#[repr(C)]
struct ArcBlock<V>(AtomicUsize, AtomicUsize, V);

impl<V> Kept<V> {
    /// `value` kept, the block its `Arc` takes charged to `budget` before it is allocated:
    /// for a value whose own memory its containers charge as they grow (none charged twice).
    /// Nothing is charged where `budget` has no room.
    pub fn new(value: V, budget: Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let mut held = Reservation::new(budget);
        held.charge(std::mem::size_of::<ArcBlock<V>>())?;
        Ok(Self {
            value: Arc::new(value),
            held,
        })
    }

    /// `value` kept, `bytes` charged to `budget`; nothing charged where it can't take them.
    pub fn charge(
        value: Arc<V>,
        bytes: usize,
        budget: Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let mut held = Reservation::new(budget);
        held.charge(bytes)?;
        Ok(Self { value, held })
    }

    pub fn value(&self) -> &Arc<V> {
        &self.value
    }

    /// The bytes charged.
    pub fn bytes(&self) -> usize {
        self.held.bytes()
    }
}

impl<V> std::fmt::Debug for Kept<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kept")
            .field("bytes", &self.bytes())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "cell_tests.rs"]
mod tests;
