//! The writer slot's guard as the dictionary takes it: proof that its holder may add entries
//! (the lifecycle contract's one appender, `notes/2026-11-08-dictionary-lifecycle-contract.md`
//! P1). The engine holds the slot ([`crate::Engine::transaction`] and the bulk and replica
//! paths); the dictionary, a layer below, only asks for the proof.
//!
//! It proves that a writer slot is held, not that it is this dictionary's engine's: one engine
//! owns one dictionary, and only engine code holds either.

use parking_lot::MutexGuard;

/// A held writer slot.
pub(crate) struct WriterGuard<'s> {
    _held: MutexGuard<'s, ()>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the engine's publishers take it in package 9 (activation)"
    )
)]
impl<'s> WriterGuard<'s> {
    /// The guard of the engine's writer slot, held.
    pub(crate) fn hold(guard: MutexGuard<'s, ()>) -> Self {
        Self { _held: guard }
    }
}

std::thread_local! {
    /// Whether [`warm_parking`] ran on this thread.
    static WARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes what parking needs on this thread, so that a later wait for a lock here allocates
/// nothing: `parking_lot_core`'s per-thread state (its `ThreadData`, the table growth that
/// registers it, a parker), made on the first park of a thread (package 1 review r1 F1). A wait
/// with a timeout already passed parks for real and returns at once. Once per thread; the state
/// lasts until the thread ends.
///
/// A publisher calls it before its durable step ([`super::Dictionary::prepare`]); its install,
/// and the locks it takes after the step, then wait on this thread with nothing to allocate.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "publications prepare with it in package 9 (activation)"
    )
)]
pub(crate) fn warm_parking() {
    if WARMED.with(std::cell::Cell::get) {
        return;
    }
    let mutex = parking_lot::Mutex::new(());
    let condvar = parking_lot::Condvar::new();
    let mut held = mutex.lock();
    let _ = condvar.wait_for(&mut held, std::time::Duration::ZERO);
    WARMED.with(|warmed| warmed.set(true));
}
