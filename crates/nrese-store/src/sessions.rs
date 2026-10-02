//! Client transactions: writes a client collects over several requests and commits as one
//! (RDF4J's `/transactions`; the console and other connectors can use the same). A session
//! holds the statement operations ([`StatementOp`]) in order; reads inside it see the
//! store as the operations would leave it ([`crate::StoreService::read_statements_pending`]
//! and the other `*_pending` reads); its commit goes through the mutation pipeline like
//! any write. Sessions untouched for [`Sessions::idle`] are dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use std::sync::{Mutex, PoisonError};

use crate::statements::{StatementOp, StatementsRequest};

/// How long an untouched session lives.
pub const SESSION_IDLE: Duration = Duration::from_secs(600);

struct Session {
    ops: Vec<StatementOp>,
    touched: Instant,
}

/// The open sessions of one store.
pub struct Sessions {
    next: AtomicU64,
    open: Mutex<HashMap<String, Session>>,
    idle: Duration,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field(
                "open",
                &self
                    .open
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .len(),
            )
            .finish()
    }
}

impl Default for Sessions {
    fn default() -> Self {
        Self::with_idle(SESSION_IDLE)
    }
}

impl Sessions {
    pub fn with_idle(idle: Duration) -> Self {
        Self {
            next: AtomicU64::new(1),
            open: Mutex::default(),
            idle,
        }
    }

    /// How long an untouched session lives.
    pub fn idle(&self) -> Duration {
        self.idle
    }

    /// Opens a session; returns its id. Sessions idle for too long are dropped first.
    pub fn begin(&self) -> String {
        let id = format!("tx-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let idle = self.idle;
        open.retain(|_, session| session.touched.elapsed() < idle);
        open.insert(
            id.clone(),
            Session {
                ops: Vec::new(),
                touched: Instant::now(),
            },
        );
        id
    }

    /// Appends `ops` to session `id`; `false` if there is no such session.
    pub fn add(&self, id: &str, ops: Vec<StatementOp>) -> bool {
        match self
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(id)
        {
            Some(session) => {
                session.ops.extend(ops);
                session.touched = Instant::now();
                true
            }
            None => false,
        }
    }

    /// Keeps session `id` alive; `false` if there is no such session.
    pub fn ping(&self, id: &str) -> bool {
        match self
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(id)
        {
            Some(session) => {
                session.touched = Instant::now();
                true
            }
            None => false,
        }
    }

    /// Session `id`'s operations so far (for reads inside it), if it is open.
    pub fn pending(&self, id: &str) -> Option<Vec<StatementOp>> {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let session = open.get_mut(id)?;
        session.touched = Instant::now();
        Some(session.ops.clone())
    }

    /// Closes session `id` and returns its operations to commit, if it was open.
    pub fn take(&self, id: &str) -> Option<StatementsRequest> {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
            .map(|session| StatementsRequest {
                ops: session.ops,
                writable: None,
            })
    }

    /// Closes session `id` without committing; whether it was open.
    pub fn rollback(&self, id: &str) -> bool {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
            .is_some()
    }

    /// The number of open sessions (idle ones included until the next `begin`).
    pub fn len(&self) -> usize {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statements::StatementPattern;

    #[test]
    fn sessions_collect_operations_until_commit_or_rollback() {
        let sessions = Sessions::default();
        let id = sessions.begin();
        assert!(sessions.add(
            &id,
            vec![StatementOp::RemoveMatching(StatementPattern::default())]
        ));
        assert_eq!(sessions.pending(&id).map(|ops| ops.len()), Some(1));
        let other = sessions.begin();
        assert_ne!(id, other);
        assert_eq!(sessions.take(&id).map(|request| request.ops.len()), Some(1));
        assert!(sessions.take(&id).is_none());
        assert!(sessions.rollback(&other));
        assert!(!sessions.add(&other, Vec::new()));
        // Idle sessions go when the next one begins.
        let short = Sessions::with_idle(Duration::ZERO);
        let gone = short.begin();
        short.begin();
        assert!(!short.ping(&gone));
    }
}
