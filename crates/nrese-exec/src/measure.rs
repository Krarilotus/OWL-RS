//! What an operation allocates, held against what it reserved: a test binary installs
//! [`Measured`] as its global allocator, [`measure`] reports the most bytes an operation
//! held at once, and [`measure_within`] the most it held beyond what it reserved of a
//! budget. An allocation that wasn't reserved first, or a charge released while its memory
//! lives, fails the test instead of needing a review, wherever in the operation it happens.
//! Test-only: built for this crate's tests and with the `measure` feature, which test
//! builds of other crates turn on; product builds have none of it. In a binary whose
//! global allocator isn't [`Measured`], [`measure`], [`measure_within`] and [`alone`]
//! panic, naming it: there a measurement would count nothing and pass.
//!
//! # What a measurement counts
//!
//! The bytes asked of the allocator on every thread of the process (the operation's own,
//! threads it starts, pools it hands work to), so a measured test runs [`alone`] in its
//! process. Of these, a measurement counts only what was made while it ran:
//! - a block, allocated while it ran, from its allocation to its free, and so memory a
//!   holder maps itself, outside the allocator, which reports it ([`mapped`]);
//! - a [`Reservation`](crate::Reservation) made while it ran: its charges and releases;
//! - a charge to its budget itself ([`Budget::charge`]) made while it ran, and of the
//!   releases to the budget itself ([`Budget::release`]) no more than such charges hold.
//!
//! A charge or release counts if it is of the measurement's budget or of a part of it
//! ([`crate::Envelope::part`]). Freeing a block that was there before, or dropping a
//! reservation made before, counts for nothing; a reservation made before that grows
//! doesn't count for it either ([`crate::ChargedVec`] renews its reservation as it grows).
//!
//! # The invariant
//!
//! A measurement's balance is the bytes of the blocks it counts that are live, less the
//! bytes it counts as charged and not released. [`measure_within`] reports the most it
//! rose to, at any event of any thread; an operation that reserves before it allocates and
//! frees before it releases keeps it at or below 0.
//!
//! Why it holds: every event is one change of the balance under the measurement's lock, so
//! the events of all threads fall in one order that keeps each thread's own. A block and a
//! reservation are counted all or nothing: each carries the generation read once when it
//! was made (the block in a header, the reservation in a field), and each of its events
//! counts for exactly the measurements running at that event that started at or before
//! that generation. A measurement is running before the generation reaches its own (it
//! publishes itself, then the generation, under one lock that orders starts), so one that
//! started at or before a block's generation was running when the block was made, and
//! counted it then; if it still runs when the block is freed, it counts the free. A
//! measurement that started later counts neither. The same holds for a reservation's
//! charges and releases. A release to the budget itself counts no more than the
//! measurement's own such charges still held: what it counts is never more than it
//! charged, so the balance is never below the truth (no false pass), and such a release of
//! an earlier charge counts for nothing while the measurement holds none.
//!
//! # Known limits
//!
//! A measurement counts every thread of its process, so a measured test runs in a child of
//! its own, on the test harness's main thread with no other test ([`alone`], under
//! cargo-nextest too): libtest's main thread, beside a test running on its own thread,
//! writes after 60 seconds that the test is running long, and that allocates (594 bytes
//! counted in a measurement across the mark, 20 Oct). The child is recognised by a marker
//! variable naming the test together with the arguments it was started with; a process
//! started by hand with the same variable and those arguments is taken for the child,
//! which it is in all but name.
//!
//! What a measurement counts is what it can tell apart. Three patterns of correct code
//! are told apart wrongly; each has a rule for measured tests:
//! - A reservation made before the measurement and used in it (moved in, then charged,
//!   allocated, freed, released) counts none of its charges, but the blocks do: a false
//!   failure. Rule: make the reservations a measured operation uses inside the
//!   measurement ([`crate::ChargedVec`] renews its own as it grows).
//! - A release to a budget itself ([`Budget::release`], the executor's `consumed`) of a
//!   charge made before the measurement counts against the measurement's own such charges
//!   still held: charging a new table directly, then freeing and releasing an earlier one,
//!   reports the earlier one's bytes: a false failure. Rule: in measured work, release
//!   earlier direct charges before making new ones, or make the inputs inside the
//!   measurement.
//! - [`crate::Reservation::adopt`] of a direct charge made before the measurement makes it
//!   the measurement's charge, which then covers new blocks while the old one still lives:
//!   a false pass. Rule: adopt only charges made inside the measurement, or make the inputs
//!   inside it.
//!
//! - Pool threads tidy up after parallel work returns (crossbeam-epoch hands each
//!   thread's garbage on, about 2 KB a thread), and a pool dropped ends its threads later:
//!   a measurement that starts meanwhile counts it. Rule: make a measured test's inputs
//!   without parallel work, and keep the pools it measures on for the whole process.
//!
//! And rayon's global queue (see [`prepare`]): work that enters the global pool from
//! outside more than 62 times, or a pool of its own from outside, allocates a queue block
//! of 1,520 bytes per 63 entries, which it counts.
//!
//! And a pool's threads as they start: each allocates its own queue (1,520 bytes) and, as
//! it first looks for work, its crossbeam-epoch handle (2,304), on its own thread, when the
//! system runs it, which under load can be well after the pool was made. [`prepare`] waits
//! for every thread of the global pool to start before the first measurement, from outside
//! any pool, whatever pool that measurement runs on: input built on the global pool, then
//! a measurement on another, counted the global threads that started late, in pairs of
//! those sizes (the GeoSPARQL guard's rare excess, attributed 20 Oct). That wait for the
//! threads to start runs no job and lasts at most 4 seconds a pool: a global thread busy for
//! longer, or one that never runs its part (the caller's own, if it is one, or one blocked on
//! this preparation), is not waited for, and if it hasn't started, a measurement counts its
//! start. A pool of a test's own is the test's: rule, start its threads before measuring
//! (`pool.broadcast(|_| ())`), or a measurement counts those that start in it.
//!
//! And the preparation's own waits for the global pool, which are not bounded: outside any
//! pool, the first measurement makes the caller's own thread state with a `rayon::join`,
//! within the preparation that runs once per process, and every measurement aligns rayon's
//! queue with such joins (see [`prepare`]); each waits for a thread of the global pool to run
//! its job. A global thread that waits for that preparation, or for a lock the measuring
//! thread holds, never does, and neither returns (Sol, measure-pool r3; the base had the same
//! waits). So, the rule for a measured test: its first measurement is not made while a thread
//! of a pool is entering one, and a measuring thread holds no lock a pool's job needs. In the
//! suite's own runs it did not occur: 0 hangs in 91 runs of this module's tests recorded from
//! 19 to 21 October 2026 (commit hooks, push gates and fleet proofs). A preparation that
//! waits on no pool is the follow-up.
//!
//! Counts are `i128`: a `usize` converts exactly, and no sum of them reaches its bounds.
//! A realloc counts the new block, then frees the old: it may move the bytes. Measurements
//! nest (up to 8 at once); one that unwinds ends as one that returns, and no thread reads
//! its budget after.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::UnsafeCell;
use std::hint::spin_loop;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};

