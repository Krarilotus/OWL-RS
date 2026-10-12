//! Heap profiles by phase: where a long operation's memory peaks, and what it holds
//! between its steps. A binary that wants a profile installs [`Counting`] as its global
//! allocator and calls [`start`] (the memory guards of `docs/design/performance.md` §0,
//! and the perf lab built with `--cfg alloc_profile`); long operations name their phases
//! with [`phase`], which costs an atomic load where nothing counts.
//!
//! The counts are bytes requested from the allocator, not what the system commits: exact
//! and, on one thread, deterministic, so a guard can compare them. A peak over the whole
//! process depends on how far other threads got meanwhile; what one thread's step holds
//! doesn't: [`begin_step`] and [`end_step`] record it for that thread alone ([`Step`]).

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A global allocator that counts the bytes live and the most live since the last phase
/// began, and forwards every call to `A`.
pub struct Counting<A>(pub A);

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ON: AtomicBool = AtomicBool::new(false);
static PHASES: Mutex<Phases> = Mutex::new(Phases {
    current: None,
    log: Vec::new(),
    steps: Vec::new(),
});

struct Phases {
    current: Option<&'static str>,
    log: Vec<Phase>,
    steps: Vec<Step>,
}

/// One thread's count: what it allocated beyond what it freed (memory another thread
/// allocated and this one frees lowers it), and the most of that since its step began.
struct Thread {
    net: Cell<isize>,
    peak: Cell<isize>,
    mark: Cell<isize>,
}

thread_local! {
    // Constant and without a destructor: the allocator may read it at any time.
    static THREAD: Thread = const {
        Thread {
            net: Cell::new(0),
            peak: Cell::new(0),
            mark: Cell::new(0),
        }
    };
}

/// One step of one thread ([`end_step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub label: &'static str,
    /// The most bytes the thread held during the step beyond what it held when the step
    /// began, by its own allocations and frees: whatever other threads did meanwhile.
    pub peak: usize,
}

/// One phase of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phase {
    pub label: &'static str,
    /// The most bytes live at once during the phase.
    pub peak: usize,
    /// The bytes live when it ended.
    pub live_after: usize,
}

// SAFETY: every call is forwarded unchanged to `A`; the counters only observe.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        // SAFETY: forwarded unchanged; the caller upholds `alloc_zeroed`'s contract.
        unsafe { self.0.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrink(layout.size());
        // SAFETY: `ptr` came from this allocator with `layout`, as the caller guarantees.
        unsafe { self.0.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Counted as the allocator may do it: the new block while the old one is live.
        grow(new_size);
        shrink(layout.size());
        // SAFETY: forwarded unchanged; the caller upholds `realloc`'s contract.
        unsafe { self.0.realloc(ptr, layout, new_size) }
    }
}

fn grow(size: usize) {
    let now = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(now, Ordering::Relaxed);
    let _ = THREAD.try_with(|thread| {
        let net = thread.net.get() + size as isize;
        thread.net.set(net);
        thread.peak.set(thread.peak.get().max(net));
    });
}

fn shrink(size: usize) {
    LIVE.fetch_sub(size, Ordering::Relaxed);
    let _ = THREAD.try_with(|thread| thread.net.set(thread.net.get() - size as isize));
}

/// The bytes live now (0 without [`Counting`]).
pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// The most bytes live at once since the current phase began, or since the start where no
/// profile runs (0 without [`Counting`]).
pub fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Starts a profile: phases are recorded from here on, the first one named `label`.
pub fn start(label: &'static str) {
    let mut phases = PHASES.lock().unwrap_or_else(|p| p.into_inner());
    phases.log.clear();
    phases.steps.clear();
    phases.current = Some(label);
    PEAK.store(live(), Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
}

/// Ends the current phase and begins one named `label` (nothing unless a profile was
/// started).
pub fn phase(label: &'static str) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let mut phases = PHASES.lock().unwrap_or_else(|p| p.into_inner());
    let live = live();
    let peak = PEAK.swap(live, Ordering::Relaxed);
    if let Some(previous) = phases.current.replace(label) {
        phases.log.push(Phase {
            label: previous,
            peak,
            live_after: live,
        });
    }
}

/// Ends the profile and returns its phases in order.
pub fn finish() -> Vec<Phase> {
    phase("");
    ON.store(false, Ordering::Relaxed);
    let mut phases = PHASES.lock().unwrap_or_else(|p| p.into_inner());
    phases.current = None;
    std::mem::take(&mut phases.log)
}

/// Begins a step on the calling thread ([`end_step`]). A thread-local write.
pub fn begin_step() {
    let _ = THREAD.try_with(|thread| {
        thread.mark.set(thread.net.get());
        thread.peak.set(thread.net.get());
    });
}

/// Ends the calling thread's step and records it under `label` (nothing unless a profile
/// was started).
pub fn end_step(label: &'static str) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let peak = THREAD
        .try_with(|thread| (thread.peak.get() - thread.mark.get()).max(0) as usize)
        .unwrap_or(0);
    let mut phases = PHASES.lock().unwrap_or_else(|p| p.into_inner());
    phases.steps.push(Step { label, peak });
}

/// The steps recorded since the profile started, in the order they ended.
pub fn steps() -> Vec<Step> {
    let phases = PHASES.lock().unwrap_or_else(|p| p.into_inner());
    phases.steps.clone()
}
