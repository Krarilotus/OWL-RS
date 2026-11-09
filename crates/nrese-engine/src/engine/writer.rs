//! What the holder of the engine's writer slot can show and do beside its commit: a proof that
//! it holds the slot ([`WriterHeld`]), and a note of the commit in a journal of the caller's
//! ([`CommitNote`]), written at the one point where the commit's revision is fixed and nothing
//! of its record is in the log yet.

use std::marker::PhantomData;

use parking_lot::MutexGuard;

use super::CommitSummary;

/// Proof that the writer slot is held, for as long as the borrow it came from lasts: only a
/// holder of the slot makes one ([`Transaction::writer`](super::Transaction::writer), except
/// for a speculative transaction; [`BulkLoad::writer`](super::BulkLoad::writer);
/// [`Rematerialisation::writer`](super::Rematerialisation::writer); [`Finished::writer`]). State
/// that must change only between commits (a store's reasoning certificate) takes one. The slot
/// isn't reentrant: under one, take nothing that waits for the slot.
#[derive(Debug, Clone, Copy)]
pub struct WriterHeld<'a> {
    _slot: PhantomData<&'a ()>,
}

impl WriterHeld<'_> {
    pub(super) fn new() -> Self {
        Self { _slot: PhantomData }
    }
}

/// A note of a commit in a journal beside the store's log (a store's quarantine resolutions),
/// given to [`Transaction::note_commit`](super::Transaction::note_commit). Called with the
/// revision the commit publishes.
pub type CommitNote<'e> = Box<dyn FnOnce(u64) -> Result<(), NoteError> + Send + Sync + 'e>;

/// Why a [`CommitNote`] failed, as its journal knows it.
#[derive(Debug)]
pub enum NoteError {
    /// The journal is as it was: the commit is refused with nothing of it written
    /// ([`EngineError::CommitNoteRefused`](crate::EngineError::CommitNoteRefused)), and the log
    /// takes the next commit at the same revision.
    Refused(std::io::Error),
    /// The journal may name the revision: the commit is refused and the log poisoned, as after
    /// a failed write of its record, so that no other commit takes the revision the journal may
    /// name ([`EngineError::Io`](crate::EngineError::Io)).
    Unknown(std::io::Error),
}

/// A published rematerialisation that still holds the writer slot
/// ([`Rematerialisation::finish_held`](super::Rematerialisation::finish_held)): what certifies
/// it happens before any other write. Dropping it lets the slot go.
pub struct Finished<'e> {
    summary: CommitSummary,
    /// Last, so that it drops after everything else.
    _slot: MutexGuard<'e, ()>,
}

impl<'e> Finished<'e> {
    pub(super) fn new(summary: CommitSummary, slot: MutexGuard<'e, ()>) -> Self {
        Self {
            summary,
            _slot: slot,
        }
    }

    /// The revision published and the inferred statements added and removed.
    pub fn summary(&self) -> CommitSummary {
        self.summary
    }

    pub fn writer(&self) -> WriterHeld<'_> {
        WriterHeld::new()
    }
}
