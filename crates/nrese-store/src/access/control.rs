//! The access state kept in a store of its own (the server's system store, which no
//! ordinary query reaches): the state's statements in [`STATE_GRAPH`], and every change in
//! [`HISTORY_GRAPH`] as a resource with its number, author, time, reason, summary and the
//! statements it added and removed (password hashes left out).
//!
//! ```turtle
//! <urn:nrese:change:7> a nra:Change ; nra:number 7 ; nra:author "alice" ;
//!     nra:time "2026-10-02T09:30:00Z"^^xsd:dateTime ; nra:reason "project start" ;
//!     nra:summary "bob is editor of project" ; nra:added "…" ; nra:removed "…" .
//! ```
//!
//! A change is one commit of the system store: the state and its history change together
//! or not at all. Reads use the state in memory; it is read from the store at start.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock};

use nrese_rdf::{Literal, NamedNode, Quad, Term};

use super::login::{LoginLimits, Logins, hash_password};
use super::rdf::{self, iri, term};
use super::{AccessError, AccessState, AccessView, Change, Principal};
use crate::{SparqlUpdateRequest, StatementPattern, StoreService};

/// The graph of the access state in the system store.
pub const STATE_GRAPH: &str = "urn:nrese:system:access";
/// The graph of its history.
pub const HISTORY_GRAPH: &str = "urn:nrese:system:history";
const CHANGE: &str = "urn:nrese:change:";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

/// One change of the state, as its history keeps it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChangeRecord {
    pub number: u64,
    pub author: String,
    /// UTC, as `xsd:dateTime`.
    pub time: String,
    pub reason: String,
    pub summary: String,
    /// The statements it added and removed (N-Triples, in the state graph).
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// The access state, kept in `store`.
pub struct AccessControl {
    pub(super) store: StoreService,
    state: RwLock<Arc<AccessState>>,
    /// The saved queries ([`super::queries`]); writes hold its lock.
    pub(super) queries: RwLock<super::queries::Queries>,
    /// Changes one at a time; holds the next change's number.
    writer: Mutex<u64>,
    logins: Logins,
}

impl std::fmt::Debug for AccessControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessControl").finish_non_exhaustive()
    }
}

fn graph_pattern(graph: &str) -> StatementPattern {
    StatementPattern {
        contexts: vec![nrese_rdf::GraphName::NamedNode(iri(graph))],
        ..StatementPattern::default()
    }
}

/// The current time, UTC, as `xsd:dateTime` (seconds).
pub(super) fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// A statement as a line of the history: N-Triples, password hashes left out.
fn line(quad: &Quad) -> String {
    let object = match quad.predicate.as_str() {
        p if p == format!("{}passwordHash", rdf::NS) => "\"(not kept)\"".to_owned(),
        _ => quad.object.to_string(),
    };
    format!("{} {} {} .", quad.subject, quad.predicate, object)
}

/// The quads as SPARQL update data, each in its graph.
pub(super) fn data(quads: &[Quad]) -> String {
    let mut by_graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for quad in quads {
        by_graph
            .entry(quad.graph_name.to_string())
            .or_default()
            .push(format!(
                "{} {} {} .",
                quad.subject, quad.predicate, quad.object
            ));
    }
    by_graph
        .into_iter()
        .map(|(graph, lines)| format!("GRAPH {graph} {{\n{}\n}}", lines.join("\n")))
        .collect::<Vec<_>>()
        .join("\n")
}

impl AccessControl {
    /// The access state kept in `store`; workspace prefixes under `base`.
    pub fn open(store: StoreService, base: &str) -> Result<Self, AccessError> {
        Self::open_with(store, base, LoginLimits::default())
    }