use crate::Budget;

/// A global allocator that forwards every call to `A`, counting while a measurement runs.
pub struct Measured<A>(pub A);

/// Measurements that can run at once (nested, or by tests that share a process).
const SLOTS: usize = 8;

/// One measurement's counts.
#[derive(Clone, Copy)]
struct Counts {
    /// The budget it holds what is live against ([`measure_within`]); never read through.
    budget: *const Budget,
    live: i128,
    peak: i128,
    balance: i128,
    excess: i128,
    /// Charged to the budget itself while it ran, not yet released.
    own: i128,
}

impl Counts {
    const NONE: Self = Self {
        budget: std::ptr::null(),
        live: 0,
        peak: 0,
        balance: 0,
        excess: 0,
        own: 0,
    };

    fn rise(&mut self, bytes: i128) {
        self.balance += bytes;
        self.excess = self.excess.max(self.balance);
    }
}

/// Where a measurement runs.
struct Slot {
    taken: AtomicBool,
    /// The generation the measurement started at; 0 while none runs.
    since: AtomicU64,
    lock: AtomicBool,
    counts: UnsafeCell<Counts>,
}

// SAFETY: `counts` is read and written only under `lock`.
unsafe impl Sync for Slot {}

impl Slot {
    const fn free() -> Self {
        Self {
            taken: AtomicBool::new(false),
            since: AtomicU64::new(0),
            lock: AtomicBool::new(false),
            counts: UnsafeCell::new(Counts::NONE),
        }
    }

