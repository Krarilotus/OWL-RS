//! Client transactions: writes a client collects over several requests and commits as one
//! (RDF4J's `/transactions`; the console and other connectors can use the same). A session
//! holds the statement operations ([`StatementOp`]) in order; reads inside it see the
//! store as the operations would leave it ([`crate::ReadContext::on`],
//! [`crate::StoreService::run_query_pending`]); its commit goes through the mutation
//! pipeline like any write.
//!
//! What a session keeps between its requests, its operations and every copy of them, is
//! charged to the memory its store's queries share ([`crate::Runtime`]) before it is kept: an
//! operation it has no room for is refused, and the session stays as it was. Its reads and
//! its commit hold the same operations, never a copy ([`StatementOps`]), with their charge,
//! which goes when the last of them lets them go.
//!
//! A session untouched for [`Sessions::idle`] is gone: any call on it finds nothing, and
//! returns its charge. Expired sessions are dropped when a session begins and when one is
//! refused room, without a task of their own.
//!
//! A session belongs to whoever opened it: its id is unguessable, and every other call
//! names its owner, for whom alone it exists. Otherwise anyone who learns or guesses an id
//! could add operations that its owner then commits with the owner's rights.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use nrese_engine::Snapshot;
use nrese_exec::{Budget, BudgetExceeded};

use crate::ReadScope;
use crate::statements::{StatementOp, StatementOps, StatementsRequest};

/// How long an untouched session lives.
pub const SESSION_IDLE: Duration = Duration::from_secs(600);

struct Session {
    /// Who opened it (a user name, else roles; `None` without authentication).
    owner: Option<String>,
    ops: StatementOps,
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
    /// Whose: the operations read as the reader may.
    scope: ReadScope,
    /// The rules its inferred statements were decided under: how many times the store's
    /// rules had been set when its read began ([`crate::support::Supports::epoch`]).
    epoch: u64,
    snapshot: Snapshot,
}

/// The open sessions of one store.
pub struct Sessions {
    open: Mutex<HashMap<String, Session>>,
    idle: Duration,
    /// What the sessions keep is charged here.
    kept: Arc<Budget>,
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
    /// Sessions living `idle` untouched, what they keep charged to no limit.
    pub fn with_idle(idle: Duration) -> Self {
        Self::within(Arc::new(Budget::unlimited()), idle)
    }

