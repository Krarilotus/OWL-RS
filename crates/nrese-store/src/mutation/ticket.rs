use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use nrese_engine::{CommitSummary, EngineError};
use nrese_sparql::CancellationToken;

use super::{MutationCommitReport, MutationError};

type RightsHeld<'a> = std::sync::RwLockReadGuard<'a, Arc<crate::access::AccessState>>;

const PENDING: u8 = 0;
const COMMITTING: u8 = 1;
const CANCELLED: u8 = 2;
const ENTERED: u8 = 3;
const ENDED: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationState {
    Pending,
    Cancelled,
    Committing,
    Entered,
    Published,
    Ended,
}

#[derive(Default)]
struct Record {
    state: AtomicU8,
    published: OnceLock<MutationCommitReport>,
}

/// Records the race between a cancellation, a known error and the claim to commit,
/// and records publication before the pipeline starts its post-commit work.
///
/// A successful cancel also cancels the ticket's evaluation token, so a long-running
/// `WHERE` clause stops early instead of running to completion unobserved.
#[derive(Clone, Default)]
pub struct MutationTicket {
    record: Arc<Record>,
    evaluation: CancellationToken,
    /// The write's rights, checked again where it publishes (none: the store's owner's).
    rights: Option<crate::access::RightsCheck>,
}

impl std::fmt::Debug for MutationTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutationTicket")
            .field("state", &self.state())
            .finish()
    }
}

impl MutationTicket {
    pub fn new() -> Self {
        Self::default()
    }

    /// The ticket of a write that publishes only while `rights` still hold.
    pub fn checking(rights: Option<crate::access::RightsCheck>) -> Self {
        Self {
            rights,
            ..Self::default()
        }
    }

    /// The access state's read guard while the write's rights hold, for its publication.
    pub(crate) fn hold_rights(&self) -> Result<Option<RightsHeld<'_>>, super::MutationError> {
        self.rights
            .as_ref()
            .map(|rights| rights.hold().ok_or(super::MutationError::RightsChanged))
            .transpose()
    }

    /// Cancels a pending mutation. A previous cancellation stays in force.
    /// Returns `true` if the mutation is (now) guaranteed not to be committed.
    pub fn cancel(&self) -> bool {
        match self.record.state.compare_exchange(
            PENDING,
            CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.evaluation.cancel();
                true
            }
            Err(current) => current == CANCELLED,
        }
    }

    /// Token checked by the SPARQL evaluator while the mutation is planned.
    pub(crate) fn evaluation_token(&self) -> &CancellationToken {
        &self.evaluation
    }

    pub fn is_cancelled(&self) -> bool {
        self.record.state.load(Ordering::Acquire) == CANCELLED
    }

    /// Claims the right to commit. Returns `false` if the ticket is no longer pending.
    pub(crate) fn begin_commit(&self) -> bool {
        self.record
            .state
            .compare_exchange(PENDING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn end_error(&self, error: MutationError) -> MutationError {
        if self.end_pending() == MutationState::Cancelled {
            MutationError::Cancelled
        } else {
            error
        }
    }

    /// Settles a known failure or a fallback that could not have published. Returns
    /// the state that determines the answer, including a cancel that settled it first.
    pub fn end_pending(&self) -> MutationState {
        if self
            .record
            .state
            .compare_exchange(PENDING, ENDED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            MutationState::Pending
        } else {
            self.state()
        }
    }

    /// The record at this instant, readable without taking a lock.
    fn state(&self) -> MutationState {
        match self.record.state.load(Ordering::Acquire) {
            PENDING => MutationState::Pending,
            CANCELLED => MutationState::Cancelled,
            COMMITTING => MutationState::Committing,
            ENTERED if self.published().is_some() => MutationState::Published,
            ENTERED => MutationState::Entered,
            _ => MutationState::Ended,
        }
    }

    /// The original report, including the revision published by this mutation.
    pub fn published(&self) -> Option<&MutationCommitReport> {
        self.record.published.get()
    }
}

/// The caller holds the commit claim. Store the report as soon as the commit returns Ok,
/// before the caller can run any maintenance.
pub(super) fn publishing(
    ticket: &MutationTicket,
    report: MutationCommitReport,
    commit: impl FnOnce() -> Result<CommitSummary, EngineError>,
) -> Result<(CommitSummary, &MutationCommitReport), EngineError> {
    ticket.record.state.store(ENTERED, Ordering::Release);
    let summary = commit()?;
    let report = ticket
        .record
        .published
        .get_or_init(|| report.committed(summary.revision));
    Ok((summary, report))
}

#[cfg(test)]
mod tests {
    use super::{MutationState, MutationTicket};

    #[test]
    fn cancel_before_commit_wins() {
        let ticket = MutationTicket::new();
        assert!(ticket.cancel());
        assert!(!ticket.begin_commit());
        assert!(ticket.cancel(), "cancel stays idempotent");
    }

    #[test]
    fn commit_before_cancel_wins() {
        let ticket = MutationTicket::new();
        assert!(ticket.begin_commit());
        assert!(!ticket.cancel());
        assert!(!ticket.is_cancelled());
    }

    // Spec: lifecycle rule 3 says a cancel and a claim have exactly one winner.
    #[test]
    fn cancel_and_commit_racing_have_one_winner() {
        let ticket = MutationTicket::new();
        let gate = std::sync::Barrier::new(2);
        let (claimed, cancelled) = std::thread::scope(|threads| {
            let cancel = threads.spawn(|| {
                gate.wait();
                ticket.cancel()
            });
            gate.wait();
            (ticket.begin_commit(), cancel.join().unwrap())
        });
        assert_ne!(claimed, cancelled);
    }

    // Spec: lifecycle rule 3 says a fallback and a racing cancel agree on the recorded end.
    #[test]
    fn pending_fallback_and_cancel_agree_on_the_winner() {
        let ticket = MutationTicket::new();
        let gate = std::sync::Barrier::new(2);
        let (state, cancelled) = std::thread::scope(|threads| {
            let cancel = threads.spawn(|| {
                gate.wait();
                ticket.cancel()
            });
            gate.wait();
            (ticket.end_pending(), cancel.join().unwrap())
        });
        assert_eq!(
            state,
            if cancelled {
                MutationState::Cancelled
            } else {
                MutationState::Pending
            }
        );
    }
}