    /// `f` on the counts, with the generation the measurement started at (0: none runs),
    /// under the lock.
    fn with<R>(&self, f: impl FnOnce(u64, &mut Counts) -> R) -> R {
        while self
            .lock
            .compare_exchange_weak(false, true, SeqCst, SeqCst)
            .is_err()
        {
            spin_loop();
        }
        // SAFETY: under the lock.
        let result = f(self.since.load(SeqCst), unsafe { &mut *self.counts.get() });
        self.lock.store(false, SeqCst);
        result
    }
}

static RUNNING: [Slot; SLOTS] = [const { Slot::free() }; SLOTS];
/// Measurements running, so that events skip the slots while there are none.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
/// The generation of the last measurement started; blocks and reservations record it.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Held while a measurement starts: starts publish their generations in order.
static STARTING: AtomicBool = AtomicBool::new(false);
/// Panics unless [`Measured`] is the process's global allocator: without it a measurement
/// counts nothing, and passes whatever the work holds. Asked anew each time: one byte from
/// the global allocator, uncounted, which only a [`Measured`] that is the global allocator
/// sees (a `Measured` of the test's own, called directly, never does).
fn require_measured() {
    let before = UNCOUNTED.replace(Some(0));
    let layout = Layout::new::<u8>();
    // SAFETY: a layout of non-zero size; the block, if one was made, is freed with it.
    unsafe {
        let block = std::alloc::alloc(layout);
        if !block.is_null() {
            std::alloc::dealloc(block, layout);
        }
    }
    let seen = UNCOUNTED.replace(before).is_some_and(|bytes| bytes > 0);
    assert!(
        seen,
        "a measurement needs nrese_exec::measure::Measured as this test binary's global \
         allocator, or it counts nothing and passes: add `#[global_allocator] static \
         MEASURED: nrese_exec::measure::Measured<std::alloc::System> = \
         nrese_exec::measure::Measured(std::alloc::System);` to the binary (a crate's unit \
         tests are one binary, each file in tests/ another)"
    );
}

/// The generation a block or reservation made now records.
pub(crate) fn tag() -> u64 {
    GENERATION.load(SeqCst)
}

/// `f` on the counts of each running measurement an event counts for: of a block or
/// reservation of generation `tag`, or with `None` an event of a budget itself (any
/// running measurement).
fn each(tag: Option<u64>, mut f: impl FnMut(&mut Counts)) {
    if ACTIVE.load(SeqCst) == 0 {
        return;
    }
    let counts = |since: u64| since != 0 && tag.is_none_or(|tag| since <= tag);
    for slot in &RUNNING {
        if counts(slot.since.load(SeqCst)) {
            slot.with(|since, c| {
                if counts(since) {
                    f(c);
                }
            });
        }
    }
}

/// Whether a measurement of `budget` counts a charge or release of `of`: `budget` is `of`
/// or the envelope `of` is a part of.
fn within(budget: *const Budget, of: &Budget) -> bool {
    let mut at = Some(of);
    while let Some(b) = at {
        if std::ptr::eq(b, budget) {
            return true;
        }
        at = b.above();
    }
    false
}

fn bytes(bytes: usize) -> i128 {
    i128::from(bytes as u64)
}