    /// [`Self::open`] with local logins bounded by `limits`.
    pub fn open_with(
        store: StoreService,
        base: &str,
        limits: LoginLimits,
    ) -> Result<Self, AccessError> {
        let quads = store
            .statements(
                &crate::ReadContext::all().infer(false),
                &graph_pattern(STATE_GRAPH),
            )
            .map_err(|error| AccessError::Store(error.to_string()))?;
        let state = rdf::decode(&quads, base);
        let queries = super::queries::load(&store)?;
        let control = Self {
            store,
            state: RwLock::new(Arc::new(state)),
            queries: RwLock::new(queries),
            writer: Mutex::new(0),
            logins: Logins::new(limits),
        };
        let next = control
            .history(usize::MAX)?
            .first()
            .map_or(1, |latest| latest.number + 1);
        *control.writer.lock().expect("writer") = next;
        Ok(control)
    }

    /// The state now.
    pub fn state(&self) -> Arc<AccessState> {
        Arc::clone(&self.state.read().expect("state"))
    }

    /// Whether the state was never changed.
    pub fn is_new(&self) -> bool {
        *self.writer.lock().expect("writer") == 1
    }

    /// What `principal` may read and write in `repository`.
    pub fn view(&self, principal: &Principal, repository: &str) -> AccessView {
        self.state().view(principal, repository)
    }

