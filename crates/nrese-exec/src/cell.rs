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
//! A slice that panics fails the build (the panic is raised again in the builder) and never
//! strands it: the waiters wake without a value, and the next caller builds again. What the
//! owner keeps of a build is charged to a budget while it is kept ([`Kept`]).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::budget::{Budget, BudgetExceeded, Reservation};

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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One build of a key's value: its current [`Scan`], which waiters help with, and once it
/// is over what it built (`Some(None)`: nothing, or it failed).
pub struct Build<V> {
    kind: Kind,
    state: Mutex<State<V>>,
    changed: Condvar,
    /// Waiters asleep on it (they have helped with what there was).
    waiting: AtomicUsize,
}

struct State<V> {
    work: Option<Arc<dyn Help>>,
    done: Option<Option<Arc<V>>>,
}

impl<V> Build<V> {
    fn new(kind: Kind, done: Option<Option<Arc<V>>>) -> Arc<Self> {
        Arc::new(Self {
            kind,
            state: Mutex::new(State { work: None, done }),
            changed: Condvar::new(),
            waiting: AtomicUsize::new(0),
        })
    }

    fn publish(&self, work: Option<Arc<dyn Help>>) {
        lock(&self.state).work = work;
        self.changed.notify_all();
    }

    /// The build's value once it is over (`None`: it built nothing, or failed), its slices
    /// helped with until then; [`Busy`] at once on a pool thread for an opaque build that
    /// isn't over.
    pub fn wait(&self) -> Result<Option<Arc<V>>, Busy> {
        loop {
            let work = {
                let state = lock(&self.state);
                if let Some(done) = &state.done {
                    return Ok(done.clone());
                }
                if self.kind == Kind::Opaque && rayon::current_thread_index().is_some() {
                    return Err(Busy);
                }
                state.work.clone()
            };
            if let Some(work) = &work {
                work.help();
            }
            let mut state = lock(&self.state);
            if let Some(done) = &state.done {
                return Ok(done.clone());
            }
            let same = match (&state.work, &work) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if same {
                self.waiting.fetch_add(1, Ordering::SeqCst);
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                self.waiting.fetch_sub(1, Ordering::SeqCst);
            }
            drop(state);
        }
    }

    /// Waiters asleep on it: what the tests hold a build for.
    #[doc(hidden)]
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

/// The builds under way, by key: one at a time per key.
pub struct BuildCell<K, V> {
    building: Mutex<HashMap<K, Arc<Build<V>>>>,
}

impl<K, V> Default for BuildCell<K, V> {
    fn default() -> Self {
        Self {
            building: Mutex::new(HashMap::new()),
        }
    }
}

/// What a [`BuildCell::claim`] gives: the build to run, or one to wait for.
pub enum Claim<'c, K: Eq + Hash + Clone, V> {
    Build(Builder<'c, K, V>),
    Wait(Arc<Build<V>>),
}

impl<K: Eq + Hash + Clone, V> BuildCell<K, V> {
    /// The build of `key`'s value for this caller to run, unless one is under way (then
    /// that one), or `kept` (called under the cell's lock, briefly) finds the value kept
    /// meanwhile: a build that keeps its value before it ends is never run twice.
    pub fn claim(
        &self,
        key: K,
        kind: Kind,
        kept: impl FnOnce() -> Option<Option<Arc<V>>>,
    ) -> Claim<'_, K, V> {
        let mut building = lock(&self.building);
        if let Some(build) = building.get(&key) {
            return Claim::Wait(Arc::clone(build));
        }
        if let Some(done) = kept() {
            return Claim::Wait(Build::new(kind, Some(done)));
        }
        let build = Build::new(kind, None);
        building.insert(key.clone(), Arc::clone(&build));
        Claim::Build(Builder {
            cell: self,
            key,
            build,
        })
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

    /// No build under way, for any key.
    pub fn is_idle(&self) -> bool {
        lock(&self.building).is_empty()
    }
}

/// The claim on a key's build: over when dropped, its value given to those who wait (none,
/// unless [`Builder::finish`] gave one: a builder that panics gives them nothing).
pub struct Builder<'c, K: Eq + Hash + Clone, V> {
    cell: &'c BuildCell<K, V>,
    key: K,
    build: Arc<Build<V>>,
}

impl<K: Eq + Hash + Clone, V> Builder<'_, K, V> {
    /// The build, which [`Scan::run`] publishes its slices on.
    pub fn build(&self) -> &Build<V> {
        &self.build
    }

    /// The value built, for those who wait.
    pub fn finish(&self, value: Option<Arc<V>>) {
        lock(&self.build.state).done = Some(value);
    }
}

impl<K: Eq + Hash + Clone, V> Drop for Builder<'_, K, V> {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.build.state);
            state.work = None;
            state.done.get_or_insert(None);
        }
        self.build.changed.notify_all();
        let mut building = lock(&self.cell.building);
        if building
            .get(&self.key)
            .is_some_and(|build| Arc::ptr_eq(build, &self.build))
        {
            building.remove(&self.key);
        }
    }
}

/// A scan's slices, as a queue: whoever helps claims the next and runs it alone.
trait Help: Send + Sync {
    fn help(&self);
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
            rayon::spawn(move || scan.help());
        }
        // A builder outside the pool waits for the lanes, as for a parallel loop, and
        // allocates nothing for the scan: its heap is the queries' after a load. With no
        // lanes, or on a pool thread, it scans too.
        if lanes == 0 || rayon::current_thread_index().is_some() {
            self.help();
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
    fn help(&self) {
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
        loop {
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
mod tests {
    use super::*;

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
}
