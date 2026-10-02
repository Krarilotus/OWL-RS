//! Client transactions: writes a client collects over several requests and commits as one
//! (RDF4J's `/transactions`; the console and other connectors can use the same). A session
//! holds the statement operations ([`StatementOp`]) in order; reads inside it see the
//! store as the operations would leave it ([`crate::ReadContext::on`],
//! [`crate::StoreService::run_query_pending`]); its commit goes through the mutation
//! pipeline like any write. Sessions untouched for [`Sessions::idle`] are dropped.
//!
//! A session belongs to whoever opened it: its id is unguessable, and every other call
//! names its owner, for whom alone it exists. Otherwise anyone who learns or guesses an id
//! could add operations that its owner then commits with the owner's rights.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use nrese_engine::Snapshot;
use nrese_sparql::GraphAccess;

use crate::ReadScope;
use crate::statements::{StatementOp, StatementsRequest};

/// How long an untouched session lives.
pub const SESSION_IDLE: Duration = Duration::from_secs(600);

struct Session {
    /// Who opened it (a user name, else roles; `None` without authentication).
    owner: Option<String>,
    ops: Vec<StatementOp>,
    touched: Instant,
    /// The data as the operations leave it, kept for the next read
    /// ([`Sessions::view`]): replaying them for every read of a long transaction would
    /// take time quadratic in its length.
    view: Option<View>,
}

/// The data as a session's operations leave it, and what it was computed from.
struct View {
    /// The committed data the operations were applied to.
    base: Snapshot,
    /// How many of the session's operations (they are only ever appended).
    ops: usize,
    /// Whose: the operations are scoped by the reader's access.
    scope: ReadScope,
    writable: Option<Arc<GraphAccess>>,
    snapshot: Snapshot,
}

/// The open sessions of one store.
pub struct Sessions {
    open: Mutex<HashMap<String, Session>>,
    idle: Duration,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field("open", &self.lock().len())
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
            open: Mutex::default(),
            idle,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How long an untouched session lives.
    pub fn idle(&self) -> Duration {
        self.idle
    }