thread_local! {
    /// The bytes this thread allocated uncounted, while it aligns rayon's queue
    /// ([`prepare`]) or asks whether [`Measured`] is the global allocator; `None` while it
    /// does neither.
    static UNCOUNTED: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The generation a block of `size` bytes made now records, and whether it is counted: not
/// while its thread aligns rayon's queue or asks for [`Measured`], whose blocks record 0,
/// before every measurement, so that none counts their allocation or their free.
fn block_tag(size: usize) -> (u64, bool) {
    match UNCOUNTED.try_with(std::cell::Cell::get) {
        Ok(Some(bytes)) => {
            UNCOUNTED.set(Some(bytes.saturating_add(size)));
            (0, false)
        }
        _ => (tag(), true),
    }
}

fn allocated(size: usize, tag: u64) {
    each(Some(tag), |c| {
        c.live += bytes(size);
        c.peak = c.peak.max(c.live);
        if !c.budget.is_null() {
            c.rise(bytes(size));
        }
    });
}

/// Memory of `size` bytes that its holder mapped itself, outside the global allocator (a
/// region's committed pages), counted as a block of generation `tag`: the generation
/// [`tag`] gave when the holder was made, as a [`Reservation`](crate::Reservation) records
/// its own, so that a measurement counts all of the holder's memory and charges, or none.
pub(crate) fn mapped(size: usize, tag: u64) {
    allocated(size, tag);
}

/// Memory [`mapped`] with generation `tag`, unmapped.
pub(crate) fn unmapped(size: usize, tag: u64) {
    freed(size, tag);
}

fn freed(size: usize, tag: u64) {
    each(Some(tag), |c| {
        c.live -= bytes(size);
        c.balance -= bytes(size);
    });
}

/// A reservation of generation `tag` charged `size` bytes of `budget`.
pub(crate) fn reserved(budget: &Budget, tag: u64, size: usize) {
    each(Some(tag), |c| {
        if within(c.budget, budget) {
            c.balance -= bytes(size);
        }
    });
}

/// A reservation of generation `tag` released `size` bytes of `budget`.
pub(crate) fn unreserved(budget: &Budget, tag: u64, size: usize) {
    each(Some(tag), |c| {
        if within(c.budget, budget) {
            c.rise(bytes(size));
        }
    });
}

/// `budget` itself charged `size` bytes ([`Budget::charge`]).
pub(crate) fn charged(budget: &Budget, size: usize) {
    each(None, |c| {
        if within(c.budget, budget) {
            c.balance -= bytes(size);
            c.own += bytes(size);
        }
    });
}

/// `budget` itself released `size` bytes ([`Budget::release`]).
pub(crate) fn released(budget: &Budget, size: usize) {
    each(None, |c| {
        if within(c.budget, budget) {
            let counted = c.own.min(bytes(size));
            c.own -= counted;
            c.rise(counted);
        }
    });
}

/// The bytes before each block: its generation, and room to keep the block aligned.
fn header(layout: Layout) -> usize {
    layout.align().max(size_of::<u64>())
}

/// `layout` with its header, or `None` if that is too large.
fn with_header(layout: Layout) -> Option<(Layout, usize)> {
    let header = header(layout);
    let size = layout.size().checked_add(header)?;
    Some((Layout::from_size_align(size, layout.align()).ok()?, header))
}

/// The generation in the header of the block at `pointer`.
///
/// # Safety
/// `pointer` is a block of this allocator.
unsafe fn tag_of(pointer: *mut u8) -> u64 {
    // SAFETY: the header's last 8 bytes, written when the block was allocated.
    unsafe { pointer.sub(size_of::<u64>()).cast::<u64>().read_unaligned() }
}

/// `base`, a block from `A` with `header` bytes for this allocator, marked with `tag` and
/// handed out past its header.
///
/// # Safety
/// `base` is null or a block of at least `header` bytes.
unsafe fn handed_out(base: *mut u8, header: usize, tag: u64) -> *mut u8 {
    if base.is_null() {
        return base;
    }
    // SAFETY: within the block; `header` is at least 8.
    unsafe {
        let pointer = base.add(header);
        pointer
            .sub(size_of::<u64>())
            .cast::<u64>()
            .write_unaligned(tag);
        pointer
    }
}

// SAFETY: every block is `A`'s, with the header added to its layout (same alignment) and
// handed out past it; the counting touches only atomics and the slots (no allocation).
unsafe impl<A: GlobalAlloc> GlobalAlloc for Measured<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some((outer, header)) = with_header(layout) else {
            return std::ptr::null_mut();
        };
        let (tag, counted) = block_tag(layout.size());
        // SAFETY: a non-zero size; the block has room for its header.
        let pointer = unsafe { handed_out(self.0.alloc(outer), header, tag) };
        if counted && !pointer.is_null() {
            allocated(layout.size(), tag);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let Some((outer, header)) = with_header(layout) else {
            return std::ptr::null_mut();
        };
        let (tag, counted) = block_tag(layout.size());
        // SAFETY: as for `alloc`.
        let pointer = unsafe { handed_out(self.0.alloc_zeroed(outer), header, tag) };
        if counted && !pointer.is_null() {
            allocated(layout.size(), tag);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let (outer, header) = with_header(layout).expect("the layout it was allocated with");
        // SAFETY: a block of this allocator, allocated with `layout`.
        unsafe {
            freed(layout.size(), tag_of(pointer));
            self.0.dealloc(pointer.sub(header), outer);
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let (outer, header) = with_header(layout).expect("the layout it was allocated with");
        let Some(grown) = size.checked_add(header) else {
            return std::ptr::null_mut();
        };
        // SAFETY: a block of this allocator, allocated with `layout`; its header moves with
        // it and is marked anew.
        unsafe {
            let was = tag_of(pointer);
            let (tag, counted) = block_tag(size);
            let moved = handed_out(
                self.0.realloc(pointer.sub(header), outer, grown),
                header,
                tag,
            );
            if !moved.is_null() {
                if counted {
                    allocated(size, tag);
                }
                freed(layout.size(), was);
            }
            moved
        }
    }
}

/// The libraries' state made once, or in steps, that a measurement would otherwise count,
/// made before it starts. Without it, the first measurement whose work a pool thread waits
/// in counted parking_lot's waiter table (93 KB here), and one in 63 that enter the global
/// pool from outside counted its queue's next block (1,520 bytes; 47 of 3,000 measurements
/// of an empty parallel loop):
/// - parking_lot's table, which grows with the threads that park at once and never shrinks:
///   once per process, before the first measurement, threads at least twice the machine's
///   and 8 more park at once;
/// - the global pool's threads, and the pool's the first measurement runs on, each of which
///   allocates its state as it starts (see "Known limits"): once per process, before the
///   first measurement, each has run a broadcast that only counts, the global pool's sent
///   from outside any pool, the caller waiting without running any job, for at most 4
///   seconds a pool (the joins below wait without a bound: "Known limits");
/// - rayon's global queue, which allocates its next block on the 63rd entry from outside
///   the pool: before each measurement made outside the pool (nested ones too), empty
///   entries until one allocates its block (at most 63, about a millisecond), uncounted by
///   any measurement, leaving 62 the work makes without allocating. Work that enters the
///   global pool from outside more often, or a pool of its own from outside, still
///   allocates such blocks: one per 63 entries.
fn prepare() {
    require_measured();
    // Under Miri, which checks what the work does to memory and not what it counts (and
    // rejects parking_lot's futex call), there is nothing to prepare.
    if cfg!(miri) {
        return;
    }
    static WARM: std::sync::Once = std::sync::Once::new();
    if ACTIVE.load(SeqCst) == 0 {
        WARM.call_once(|| {
            let threads = std::thread::available_parallelism().map_or(8, |n| 2 * n.get() + 8);
            let all = std::sync::Barrier::new(threads);
            std::thread::scope(|scope| {
                for _ in 0..threads {
                    scope.spawn(|| {
                        // SAFETY: parks nowhere (its validation fails): the thread's waiter
                        // data is made, and the table sized for it.
                        unsafe {
                            parking_lot_core::park(
                                std::ptr::from_ref(&all).addr(),
                                || false,
                                || {},
                                |_, _| {},
                                parking_lot_core::DEFAULT_PARK_TOKEN,
                                None,
                            );
                        }
                        all.wait();
                    });
                }
            });
            // Every thread of the global pool started, and of the pool the caller is on (a
            // thread allocates as it starts: "Known limits"). The wait runs no job on the
            // caller: a job it ran could measure, entering this preparation again, or wait
            // for a lock the caller holds (Sol, measure-pool r2). So each pool gets a
            // broadcast that only counts, sent without waiting, and the caller waits for the
            // count (`started`). The global pool's is sent from outside any pool, as a
            // broadcast reaches the pool it is made on.
            let in_pool = rayon::current_thread_index().is_some();
            let global = Arc::new(AtomicUsize::new(0));
            let threads = if in_pool {
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| counted_broadcast(&global))
                        .join()
                        .expect("the broadcast is sent")
                })
            } else {
                counted_broadcast(&global)
            };
            started(&global, threads);
            if in_pool {
                let own = Arc::new(AtomicUsize::new(0));
                let threads = counted_broadcast(&own);
                started(&own, threads);
            } else {
                // The entering thread's state, made: outside any pool, nothing it runs.
                rayon::join(|| (), || ());
            }
        });
    }
    if rayon::current_thread_index().is_none() {
        for _ in 0..=63 {
            UNCOUNTED.set(Some(0));
            rayon::join(|| (), || ());
            if UNCOUNTED.replace(None).is_some_and(|bytes| bytes > 0) {
                break;
            }
        }
    }
}

