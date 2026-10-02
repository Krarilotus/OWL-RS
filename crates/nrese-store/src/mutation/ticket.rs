use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use nrese_sparql::CancellationToken;

const PENDING: u8 = 0;
const COMMITTING: u8 = 1;
const CANCELLED: u8 = 2;

/// Decides the race between "the caller gives up" and "the pipeline commits".
///
/// Exactly one of [`cancel`](Self::cancel) and the pipeline's commit step wins. A caller
/// that gets `true` from `cancel` knows the mutation will never be committed and may report
/// a timeout. A caller that gets `false` knows the commit is already underway and must wait
/// for the result instead of reporting a timeout. This closes the v1 defect where a 408
/// response could be followed by a successful commit.
///
/// A successful cancel also cancels the ticket's evaluation token, so a long-running
/// `WHERE` clause stops early instead of running to completion unobserved.
#[derive(Clone, Default)]
pub struct MutationTicket {
    state: Arc<AtomicU8>,
    evaluation: CancellationToken,
}

impl std::fmt::Debug for MutationTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutationTicket")
            .field("state", &self.state.load(Ordering::Acquire))
            .finish()
    }
}

impl MutationTicket {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels the mutation unless its commit has already started.
    /// Returns `true` if the mutation is (now) guaranteed not to be committed.
    pub fn cancel(&self) -> bool {
        match self
            .state
            .compare_exchange(PENDING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
        {
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
        self.state.load(Ordering::Acquire) == CANCELLED
    }

    /// Claims the right to commit. Returns `false` if the caller cancelled first.
    pub(crate) fn begin_commit(&self) -> bool {
        self.state
            .compare_exchange(PENDING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::MutationTicket;

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
}