    /// Applies `change` by `by`, with `reason` (required): the state and its history change
    /// in one commit. Returns the change's record.
    pub fn apply(
        &self,
        by: &Principal,
        change: Change,
        reason: &str,
    ) -> Result<ChangeRecord, AccessError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(AccessError::Invalid(
                "a change of the access state needs a reason".to_owned(),
            ));
        }
        // A user's sessions end when its password changes or it is removed.
        let ends_sessions = match &change {
            Change::PutUser {
                name,
                password_hash: Some(_),
                ..
            }
            | Change::RemoveUser(name) => Some(name.clone()),
            _ => None,
        };
        let mut next = self.writer.lock().expect("writer");
        let before = self.state();
        let mut after = (*before).clone();
        let summary = after.apply(by, change)?;
        let graph = iri(STATE_GRAPH);
        let old: BTreeSet<Quad> = rdf::encode(&before, &graph).into_iter().collect();
        let new: BTreeSet<Quad> = rdf::encode(&after, &graph).into_iter().collect();
        let removed: Vec<Quad> = old.difference(&new).cloned().collect();
        let added: Vec<Quad> = new.difference(&old).cloned().collect();
        let record = ChangeRecord {
            number: *next,
            author: by.display(),
            time: now(),
            reason: reason.to_owned(),
            summary,
            added: added.iter().map(line).collect(),
            removed: removed.iter().map(line).collect(),
        };
        let mut inserts = added;
        inserts.extend(record_quads(&record));
        let mut update = String::new();
        if !removed.is_empty() {
            update.push_str(&format!("DELETE DATA {{\n{}\n}} ;\n", data(&removed)));
        }
        update.push_str(&format!("INSERT DATA {{\n{}\n}}", data(&inserts)));
        self.store
            .execute_update(&SparqlUpdateRequest::new(update))
            .map_err(|error| AccessError::Store(error.to_string()))?;
        *self.state.write().expect("state") = Arc::new(after);
        *next += 1;
        if let Some(user) = ends_sessions {
            self.logins.close_all(&user);
        }
        Ok(record)
    }

    /// The password hash a change of `password` stores: `None` for an empty one (the
    /// local login removed); refused if shorter than the settings allow.
    pub fn password_hash(&self, password: &str) -> Result<Option<String>, AccessError> {
        if password.is_empty() {
            return Ok(None);
        }
        let shortest = self.state().settings.min_password_length;
        if password.chars().count() < shortest as usize {
            return Err(AccessError::Invalid(format!(
                "a password needs {shortest} characters at least"
            )));
        }
        hash_password(password).map(Some)
    }

    /// The principal of a local login with `user` and `password` from `client` (the
    /// client's address, for throttling failures; `None` if unknown).
    pub fn login(
        &self,
        user: &str,
        password: &str,
        client: Option<std::net::IpAddr>,
    ) -> Result<Principal, AccessError> {
        self.logins.login(&self.state(), user, password, client)
    }

    /// The failure counts local logins keep: per name and address, per address.
    pub fn login_failures_tracked(&self) -> (usize, usize) {
        self.logins.tracked()
    }

    /// Logs in and opens a session: its token and how long it lasts.
    pub fn open_session(
        &self,
        user: &str,
        password: &str,
        client: Option<std::net::IpAddr>,
    ) -> Result<(String, std::time::Duration), AccessError> {
        self.login(user, password, client)?;
        let hours = self.state().settings.session_hours.max(1);
        let lifetime = std::time::Duration::from_secs(u64::from(hours) * 3600);
        Ok((self.logins.open(user, lifetime)?, lifetime))
    }

    /// The principal of session `token`, while it lasts and its user has a local login.
    pub fn session(&self, token: &str) -> Option<Principal> {
        let user = self.logins.session(token)?;
        let state = self.state();
        state
            .users
            .get(&user)
            .is_some_and(|record| record.password_hash.is_some())
            .then(|| Principal {
                user: Some(user),
                ..Principal::default()
            })
    }

    /// Closes session `token`; whether it was open.
    pub fn close_session(&self, token: &str) -> bool {
        self.logins.close(token)
    }

    /// The latest `limit` changes, the latest first.
    pub fn history(&self, limit: usize) -> Result<Vec<ChangeRecord>, AccessError> {
        let quads = self
            .store
            .statements(
                &crate::ReadContext::all().infer(false),
                &graph_pattern(HISTORY_GRAPH),
            )
            .map_err(|error| AccessError::Store(error.to_string()))?;
        let mut changes: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for quad in &quads {
            let Some(local) = quad.predicate.as_str().strip_prefix(rdf::NS) else {
                continue;
            };
            let value = match &quad.object {
                Term::Literal(literal) => literal.value().to_owned(),
                other => other.to_string(),
            };
            changes
                .entry(quad.subject.to_string())
                .or_default()
                .push((local.to_owned(), value));
        }
        let mut records: Vec<ChangeRecord> = changes
            .values()
            .filter_map(|fields| {
                let get = |name: &str| {
                    fields
                        .iter()
                        .find(|(p, _)| p == name)
                        .map(|(_, v)| v.clone())
                };
                let lines = |name: &str| -> Vec<String> {
                    get(name)
                        .map(|text| text.lines().map(str::to_owned).collect())
                        .unwrap_or_default()
                };
                Some(ChangeRecord {
                    number: get("number")?.parse().ok()?,
                    author: get("author").unwrap_or_default(),
                    time: get("time").unwrap_or_default(),
                    reason: get("reason").unwrap_or_default(),
                    summary: get("summary").unwrap_or_default(),
                    added: lines("added"),
                    removed: lines("removed"),
                })
            })
            .collect();
        records.sort_by_key(|record| std::cmp::Reverse(record.number));
        records.truncate(limit);
        Ok(records)
    }
}

/// A change's record as statements of the history graph.
fn record_quads(record: &ChangeRecord) -> Vec<Quad> {
    let graph = nrese_rdf::GraphName::NamedNode(iri(HISTORY_GRAPH));
    let subject: NamedNode = iri(&format!("{CHANGE}{}", record.number));
    let literal = |value: &str| -> Term { Literal::new_simple_literal(value).into() };
    let typed = |value: &str, datatype: &str| -> Term {
        Literal::new_typed_literal(value, iri(datatype)).into()
    };
    let mut quads = vec![Quad::new(
        subject.clone(),
        iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        term("Change"),
        graph.clone(),
    )];
    for (local, object) in [
        ("number", typed(&record.number.to_string(), XSD_INTEGER)),
        ("author", literal(&record.author)),
        ("time", typed(&record.time, XSD_DATE_TIME)),
        ("reason", literal(&record.reason)),
        ("summary", literal(&record.summary)),
        ("added", literal(&record.added.join("\n"))),
        ("removed", literal(&record.removed.join("\n"))),
    ] {
        quads.push(Quad::new(
            subject.clone(),
            term(local),
            object,
            graph.clone(),
        ));
    }
    quads
}