    /// Sessions living `idle` untouched, what they keep charged to `kept`.
    pub fn within(kept: Arc<Budget>, idle: Duration) -> Self {
        Self {
            open: Mutex::default(),
            idle,
            kept,
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
                ops: StatementOps::default(),
                touched: Instant::now(),
                view: None,
            },
        );
        Ok(id)
    }

    /// Session `id` if it is `owner`'s and not expired, touched; an expired one is dropped
    /// (its owner's call ends it, nobody else's).
    fn live<'a>(
        &self,
        open: &'a mut HashMap<String, Session>,
        id: &str,
        owner: Option<&str>,
    ) -> Option<&'a mut Session> {
        let session = open
            .get(id)
            .filter(|session| session.owner.as_deref() == owner)?;
        if session.touched.elapsed() >= self.idle {
            open.remove(id);
            return None;
        }
        let session = open.get_mut(id)?;
        session.touched = Instant::now();
        Some(session)
    }

    /// Runs `f` on session `id` if it is open and `owner`'s, touching it.
    fn with<T>(
        &self,
        id: &str,
        owner: Option<&str>,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Option<T> {
        let mut open = self.lock();
        self.live(&mut open, id, owner).map(f)
    }

    /// Appends `ops` to `owner`'s session `id`, charged first; `false` if there is no such
    /// session. Refused room, expired sessions are dropped and the room asked for again; a
    /// refusal leaves the session as it was.
    pub fn add(
        &self,
        id: &str,
        owner: Option<&str>,
        ops: Vec<StatementOp>,
    ) -> Result<bool, BudgetExceeded> {
        let mut open = self.lock();
        let Some(session) = self.live(&mut open, id, owner) else {
            return Ok(false);
        };
        let (refused, ops) = match session.ops.append_within(ops, &self.kept) {
            Ok(()) => {
                session.view = None;
                return Ok(true);
            }
            Err(refused) => refused,
        };
        // Expired sessions' room, then once more if they had any.
        let before = open.len();
        let idle = self.idle;
        open.retain(|other, session| other == id || session.touched.elapsed() < idle);
        let swept = open.len() < before;
        let Some(session) = open.get_mut(id).filter(|_| swept) else {
            return Err(refused);
        };
        match session.ops.append_within(ops, &self.kept) {
            Ok(()) => {
                session.view = None;
                Ok(true)
            }
            Err((refused, _)) => Err(refused),
        }
    }

    /// Keeps `owner`'s session `id` alive; `false` if there is no such session.
    pub fn ping(&self, id: &str, owner: Option<&str>) -> bool {
        self.with(id, owner, |_| ()).is_some()
    }

    /// The operations so far of `owner`'s session `id` (for reads inside it), if it is
    /// open: the session's own, shared, with their charge.
    pub fn pending(&self, id: &str, owner: Option<&str>) -> Option<StatementOps> {
        self.with(id, owner, |session| session.ops.clone())
    }

    /// Closes `owner`'s session `id` and returns its operations to commit, if it was open;
    /// their charge goes with them.
    pub fn take(&self, id: &str, owner: Option<&str>) -> Option<StatementsRequest> {
        let mut open = self.lock();
        self.live(&mut open, id, owner)?;
        open.remove(id).map(|session| StatementsRequest {
            ops: session.ops,
            ..StatementsRequest::default()
        })
    }

    /// Closes `owner`'s session `id` without committing; whether it was open.
    pub fn rollback(&self, id: &str, owner: Option<&str>) -> bool {
        self.take(id, owner).is_some()
    }

    /// The data as `request` (the session's operations so far, scoped for a reader in
    /// `scope`) leaves the committed data `latest` under the rules set `epoch` times, if a
    /// read computed it before.
    pub(crate) fn view(
        &self,
        latest: &Snapshot,
        scope: &ReadScope,
        request: &StatementsRequest,
        epoch: u64,
    ) -> Option<Snapshot> {
        let id = request.session.as_deref()?;
        let open = self.lock();
        let view = open.get(id)?.view.as_ref()?;
        (view.base.same_version(latest)
            && view.ops == request.ops.len()
            && view.scope == *scope
            && view.epoch == epoch)
            .then(|| view.snapshot.clone())
    }

    /// Keeps `snapshot`, the data as `request` leaves `base` under the rules set `epoch`
    /// times when its read began, for the session's next read; only while those are still
    /// the rules (`current`): a read that outlived a change of them keeps nothing.
    pub(crate) fn keep_view(
        &self,
        base: Snapshot,
        scope: &ReadScope,
        request: &StatementsRequest,
        snapshot: Snapshot,
        epoch: u64,
        current: u64,
    ) {
        let Some(id) = request.session.as_deref() else {
            return;
        };
        let mut open = self.lock();
        // Only for the operations the session still has (none were added meanwhile).
        if let Some(session) = open.get_mut(id)
            && session.ops.len() == request.ops.len()
            && epoch == current
        {
            session.view = Some(View {
                base,
                ops: request.ops.len(),
                scope: scope.clone(),
                epoch,
                snapshot,
            });
        }
    }

    /// The number of open sessions (expired ones included until a call drops them).
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
        let remove = vec![StatementOp::RemoveMatching(StatementPattern::default())];
        assert!(sessions.add(&id, None, remove).unwrap());
        assert_eq!(sessions.pending(&id, None).map(|ops| ops.len()), Some(1));
        let other = sessions.begin(None).unwrap();
        assert_ne!(id, other);
        assert_eq!(
            sessions.take(&id, None).map(|request| request.ops.len()),
            Some(1)
        );
        assert!(sessions.take(&id, None).is_none());
        assert!(sessions.rollback(&other, None));
        assert!(!sessions.add(&other, None, Vec::new()).unwrap());
        // Idle sessions go when the next one begins.
        let short = Sessions::with_idle(Duration::ZERO);
        let gone = short.begin(None).unwrap();
        short.begin(None).unwrap();
        assert!(!short.ping(&gone, None));
    }

    /// Spec: a session's kept view holds for the rules it was decided under (review of O4's
    /// fix round, S1): it is served only while they are the store's, and a read that began
    /// under earlier rules and ends after a change of them keeps nothing.
    #[test]
    fn a_view_is_kept_and_served_for_its_rules_only() {
        let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
        let snapshot = store.read_snapshot(None).unwrap();
        let sessions = Sessions::default();
        let id = sessions.begin(None).unwrap();
        let remove = vec![StatementOp::RemoveMatching(StatementPattern::default())];
        assert!(sessions.add(&id, None, remove).unwrap());
        let request = StatementsRequest {
            ops: sessions.pending(&id, None).unwrap(),
            session: Some(id),
        };
        let keep = |epoch, current| {
            let view = snapshot.clone();
            sessions.keep_view(
                snapshot.clone(),
                &ReadScope::All,
                &request,
                view,
                epoch,
                current,
            );
        };
        let served = |epoch| {
            sessions
                .view(&snapshot, &ReadScope::All, &request, epoch)
                .is_some()
        };
        // A read that began under the rules set once ends after they were set again.
        keep(1, 2);
        assert!(
            !served(1) && !served(2),
            "kept by a read that outlived its rules"
        );
        keep(2, 2);
        assert!(served(2), "kept under the rules in place");
        assert!(!served(3), "served after the rules changed");
    }

    #[test]
    fn a_session_exists_for_its_owner_only() {
        let sessions = Sessions::default();
        let id = sessions.begin(Some("ada")).unwrap();
        assert_eq!(id.len(), "tx-".len() + 32, "{id}");
        assert_ne!(sessions.begin(Some("ada")).unwrap(), id);
        for stranger in [Some("eve"), None] {
            assert!(!sessions.add(&id, stranger, Vec::new()).unwrap());
            assert!(!sessions.ping(&id, stranger));
            assert!(sessions.pending(&id, stranger).is_none());
            assert!(sessions.take(&id, stranger).is_none());
            assert!(!sessions.rollback(&id, stranger));
        }
        assert!(sessions.add(&id, Some("ada"), Vec::new()).unwrap());
        assert!(sessions.rollback(&id, Some("ada")));
    }

    /// An ADD of a `bytes`-byte payload.
    fn body(bytes: usize) -> Vec<StatementOp> {
        filled(b'#', bytes)
    }

    /// An ADD of `bytes` bytes, each `byte`.
    fn filled(byte: u8, bytes: usize) -> Vec<StatementOp> {
        vec![StatementOp::Add {
            data: crate::RdfPayload {
                payload: vec![byte; bytes].into(),
                format: crate::query::GraphResultFormat::Turtle,
                base_iri: None,
            },
            contexts: Vec::new(),
        }]
    }

    /// Each ADD's payload byte, in order (`?` where its payload isn't one byte repeated).
    fn bytes_of(ops: &[StatementOp]) -> Vec<u8> {
        let byte = |op: &StatementOp| match op {
            StatementOp::Add { data, .. } => match data.payload.split_first() {
                Some((&first, rest)) if rest.iter().all(|&b| b == first) => first,
                _ => b'?',
            },
            _ => b'?',
        };
        ops.iter().map(byte).collect()
    }

    /// Session `id`, untouched for longer than `sessions` keeps one.
    fn age(sessions: &Sessions, id: &str) {
        let past = Instant::now().checked_sub(sessions.idle + Duration::from_secs(1));
        sessions.lock().get_mut(id).expect("open").touched = past.expect("a clock past it");
    }

    /// Spec (design note N3, I1–I4): what a session keeps is charged before it is kept, a
    /// refusal leaves it as it was, a read holds the operations with their charge (an add
    /// meanwhile copies them under a charge of its own), and the charge goes when the last
    /// holder lets them go, after a rollback too.
    #[test]
    fn what_a_session_keeps_is_charged_until_its_last_holder_lets_it_go() {
        let kept = Arc::new(Budget::new(100_000));
        let sessions = Sessions::within(Arc::clone(&kept), SESSION_IDLE);
        let id = sessions.begin(None).unwrap();
        assert!(sessions.add(&id, None, filled(b'a', 20_000)).unwrap());
        let one = kept.used();
        assert!(one >= 20_000, "{one} charged for a 20 kB body");
        // A read holds the operations; an add meanwhile copies them, charged: the read
        // keeps what it read, the session has both in order.
        let read = sessions.pending(&id, None).unwrap();
        assert!(sessions.add(&id, None, filled(b'b', 20_000)).unwrap());
        let both = kept.used();
        assert!(both >= one + 40_000, "{both} with a read held");
        assert_eq!(bytes_of(&read), b"a");
        assert_eq!(bytes_of(&sessions.pending(&id, None).unwrap()), b"ab");
        drop(read);
        let two = kept.used();
        assert!((40_000..one + 40_000).contains(&two), "{two} after");
        // No room: refused, nothing kept, nothing charged.
        let refused = sessions.add(&id, None, body(70_000)).unwrap_err();
        assert!(!refused.shared && refused.limit == 100_000, "{refused:?}");
        assert_eq!(sessions.pending(&id, None).unwrap().len(), 2);
        assert_eq!(kept.used(), two);
        // Room for an op's bytes but not for its slot: refused, the bytes' charge returned.
        let slot = std::mem::size_of::<StatementOp>();
        let tight = Arc::new(Budget::new(1_000 + 10 + 2 * slot));
        let small = Sessions::within(Arc::clone(&tight), SESSION_IDLE);
        let id_small = small.begin(None).unwrap();
        assert!(small.add(&id_small, None, body(1_000)).unwrap());
        let before = tight.used();
        assert_eq!(before, 1_000 + slot, "one op: its bytes and its slot");
        assert!(small.add(&id_small, None, body(10)).is_err());
        assert_eq!(tight.used(), before, "the refused bytes stayed charged");
        assert_eq!(small.pending(&id_small, None).unwrap().len(), 1);
        // A read outlives the rollback, and its charge with it.
        let read = sessions.pending(&id, None).unwrap();
        assert!(sessions.rollback(&id, None));
        assert_eq!(kept.used(), two);
        drop(read);
        assert_eq!(kept.used(), 0);
        // The commit's operations hold it until they go.
        let id = sessions.begin(None).unwrap();
        assert!(sessions.add(&id, None, body(20_000)).unwrap());
        let commit = sessions.take(&id, None).unwrap();
        assert!(kept.used() >= 20_000);
        drop(commit);
        assert_eq!(kept.used(), 0);
    }

    /// Spec (D16-039 S12, charged by length): a body that is a slice of a larger request
    /// buffer is kept as a copy of its own, never holding that buffer past the request.
    #[test]
    fn a_session_keeps_a_body_slice_as_its_own_copy() {
        let sessions = Sessions::within(Arc::new(Budget::new(100_000)), SESSION_IDLE);
        let id = sessions.begin(None).unwrap();
        let buffer = bytes::Bytes::from(vec![b'#'; 64 * 1024]);
        let mut add = body(0);
        if let StatementOp::Add { data, .. } = &mut add[0] {
            data.payload = buffer.slice(..10);
        }
        assert!(sessions.add(&id, None, add).unwrap());
        assert!(buffer.is_unique(), "the session holds the request's buffer");
        assert_eq!(bytes_of(&sessions.pending(&id, None).unwrap()), b"#");
    }

    /// Spec (design note N3, I5; triage N3's expiry control): a session idle past its
    /// lifetime is gone for its owner's every call, which returns its charge; a stranger's
    /// call ends nothing; and an add refused room first drops expired sessions and tries
    /// once more.
    #[test]
    fn an_expired_session_is_gone_and_returns_its_charge() {
        let kept = Arc::new(Budget::new(100_000));
        let sessions = Sessions::within(Arc::clone(&kept), SESSION_IDLE);
        for call in ["ping", "add", "pending", "take", "rollback"] {
            let id = sessions.begin(Some("ada")).unwrap();
            assert!(sessions.add(&id, Some("ada"), body(20_000)).unwrap());
            age(&sessions, &id);
            assert!(!sessions.ping(&id, Some("eve")));
            assert!(kept.used() >= 20_000, "{call}: a stranger ended it");
            let found = match call {
                "ping" => sessions.ping(&id, Some("ada")),
                "add" => sessions.add(&id, Some("ada"), body(1)).unwrap(),
                "pending" => sessions.pending(&id, Some("ada")).is_some(),
                "take" => sessions.take(&id, Some("ada")).is_some(),
                _ => sessions.rollback(&id, Some("ada")),
            };
            assert!(!found, "{call} found an expired session");
            assert_eq!(kept.used(), 0, "{call}: its charge stayed");
            assert!(sessions.is_empty(), "{call}");
        }
        // An abandoned session holds most of the room; a live one's add takes it back.
        let abandoned = sessions.begin(None).unwrap();
        assert!(sessions.add(&abandoned, None, body(70_000)).unwrap());
        let live = sessions.begin(None).unwrap();
        age(&sessions, &abandoned);
        assert!(sessions.add(&live, None, body(50_000)).unwrap());
        assert_eq!(sessions.len(), 1);
        assert!(sessions.pending(&abandoned, None).is_none());
        assert_eq!(sessions.pending(&live, None).unwrap().len(), 1);
    }
}
