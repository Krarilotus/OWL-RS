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

/// Releases what every pool thread and the calling thread hold unused: at the end of a
/// phase, and within one whose memory other threads freed.
pub fn release_all() {
    if let Some(&release) = RELEASE.get() {
        rayon::broadcast(|_| release(true));
        release(true);
    }
}