/// Sends every thread of the current pool (the global one outside any pool) a job that only
/// counts into `count`, without waiting for it; the pool's threads.
fn counted_broadcast(count: &Arc<AtomicUsize>) -> usize {
    let ran = Arc::clone(count);
    rayon::spawn_broadcast(move |_| {
        ran.fetch_add(1, SeqCst);
    });
    rayon::current_num_threads()
}

/// Waits, running nothing, until `count` says each of `threads` has run its job: all but
/// one for at most 3 seconds, then the last for at most 1 more. The caller's own thread, if
/// it is one of them, runs its job only once it is back in its pool, so it waits for itself
/// that 1 second; a thread that doesn't come within the time (busy for longer, so started)
/// doesn't hold it either.
fn started(count: &AtomicUsize, threads: usize) {
    use std::time::{Duration, Instant};
    let wait = |least: usize, most: Duration| {
        let since = Instant::now();
        while count.load(SeqCst) < least && since.elapsed() < most {
            std::thread::yield_now();
        }
    };
    wait(threads.saturating_sub(1), Duration::from_secs(3));
    wait(threads, Duration::from_secs(1));
}

/// A running measurement; it ends when this drops, whether the measurement returns or
/// unwinds.
struct Running(&'static Slot);

impl Running {
    fn start(budget: Option<&Budget>) -> Self {
        while STARTING
            .compare_exchange_weak(false, true, SeqCst, SeqCst)
            .is_err()
        {
            spin_loop();
        }
        let slot = RUNNING.iter().find(|s| {
            s.taken
                .compare_exchange(false, true, SeqCst, SeqCst)
                .is_ok()
        });
        let Some(slot) = slot else {
            STARTING.store(false, SeqCst);
            panic!("at most {SLOTS} measurements at once");
        };
        let since = GENERATION.load(SeqCst) + 1;
        slot.with(|_, c| {
            *c = Counts {
                budget: budget.map_or(std::ptr::null(), std::ptr::from_ref),
                ..Counts::NONE
            };
            slot.since.store(since, SeqCst);
        });
        ACTIVE.fetch_add(1, SeqCst);
        // Running, then its generation published: a block or reservation that records it
        // is counted from the start.
        GENERATION.store(since, SeqCst);
        STARTING.store(false, SeqCst);
        Self(slot)
    }

    /// Ends the measurement: its counts, which no event changes after.
    fn end(&self) -> Counts {
        self.0.with(|_, c| {
            self.0.since.store(0, SeqCst);
            *c
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.end();
        ACTIVE.fetch_sub(1, SeqCst);
        self.0.taken.store(false, SeqCst);
    }
}

fn reported(count: i128) -> usize {
    usize::try_from(count.max(0)).unwrap_or(usize::MAX)
}

/// `work`'s result, and the most bytes it held at once: allocated while it ran and not
/// yet freed. Without [`Measured`] as the global allocator, it panics. Its preparation can
/// wait for the global pool: mind the rule for measured tests in "Known limits".
pub fn measure<T>(work: impl FnOnce() -> T) -> (T, usize) {
    prepare();
    let running = Running::start(None);
    let result = work();
    (result, reported(running.end().peak))
}

/// `work`'s result, and the most bytes it held beyond what it reserved of `budget` (or a
/// part of it) at any of its events, 0 if never beyond: what it allocated before reserving
/// it, or still held when it released the charge. As for [`measure`], mind the rule for
/// measured tests in "Known limits".
pub fn measure_within<T>(budget: &Budget, work: impl FnOnce() -> T) -> (T, usize) {
    prepare();
    let running = Running::start(Some(budget));
    let result = work();
    (result, reported(running.end().excess))
}

/// The variable that tells a test process it runs one test alone.
const ALONE: &str = "NRESE_MEASURE_ALONE";

/// The arguments [`alone`] runs `test` with, in a child of its own: that test alone, on one
/// test thread (the harness's main thread).
fn alone_args(test: &str) -> [&str; 5] {
    [
        test,
        "--exact",
        "--include-ignored",
        "--test-threads=1",
        "--nocapture",
    ]
}

/// Whether the calling test runs alone in its process, as a measurement needs: only in the
/// process this starts. Elsewhere it runs the test again, alone (ignored or not), on the
/// main thread of a child process of this binary (`--test-threads=1`) and ends as it did
/// there: a pass returns false (the caller returns), a failure fails, and a `should_panic`
/// test that panicked there panics with its message. One that didn't panic there ends this
/// binary with a failure, as a panic would pass it. The test is the calling thread's, which
/// the test harness names after it; what the run printed is printed (where the harness
/// shows it).
///
/// Under cargo-nextest too, though it runs a process per test: there the harness runs the
/// test on a thread of its own, and its main thread, past 60 seconds, writes that the test
/// is running long, allocating into the measurement (594 bytes counted, 20 Oct, a guard
/// slowed past a minute by a loaded machine). On the main thread nothing else runs.
pub fn alone() -> bool {
    require_measured();
    // Miri can't start a process; it runs the tests it is given one at a time.
    if cfg!(miri) {
        return true;
    }
    let current = std::thread::current();
    let test = current.name().expect("a test thread, named after its test");
    // The child [`alone`] started: the marker names this test, and the process was started
    // with exactly the arguments that run it alone on one thread. A marker inherited from
    // elsewhere, in a run of other arguments, is no such child.
    let started_alone = || std::env::args().skip(1).eq(alone_args(test));
    if std::env::var(ALONE).is_ok_and(|t| t == test) && started_alone() {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(alone_args(test))
        .env(ALONE, test)
        .output()
        .expect("the test binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprint!("{stdout}{stderr}");
    let passed = output.status.success() && stdout.contains("test result: ok. 1 passed");
    if stdout.contains(&format!("test {test} - should panic ...")) {
        if !passed {
            eprintln!("{test}, run alone, did not panic");
            std::process::exit(101);
        }
        // The panic's message: what follows "panicked at <place>:" up to the note after it.
        let message = stderr
            .split_once(" panicked at ")
            .and_then(|(_, rest)| rest.split_once('\n'))
            .map_or("", |(_, message)| {
                message.split("\nnote:").next().unwrap_or(message)
            });
        panic!("{message}");
    }
    assert!(passed, "{test}, run alone, failed");
    false
}

#[cfg(test)]
mod tests;
