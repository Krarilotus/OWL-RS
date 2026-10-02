//! The queries running on a store, for operators: what runs, since when, for whom, and a
//! way to stop one. Every evaluation through [`crate::StoreService`] registers itself for
//! as long as it runs; cancelling it fires its [`CancellationToken`], and the evaluation
//! stops at its next check (it then reports itself cancelled).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use nrese_sparql::CancellationToken;

/// A query running now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunningQuery {
    pub id: u64,
    /// Its text (the first 4096 bytes).
    pub query: String,
    /// Who sent it, where the caller said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// When it started, in seconds since the Unix epoch.
    pub started: u64,
    pub elapsed_ms: u64,
}

struct Entry {
    query: String,
    origin: Option<String>,
    started: SystemTime,
    since: Instant,
    token: CancellationToken,
}

/// The running queries of one store.
#[derive(Default)]
pub struct RunningQueries {
    next: AtomicU64,
    entries: Mutex<HashMap<u64, Entry>>,
}

impl std::fmt::Debug for RunningQueries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningQueries").finish_non_exhaustive()
    }
}

/// A registration: the query leaves the list when this is dropped.
pub struct Running<'a> {
    queries: &'a RunningQueries,
    id: u64,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.queries
            .entries
            .lock()
            .expect("running queries")
            .remove(&self.id);
    }
}

const SHOWN: usize = 4096;

impl RunningQueries {
    /// Registers a query that `token` cancels, until the returned guard is dropped.
    pub fn register(
        &self,
        query: &str,
        origin: Option<&str>,
        token: &CancellationToken,
    ) -> Running<'_> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut end = query.len().min(SHOWN);
        while !query.is_char_boundary(end) {
            end -= 1;
        }
        self.entries.lock().expect("running queries").insert(
            id,
            Entry {
                query: query[..end].to_owned(),
                origin: origin.map(str::to_owned),
                started: SystemTime::now(),
                since: Instant::now(),
                token: token.clone(),
            },
        );
        Running { queries: self, id }
    }

    /// The queries running now, the longest-running first.
    pub fn list(&self) -> Vec<RunningQuery> {
        let entries = self.entries.lock().expect("running queries");
        let mut list: Vec<RunningQuery> = entries
            .iter()
            .map(|(&id, entry)| RunningQuery {
                id,
                query: entry.query.clone(),
                origin: entry.origin.clone(),
                started: entry
                    .started
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
                elapsed_ms: u64::try_from(entry.since.elapsed().as_millis()).unwrap_or(u64::MAX),
            })
            .collect();
        list.sort_by(|a, b| b.elapsed_ms.cmp(&a.elapsed_ms).then(a.id.cmp(&b.id)));
        list
    }

    /// Cancels query `id`; whether it was running.
    pub fn cancel(&self, id: u64) -> bool {
        match self.entries.lock().expect("running queries").get(&id) {
            Some(entry) => {
                entry.token.cancel();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_listed_while_they_run_and_can_be_cancelled() {
        let queries = RunningQueries::default();
        let token = CancellationToken::new();
        {
            let _first = queries.register("SELECT * WHERE { ?s ?p ?o }", Some("alice"), &token);
            let other = CancellationToken::new();
            let _second = queries.register(&"é".repeat(5000), None, &other);
            let list = queries.list();
            assert_eq!(list.len(), 2);
            let first = list.iter().find(|q| q.origin.is_some()).unwrap();
            assert_eq!(first.query, "SELECT * WHERE { ?s ?p ?o }");
            assert!(list.iter().all(|q| q.query.len() <= SHOWN));
            assert!(queries.cancel(first.id));
            assert!(token.is_cancelled());
            assert!(!other.is_cancelled());
        }
        assert!(queries.list().is_empty());
        assert!(!queries.cancel(1));
    }
}
