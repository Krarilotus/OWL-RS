//! Query result cache: serialised results per query, format and revision.
//!
//! A result is valid exactly for the revision it was computed on, so the snapshot revision is
//! part of the key and no invalidation logic is needed: a commit makes older entries
//! unreachable, and they are dropped when a newer revision shows up. Entries are the
//! serialised response bytes, so a hit is a copy into the output. The cache holds at most
//! its byte budget, evicting the oldest entries first; results larger than a quarter of
//! the budget are not kept.
//!
//! Queries that must not repeat their answer (`NOW()`, `RAND()`, `UUID()`, `STRUUID()`,
//! `BNODE()`) are never cached.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// What a cached result depends on besides the revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CacheKey {
    /// The query text, the output format and every dataset or read-model parameter.
    pub request: String,
    pub revision: u64,
}

#[derive(Default)]
struct Entries {
    map: HashMap<CacheKey, Arc<[u8]>>,
    /// Insertion order, oldest first (for eviction).
    order: VecDeque<CacheKey>,
    bytes: usize,
    /// The newest revision seen; entries of older ones are dropped.
    revision: u64,
}

/// Hit and miss counts of a [`QueryCache`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub bytes: usize,
    pub capacity: usize,
}

pub(crate) struct QueryCache {
    capacity: usize,
    entries: Mutex<Entries>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl QueryCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::default(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.capacity > 0
    }

    /// The largest result kept: a quarter of the budget, so one query can't evict all.
    pub(crate) fn max_entry(&self) -> usize {
        self.capacity / 4
    }

    fn lock(&self) -> MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn get(&self, key: &CacheKey) -> Option<Arc<[u8]>> {
        let found = self.lock().map.get(key).cloned();
        let counter = if found.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        counter.fetch_add(1, Ordering::Relaxed);
        found
    }

    pub(crate) fn insert(&self, key: CacheKey, bytes: Vec<u8>) {
        if bytes.len() > self.max_entry() {
            return;
        }
        let mut entries = self.lock();
        if key.revision < entries.revision {
            return; // computed on a snapshot that is already outdated
        }
        if key.revision > entries.revision {
            *entries = Entries {
                revision: key.revision,
                ..Entries::default()
            };
        }
        if entries.map.contains_key(&key) {
            return;
        }
        while entries.bytes + bytes.len() > self.capacity {
            let Some(oldest) = entries.order.pop_front() else {
                break;
            };
            if let Some(evicted) = entries.map.remove(&oldest) {
                entries.bytes -= evicted.len();
            }
        }
        entries.bytes += bytes.len();
        entries.order.push_back(key.clone());
        entries.map.insert(key, bytes.into());
    }

    pub(crate) fn stats(&self) -> QueryCacheStats {
        let entries = self.lock();
        QueryCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            entries: entries.map.len(),
            bytes: entries.bytes,
            capacity: self.capacity,
        }
    }
}

/// True if `query` may give another answer when repeated on the same data.
pub(crate) fn volatile(query: &str) -> bool {
    let upper = query.to_ascii_uppercase();
    // SERVICE: the endpoint's data may change without this store's revision.
    ["NOW(", "RAND(", "UUID(", "BNODE(", "SERVICE"]
        .iter()
        .any(|function| upper.contains(function))
}

/// A writer that forwards to `inner` and keeps a copy up to `limit` bytes.
pub(crate) struct Tee<W> {
    pub inner: W,
    pub copy: Option<Vec<u8>>,
    pub limit: usize,
}

impl<W: std::io::Write> std::io::Write for Tee<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        if let Some(copy) = &mut self.copy {
            if copy.len() + written > self.limit {
                self.copy = None;
            } else {
                copy.extend_from_slice(&buf[..written]);
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(request: &str, revision: u64) -> CacheKey {
        CacheKey {
            request: request.to_owned(),
            revision,
        }
    }

    #[test]
    fn entries_are_per_revision_and_bounded() {
        let cache = QueryCache::new(800);
        cache.insert(key("a", 1), vec![1; 100]);
        assert_eq!(cache.get(&key("a", 1)).as_deref(), Some(&[1u8; 100][..]));
        assert!(cache.get(&key("a", 2)).is_none(), "another revision");
        // A newer revision drops the older entries.
        cache.insert(key("b", 2), vec![2; 50]);
        assert!(cache.get(&key("a", 1)).is_none());
        // Results computed on an outdated snapshot are not kept.
        cache.insert(key("c", 1), vec![3; 10]);
        assert!(cache.get(&key("c", 1)).is_none());
        // Over budget: the oldest entries go first; too large results are not kept.
        for i in 0..10 {
            cache.insert(key(&format!("q{i}"), 2), vec![0; 100]);
        }
        assert!(cache.stats().bytes <= 800);
        assert!(cache.get(&key("q9", 2)).is_some());
        assert!(cache.get(&key("q0", 2)).is_none());
        cache.insert(key("huge", 2), vec![0; 201]);
        assert!(cache.get(&key("huge", 2)).is_none());
        let stats = cache.stats();
        assert!(stats.hits >= 2 && stats.misses >= 5, "{stats:?}");
    }

    #[test]
    fn volatile_queries_are_detected() {
        assert!(volatile("SELECT (now() AS ?t) {}"));
        assert!(volatile("SELECT ?x { BIND(STRUUID() AS ?x) }"));
        assert!(volatile("SELECT ?x { BIND(RAND() AS ?x) }"));
        assert!(!volatile("SELECT ?s { ?s ?p ?o }"));
    }
}