    /// Opens a session for `owner`; returns its id (128 random bits). Sessions idle for
    /// too long are dropped first.
    pub fn begin(&self, owner: Option<&str>) -> std::io::Result<String> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
        let mut id = String::from("tx-");
        for byte in bytes {
            let _ = write!(id, "{byte:02x}"); // writing to a String doesn't fail
        }
        let mut open = self.lock();
        let idle = self.idle;
        open.retain(|_, session| session.touched.elapsed() < idle);
        open.insert(
            id.clone(),
            Session {
                owner: owner.map(str::to_owned),
                ops: Vec::new(),
                touched: Instant::now(),
                view: None,
            },
        );
        Ok(id)
    }

    /// Runs `f` on session `id` if it is open and `owner`'s, touching it.
    fn with<T>(
        &self,
        id: &str,
        owner: Option<&str>,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Option<T> {
        let mut open = self.lock();
        let session = open
            .get_mut(id)
            .filter(|session| session.owner.as_deref() == owner)?;
        session.touched = Instant::now();
        Some(f(session))
    }

    /// Appends `ops` to `owner`'s session `id`; `false` if there is no such session.
    pub fn add(&self, id: &str, owner: Option<&str>, ops: Vec<StatementOp>) -> bool {
        self.with(id, owner, |session| {
            session.ops.extend(ops);
            session.view = None;
        })
        .is_some()
    }

    /// Keeps `owner`'s session `id` alive; `false` if there is no such session.
    pub fn ping(&self, id: &str, owner: Option<&str>) -> bool {
        self.with(id, owner, |_| ()).is_some()
    }

    /// The operations so far of `owner`'s session `id` (for reads inside it), if it is
    /// open.
    pub fn pending(&self, id: &str, owner: Option<&str>) -> Option<Vec<StatementOp>> {
        self.with(id, owner, |session| session.ops.clone())
    }

    /// Closes `owner`'s session `id` and returns its operations to commit, if it was open.
    pub fn take(&self, id: &str, owner: Option<&str>) -> Option<StatementsRequest> {
        let mut open = self.lock();
        open.get(id)
            .is_some_and(|session| session.owner.as_deref() == owner)
            .then(|| open.remove(id))
            .flatten()
            .map(|session| StatementsRequest {
                ops: session.ops,
                ..StatementsRequest::default()
            })
    }

    /// Closes `owner`'s session `id` without committing; whether it was open.
    pub fn rollback(&self, id: &str, owner: Option<&str>) -> bool {
        self.take(id, owner).is_some()
    }

    /// The data as `request` (the session's operations so far, scoped for a reader in
    /// `scope`) leaves the committed data `latest`, if a read computed it before.
    pub(crate) fn view(
        &self,
        latest: &Snapshot,
        scope: &ReadScope,
        request: &StatementsRequest,
    ) -> Option<Snapshot> {
        let id = request.session.as_deref()?;
        let open = self.lock();
        let view = open.get(id)?.view.as_ref()?;
        (view.base.same_version(latest)
            && view.ops == request.ops.len()
            && view.scope == *scope
            && view.writable == request.writable)
            .then(|| view.snapshot.clone())
    }

    /// Keeps `snapshot`, the data as `request` leaves `base`, for the session's next read.
    pub(crate) fn keep_view(
        &self,
        base: Snapshot,
        scope: &ReadScope,
        request: &StatementsRequest,
        snapshot: Snapshot,
    ) {
        let Some(id) = request.session.as_deref() else {
            return;
        };
        let mut open = self.lock();
        // Only for the operations the session still has (none were added meanwhile).
        if let Some(session) = open.get_mut(id)
            && session.ops.len() == request.ops.len()
        {
            session.view = Some(View {
                base,
                ops: request.ops.len(),
                scope: scope.clone(),
                writable: request.writable.clone(),
                snapshot,
            });
        }
    }

    /// The number of open sessions (idle ones included until the next `begin`).
    pub fn len(&self) -> usize {
        self.lock().len()
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
        let id = sessions.begin(None).unwrap();
        assert!(sessions.add(
            &id,
            None,
            vec![StatementOp::RemoveMatching(StatementPattern::default())]
        ));
        assert_eq!(sessions.pending(&id, None).map(|ops| ops.len()), Some(1));
        let other = sessions.begin(None).unwrap();
        assert_ne!(id, other);
        assert_eq!(
            sessions.take(&id, None).map(|request| request.ops.len()),
            Some(1)
        );
        assert!(sessions.take(&id, None).is_none());
        assert!(sessions.rollback(&other, None));
        assert!(!sessions.add(&other, None, Vec::new()));
        // Idle sessions go when the next one begins.
        let short = Sessions::with_idle(Duration::ZERO);
        let gone = short.begin(None).unwrap();
        short.begin(None).unwrap();
        assert!(!short.ping(&gone, None));
    }

    #[test]
    fn a_session_exists_for_its_owner_only() {
        let sessions = Sessions::default();
        let id = sessions.begin(Some("ada")).unwrap();
        assert_eq!(id.len(), "tx-".len() + 32, "{id}");
        assert_ne!(sessions.begin(Some("ada")).unwrap(), id);
        for stranger in [Some("eve"), None] {
            assert!(!sessions.add(&id, stranger, Vec::new()));
            assert!(!sessions.ping(&id, stranger));
            assert!(sessions.pending(&id, stranger).is_none());
            assert!(sessions.take(&id, stranger).is_none());
            assert!(!sessions.rollback(&id, stranger));
        }
        assert!(sessions.add(&id, Some("ada"), Vec::new()));
        assert!(sessions.rollback(&id, Some("ada")));
    }
}
