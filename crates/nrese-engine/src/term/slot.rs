//! The writer slot's guard as the dictionary takes it: proof that its holder may add entries
//! (the lifecycle contract's one appender, `notes/2026-11-08-dictionary-lifecycle-contract.md`
//! P1). The engine holds the slot ([`crate::Engine::transaction`] and the bulk and replica
//! paths); the dictionary, a layer below, only asks for the proof.
//!
//! It proves that a writer slot is held, not that it is this dictionary's engine's: one engine
//! owns one dictionary, and only engine code holds either.

use parking_lot::MutexGuard;

/// A held writer slot.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the engine's publishers take it in package 9 (activation)"
    )
)]
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
