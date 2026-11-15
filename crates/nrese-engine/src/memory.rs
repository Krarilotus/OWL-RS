//! Giving memory back to the system after the heavy phases: bulk loads and
//! rematerialisation.
//!
//! Allocators such as mimalloc keep what a thread frees for that thread's later
//! allocations, and give it back only when the thread allocates again. A pool thread that
//! goes idle after a load keeps it for good: on DBpedia (67 M quads) 5.6 GB stayed
//! resident after the load that the store no longer used, against 0.1 GB once released.
//! Releasing on every free instead (mimalloc's `purge_delay=0`) slowed query sets by
//! 8-25%, so the engine and the store release during and at the end of those phases.
//!
//! The binary that picks the allocator registers how to release ([`set_release`]); without
//! it, nothing happens.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Gives back what the calling thread's allocator holds unused; `true`: all of it now.
pub type Release = fn(force: bool);

static RELEASE: OnceLock<Release> = OnceLock::new();

/// Registers the allocator's release (once per process; later calls are ignored).
pub fn set_release(release: Release) {
    let _ = RELEASE.set(release);
}

/// Releases what the calling thread holds unused: in a long phase on a pool thread.
pub fn release_thread() {
    if let Some(release) = RELEASE.get() {
        release(true);
    }
}

/// Whether a materialisation also releases between its rounds ([`release_between_rounds`]).
static BETWEEN_ROUNDS: AtomicBool = AtomicBool::new(false);

/// Sets whether a materialisation releases between its rounds, after the last one and before
/// the store builds the inferred stack (`budgets.reasoning_release`; off by default). On, its
/// resident peak is lower and steadier: mimalloc keeps a freed large block 100 ms, about a
/// round, so whether one round's freed buffers still count when the next allocates depends on
/// timing. It costs time, as the memory given back is soon taken and faulted in again:
/// LUBM 100's peak 2,074 → 1,776 MiB for 5.6 % more reasoning time, RL-1 −25 % for 6.6 %,
/// commits-rdfs's reasoning peak −28 % for 14 % (office-pc, 13 Oct 2026). A thread of its
/// own halves the cost and keeps most of the memory; no variant measured was free.
pub fn set_release_between_rounds(on: bool) {
    BETWEEN_ROUNDS.store(on, Ordering::Relaxed);
}

/// Releases what every thread holds unused, where [`set_release_between_rounds`] asks for it:
/// between a materialisation's rounds.
pub fn release_between_rounds() {
    if BETWEEN_ROUNDS.load(Ordering::Relaxed) {
        release_all();
    }
}

/// Releases what every pool thread and the calling thread hold unused: at the end of a
/// phase, and within one whose memory other threads freed.
pub fn release_all() {
    if let Some(&release) = RELEASE.get() {
        rayon::broadcast(|_| release(true));
        release(true);
    }
}

/// Offers back, unforced, what the calling thread and every pool thread hold unused: at
/// the end of an operation that leaves the shared workers idle. Without it they keep the
/// operation's freed memory, so the next one's peak starts above it.
///
/// The calling thread releases before this returns. Each pool thread releases when it is
/// next free, not before the call returns: a worker busy with other work (another request's
/// operation, a materialisation) is not waited for, so the caller's time doesn't depend on it.
///
/// Unforced, the allocator gives back what it can without waiting: on an EL classification
/// of 20,000 classes through the endpoint the call took about 2 ms and the request's time
/// was unchanged, while a forced release ([`release_all`]) took about 8 ms and the memory it
/// gave back was soon faulted in again, a few percent more time on the request.
pub fn release_idle() {
    if let Some(&release) = RELEASE.get() {
        rayon::spawn_broadcast(move |_| release(false));
        release(false);
    }
}
