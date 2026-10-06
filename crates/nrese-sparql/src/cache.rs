//! The result cache: the results of query plan parts, kept as id columns and shared by
//! every query of a store (QLever's level, merge checklist §2 item 7, step 1).
//!
//! **What is kept.** Each operator the executor evaluates is a part: a scan, a basic graph
//! pattern, a join, a group, the whole query. Its result is an id table with the part's
//! variables by number ([`crate::native`]'s cache key numbers them, so parts that differ
//! only in their variables' names share an entry), plus the few terms computed during the
//! query that it holds (`BIND` and aggregate values the dictionary doesn't know). Nothing
//! is decoded: a cached part costs 8 bytes per value.
//!
//! **When it is valid.** A key is the part's algebra with everything its result depends
//! on: the snapshot's identity (the revision and the statements it shows: a transaction's
//! pending state or a masked view has its own, [`nrese_engine::SnapshotIdentity`]), the
//! read model, the dataset as resolved for the user's access, the active graph, the
//! equality reading, the base IRI and the executor's options. So no invalidation is
//! needed: a commit makes older entries unreachable, and they are dropped when a newer
//! revision is first stored. Parts calling `RAND`, `NOW`, `UUID`, `STRUUID` or `BNODE`, or
//! reading a `SERVICE`, are never kept.
//!
//! **Memory.** The cache holds at most its byte budget, counting each entry's columns,
//! computed terms, key and bookkeeping. A part is admitted by its benefit: it must have
//! taken longer to compute than a hit takes to copy it ([`MIN_COST`],
//! [`COPY_BYTES_PER_MICRO`]), and no larger than a quarter of the budget. Entries are ranked
//! by GreedyDual-Size-Frequency (Cao and Irani 1997; the benefit policy of MonetDB's
//! recycler, Ivanova et al. 2010): `clock + cost × (1 + hits) / bytes`; the lowest goes
//! first and sets the clock, so entries that are not hit again age out. A new part that
//! would have to evict entries ranked above it is not admitted.
//!
//! **Computed once.** A part requested while another query computes it waits for that
//! result instead of computing it again (QLever's `computeOnce`); a waiter gets the result
//! even if it isn't admitted, and computes it itself if the other query fails.
//!
//! **Pinned.** A named query's result can be pinned ([`ResultCache::pins`]): it is never
//! evicted while its revision is current, and counts against the budget. After a commit it
//! is pinned again the next time it is computed.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::Duration;

use nrese_engine::SnapshotIdentity;
use nrese_exec::IdTable;
use nrese_rdf::Term;

/// A part computed faster than this isn't kept: a hit would save less than looking it
/// up costs.
pub const MIN_COST: Duration = Duration::from_micros(20);

/// Bytes a hit copies per microsecond (at about 4 GB/s, half of it): a part must have
/// taken longer to compute than its copy takes, twice over.
pub const COPY_BYTES_PER_MICRO: u64 = 2_000;

/// Bookkeeping per entry beside its columns and key: the part's allocation, the map's
/// slot (with its free share), the ranking's node, the counters. Checked against the
/// allocator by the budget guard (`tests/cache_memory.rs`: about 650 bytes held per entry
/// on 6 October).
const ENTRY_OVERHEAD: usize = 768;

/// How long a waiter sleeps between checks of its own cancellation.
const WAIT_CHECK: Duration = Duration::from_millis(10);

/// A part's key: the snapshot it was computed on and its encoding (the context, then the
/// algebra).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub(crate) snapshot: SnapshotIdentity,
    pub(crate) part: Arc<[u8]>,
}

/// A part's result.
#[derive(Debug)]
pub(crate) struct Part {
    /// The number (in the key) of each column's variable.
    pub(crate) vars: Vec<u32>,
    pub(crate) table: IdTable,
    /// Rows in an order that matters (`ORDER BY`).
    pub(crate) ordered: bool,
    /// The computed terms the table holds: its computed ids are their positions.
    pub(crate) computed: Vec<Term>,
}

impl Part {
    fn bytes(&self) -> usize {
        let computed: usize = self
            .computed
            .iter()
            .map(|term| std::mem::size_of::<Term>() + term_heap(term))
            .sum();
        let columns = self.table.width() * std::mem::size_of::<Vec<u64>>();
        self.table.memory_bytes() + columns + self.vars.len() * 4 + computed
    }
}

/// Heap bytes of a term's strings, roughly (for the budget).
fn term_heap(term: &Term) -> usize {
    match term {
        Term::NamedNode(n) => n.as_str().len(),
        Term::BlankNode(b) => b.as_str().len(),
        Term::Literal(l) => {
            l.value().len() + l.datatype().as_str().len() + l.language().map_or(0, str::len)
        }
        Term::Triple(t) => 3 * std::mem::size_of::<Term>() + t.to_string().len(),
    }
}

struct Entry {
    part: Arc<Part>,
    bytes: usize,
    cost: u64,
    hits: u64,
    /// Its place in the ranking; `None` while pinned.
    rank: Option<(u64, u64)>,
}

/// One computation of a part that others may wait for.
pub(crate) struct Flight {
    owner: ThreadId,
    waiters: AtomicUsize,
    /// `None` while running; then the result, if there is one to share.
    done: Mutex<Option<Option<Arc<Part>>>>,
    ready: Condvar,
}

impl Flight {
    fn finish(&self, part: Option<Arc<Part>>) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = Some(part);
        self.ready.notify_all();
    }

    /// Waits for the computation; `None` if it gave no result (it failed, or its query
    /// was cancelled). `cancelled` is checked every few milliseconds.
    pub(crate) fn wait(&self, cancelled: impl Fn() -> bool) -> Result<Option<Arc<Part>>, ()> {
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(result) = &*done {
                return Ok(result.clone());
            }
            if cancelled() {
                return Err(());
            }
            done = self
                .ready
                .wait_timeout(done, WAIT_CHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// A pinned query ([`ResultCache::pins`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedResult {
    pub name: String,
    /// The query, as given.
    pub query: String,
    /// The revision its result is held for; `None` if it isn't held now (it is computed
    /// and pinned again the next time the query runs).
    pub revision: Option<u64>,
    pub rows: usize,
    pub bytes: usize,
}

/// A request to pin a query's result under a name ([`crate::QueryOptions::pin`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinRequest {
    pub name: String,
    pub query: String,
}

struct Pin {
    part: Arc<[u8]>,
    query: String,
}

#[derive(Default)]
struct State {
    entries: HashMap<Key, Entry>,
    /// Unpinned entries by rank, lowest first.
    ranking: BTreeMap<(u64, u64), Key>,
    bytes: usize,
    /// GreedyDual's clock: the rank of the last entry evicted.
    clock: f64,
    /// Ranks issued, to order equal ones.
    sequence: u64,
    /// The newest revision stored; older entries are gone.
    revision: u64,
    flights: HashMap<Key, Arc<Flight>>,
    pins: BTreeMap<String, Pin>,
}

impl State {
    fn pinned(&self, part: &[u8]) -> bool {
        self.pins.values().any(|pin| *pin.part == *part)
    }

    /// GreedyDual-Size-Frequency's rank, as bits that order like the (positive) value.
    fn rank(&mut self, cost: u64, hits: u64, bytes: usize) -> (u64, u64) {
        let value = self.clock + (cost as f64 * (1 + hits) as f64) / bytes.max(1) as f64;
        self.sequence += 1;
        (value.to_bits(), self.sequence)
    }

    fn rerank(&mut self, key: &Key) {
        let Some(entry) = self.entries.get(key) else {
            return;
        };
        let (cost, hits, bytes, old) = (entry.cost, entry.hits, entry.bytes, entry.rank);
        if let Some(old) = old {
            self.ranking.remove(&old);
        }
        let rank = match self.pinned(&key.part) {
            true => None,
            false => Some(self.rank(cost, hits, bytes)),
        };
        if let Some(rank) = rank {
            self.ranking.insert(rank, key.clone());
        }
        if let Some(entry) = self.entries.get_mut(key) {
            entry.rank = rank;
        }
    }

    fn remove(&mut self, key: &Key) -> Option<Entry> {
        let entry = self.entries.remove(key)?;
        if let Some(rank) = entry.rank {
            self.ranking.remove(&rank);
        }
        self.bytes -= entry.bytes;
        // A map that held many small entries keeps its slots: give back what few use.
        if self.entries.capacity() > 2 * self.entries.len() + 64 {
            self.entries.shrink_to_fit();
        }
        Some(entry)
    }

    /// Drops the entries of revisions before `revision`.
    fn advance(&mut self, revision: u64) {
        if revision <= self.revision {
            return;
        }
        self.revision = revision;
        let stale: Vec<Key> = self
            .entries
            .keys()
            .filter(|key| key.snapshot.revision < revision)
            .cloned()
            .collect();
        for key in stale {
            self.remove(&key);
        }
    }

    /// The unpinned entries to evict, lowest rank first, so that `bytes` more fit in
    /// `capacity`; `None` if they can't be freed, or (`above`) only by evicting an entry
    /// ranked above it.
    fn victims(&self, bytes: usize, capacity: usize, above: Option<u64>) -> Option<Vec<Key>> {
        let mut needed = (self.bytes + bytes).saturating_sub(capacity);
        let mut victims = Vec::new();
        for ((rank, _), key) in &self.ranking {
            if needed == 0 {
                break;
            }
            if above.is_some_and(|limit| *rank > limit) {
                return None;
            }
            needed = needed.saturating_sub(self.entries[key].bytes);
            victims.push(key.clone());
        }
        (needed == 0).then_some(victims)
    }
}

/// Counts of a [`ResultCache`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResultCacheStats {
    /// Parts answered from the cache.
    pub hits: u64,
    /// Parts computed (that could have been cached).
    pub misses: u64,
    /// Parts taken from another query computing them at the same time.
    pub shared: u64,
    /// Parts admitted.
    pub stored: u64,
    /// Parts not admitted: too cheap to recompute, too large, or worth less than what
    /// they would evict.
    pub rejected: u64,
    pub evicted: u64,
    pub entries: usize,
    pub bytes: usize,
    /// Bytes of pinned entries.
    pub pinned_bytes: usize,
    pub capacity: usize,
}

/// The result cache of a store's queries (module docs). Cheap to share: queries hold it in
/// an `Arc` through [`crate::QueryOptions::result_cache`].
pub struct ResultCache {
    capacity: usize,
    /// Admit every part that fits, however cheap ([`Self::admitting_all`]).
    admit_all: bool,
    state: Mutex<State>,
    hits: AtomicU64,
    misses: AtomicU64,
    shared: AtomicU64,
    stored: AtomicU64,
    rejected: AtomicU64,
    evicted: AtomicU64,
}

impl std::fmt::Debug for ResultCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResultCache")
            .field("stats", &self.stats())
            .finish()
    }
}

/// What a query does about a part ([`ResultCache::claim`]).
pub(crate) enum Claim<'c> {
    Hit(Arc<Part>),
    /// Compute it; others wait for it.
    Compute(Computing<'c>),
    /// Another query computes it.
    Wait(Arc<Flight>),
    /// Compute it without the cache: this thread computes it already (an evaluation
    /// inside its own), or the snapshot is outdated.
    Bypass,
}

/// A part this query computes ([`Claim::Compute`]). Dropped without
/// [`Computing::finish`], it lets its waiters compute the part themselves.
pub(crate) struct Computing<'c> {
    cache: &'c ResultCache,
    key: Key,
    flight: Arc<Flight>,
    finished: bool,
}

impl Computing<'_> {
    /// Whether a part of `bytes` that took `cost` to compute is wanted: the cache would
    /// admit it, or a query waits for it.
    pub(crate) fn wants(&self, bytes: usize, cost: Duration) -> bool {
        self.flight.waiters.load(Ordering::Acquire) > 0
            || self.cache.state().pinned(&self.key.part)
            || self.cache.worth(bytes, cost)
    }

    /// Hands `part` (if it was computed and wanted) to the waiters and offers it to the
    /// cache. An error if the part is pinned but doesn't fit in the budget beside the
    /// other pinned ones.
    pub(crate) fn finish(mut self, part: Option<Part>, cost: Duration) -> Result<(), String> {
        self.finished = true;
        let part = part.map(Arc::new);
        let stored = match &part {
            Some(part) => self.cache.store(&self.key, Arc::clone(part), cost),
            None => Ok(()),
        };
        self.cache.land(&self.key, &self.flight, part);
        stored
    }
}

impl Drop for Computing<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.cache.land(&self.key, &self.flight, None);
        }
    }
}

impl ResultCache {
    /// A cache of `capacity` bytes; 0 keeps nothing (but still computes concurrent
    /// requests for the same part once).
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            admit_all: false,
            state: Mutex::default(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            shared: AtomicU64::new(0),
            stored: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            evicted: AtomicU64::new(0),
        }
    }

    /// This cache admitting every part that takes at most a quarter of the budget,
    /// however cheap to recompute: tests, whose parts take microseconds, use it to
    /// exercise hits.
    pub fn admitting_all(mut self) -> Self {
        self.admit_all = true;
        self
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a part is worth keeping: it took longer than [`MIN_COST`] and than copying
    /// it twice, and takes at most a quarter of the budget.
    fn worth(&self, bytes: usize, cost: Duration) -> bool {
        let micros = cost.as_micros() as u64;
        if self.admit_all {
            return bytes <= self.capacity / 4;
        }
        bytes <= self.capacity / 4
            && cost >= MIN_COST
            && micros.saturating_mul(COPY_BYTES_PER_MICRO) >= 2 * bytes as u64
    }

    /// Looks `key` up; on a miss, the caller computes it or waits for whoever does.
    /// `pin` names the query whose whole result `key` is, to pin it.
    pub(crate) fn claim(&self, key: &Key, pin: Option<&PinRequest>) -> Claim<'_> {
        let mut state = self.state();
        // A query on a newer revision: what older ones computed is unreachable now.
        state.advance(key.snapshot.revision);
        if let Some(pin) = pin {
            state.pins.insert(
                pin.name.clone(),
                Pin {
                    part: Arc::clone(&key.part),
                    query: pin.query.clone(),
                },
            );
            // Unpins what the name pinned before.
            let keys: Vec<Key> = state.entries.keys().cloned().collect();
            for key in keys {
                state.rerank(&key);
            }
        }
        if let Some(part) = self.hit(&mut state, key) {
            return Claim::Hit(part);
        }
        let me = std::thread::current().id();
        if let Some(flight) = state.flights.get(key) {
            if flight.owner == me {
                return Claim::Bypass;
            }
            flight.waiters.fetch_add(1, Ordering::AcqRel);
            return Claim::Wait(Arc::clone(flight));
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        if key.snapshot.revision < state.revision {
            return Claim::Bypass;
        }
        let flight = Arc::new(Flight {
            owner: me,
            waiters: AtomicUsize::new(0),
            done: Mutex::new(None),
            ready: Condvar::new(),
        });
        state.flights.insert(key.clone(), Arc::clone(&flight));
        Claim::Compute(Computing {
            cache: self,
            key: key.clone(),
            flight,
            finished: false,
        })
    }

    /// The part under `key`, if it is cached (for a request that won't store one).
    pub(crate) fn lookup(&self, key: &Key) -> Option<Arc<Part>> {
        self.hit(&mut self.state(), key)
    }

    fn hit(&self, state: &mut State, key: &Key) -> Option<Arc<Part>> {
        let entry = state.entries.get_mut(key)?;
        entry.hits += 1;
        let part = Arc::clone(&entry.part);
        state.rerank(key);
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(part)
    }

    /// Offers a part computed without a claim (a prefix of a basic graph pattern's joins,
    /// which no other query waits for): `part` is made only if one of `bytes` that took
    /// `cost` is worth keeping.
    pub(crate) fn offer(
        &self,
        key: &Key,
        bytes: usize,
        cost: Duration,
        part: impl FnOnce() -> Option<Part>,
    ) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        if !self.worth(bytes, cost) {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if let Some(part) = part() {
            // Not pinned (pins are whole queries): never an error.
            let _ = self.store(key, Arc::new(part), cost);
        }
    }

    /// Counts a part taken from another query's computation.
    pub(crate) fn note_shared(&self) {
        self.shared.fetch_add(1, Ordering::Relaxed);
    }

    fn land(&self, key: &Key, flight: &Arc<Flight>, part: Option<Arc<Part>>) {
        let mut state = self.state();
        if state
            .flights
            .get(key)
            .is_some_and(|f| Arc::ptr_eq(f, flight))
        {
            state.flights.remove(key);
        }
        drop(state);
        flight.finish(part);
    }

    /// Admits `part` if it is worth its bytes (module docs), or pins it.
    fn store(&self, key: &Key, part: Arc<Part>, cost: Duration) -> Result<(), String> {
        let bytes = part.bytes() + key.part.len() + ENTRY_OVERHEAD;
        let cost = cost.as_micros() as u64;
        let mut state = self.state();
        let pinned = state.pinned(&key.part);
        state.advance(key.snapshot.revision);
        let reject = |state: &mut State, reason: Option<String>| {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            match reason {
                Some(reason) => {
                    state.pins.retain(|_, pin| *pin.part != *key.part);
                    Err(reason)
                }
                None => Ok(()),
            }
        };
        if key.snapshot.revision < state.revision || state.entries.contains_key(key) {
            return Ok(());
        }
        let victims = match pinned {
            true => state.victims(bytes, self.capacity, None),
            false if !self.worth(bytes, Duration::from_micros(cost)) => None,
            false if self.admit_all => state.victims(bytes, self.capacity, None),
            false => {
                let rank = state.clock + cost as f64 / bytes.max(1) as f64;
                state.victims(bytes, self.capacity, Some(rank.to_bits()))
            }
        };
        let Some(victims) = victims else {
            let reason = pinned.then(|| {
                format!(
                    "the result takes {bytes} bytes, more than the result cache's budget of \
                     {} bytes leaves beside the other pinned results",
                    self.capacity
                )
            });
            return reject(&mut state, reason);
        };
        for victim in victims {
            if let Some(entry) = state.remove(&victim)
                && let Some((rank, _)) = entry.rank
            {
                state.clock = state.clock.max(f64::from_bits(rank));
                self.evicted.fetch_add(1, Ordering::Relaxed);
            }
        }
        state.bytes += bytes;
        state.entries.insert(
            key.clone(),
            Entry {
                part,
                bytes,
                cost,
                hits: 0,
                rank: None,
            },
        );
        state.rerank(key);
        self.stored.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn stats(&self) -> ResultCacheStats {
        let state = self.state();
        let pinned_bytes = state
            .entries
            .values()
            .filter(|entry| entry.rank.is_none())
            .map(|entry| entry.bytes)
            .sum();
        ResultCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            shared: self.shared.load(Ordering::Relaxed),
            stored: self.stored.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            evicted: self.evicted.load(Ordering::Relaxed),
            entries: state.entries.len(),
            bytes: state.bytes,
            pinned_bytes,
            capacity: self.capacity,
        }
    }

    /// The pinned queries, by name.
    pub fn pins(&self) -> Vec<PinnedResult> {
        let state = self.state();
        state
            .pins
            .iter()
            .map(|(name, pin)| {
                let held = state
                    .entries
                    .iter()
                    .filter(|(key, _)| *key.part == *pin.part)
                    .max_by_key(|(key, _)| key.snapshot.revision);
                PinnedResult {
                    name: name.clone(),
                    query: pin.query.clone(),
                    revision: held.map(|(key, _)| key.snapshot.revision),
                    rows: held.map_or(0, |(_, entry)| entry.part.table.len()),
                    bytes: held.map_or(0, |(_, entry)| entry.bytes),
                }
            })
            .collect()
    }

    /// Unpins `name`'s result (it stays cached as any other); `false` if nothing is pinned
    /// under that name.
    pub fn unpin(&self, name: &str) -> bool {
        let mut state = self.state();
        let Some(pin) = state.pins.remove(name) else {
            return false;
        };
        let keys: Vec<Key> = state
            .entries
            .keys()
            .filter(|key| key.part == pin.part)
            .cloned()
            .collect();
        for key in keys {
            state.rerank(&key);
        }
        true
    }

    /// Drops every entry that isn't pinned.
    pub fn clear(&self) {
        let mut state = self.state();
        let keys: Vec<Key> = state.ranking.values().cloned().collect();
        for key in keys {
            state.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(revision: u64) -> SnapshotIdentity {
        // Snapshots of one engine at successive revisions.
        let engine = nrese_engine::Engine::new(nrese_engine::EngineConfig::default()).unwrap();
        for i in 0..revision {
            let mut tx = engine.transaction();
            tx.insert(nrese_rdf::QuadRef::new(
                nrese_rdf::NamedNodeRef::new_unchecked("http://e/s"),
                nrese_rdf::NamedNodeRef::new_unchecked("http://e/p"),
                nrese_rdf::Literal::from(i as i64).as_ref(),
                nrese_rdf::GraphNameRef::DefaultGraph,
            ));
            tx.commit().unwrap();
        }
        let identity = engine.snapshot().identity();
        assert_eq!(identity.revision, revision);
        identity
    }

    fn key(snapshot: SnapshotIdentity, part: &str) -> Key {
        Key {
            snapshot,
            part: part.as_bytes().into(),
        }
    }

    fn part(rows: usize) -> Part {
        Part {
            vars: vec![0],
            table: IdTable::from_columns(vec![vec![7; rows]]),
            ordered: false,
            computed: Vec::new(),
        }
    }

    fn compute(cache: &ResultCache, key: &Key, rows: usize, cost: Duration) -> bool {
        match cache.claim(key, None) {
            Claim::Hit(_) => true,
            Claim::Compute(computing) => {
                computing.finish(Some(part(rows)), cost).unwrap();
                false
            }
            _ => panic!("no other query runs"),
        }
    }

    const SLOW: Duration = Duration::from_millis(5);

    #[test]
    fn parts_are_kept_per_snapshot_within_the_budget() {
        let one = identity(1);
        let cache = ResultCache::new(64 << 10);
        assert!(!compute(&cache, &key(one, "a"), 100, SLOW));
        assert!(compute(&cache, &key(one, "a"), 100, SLOW), "a hit");
        // Too cheap to recompute: not kept.
        assert!(!compute(
            &cache,
            &key(one, "cheap"),
            100,
            Duration::from_micros(5)
        ));
        assert!(!compute(
            &cache,
            &key(one, "cheap"),
            100,
            Duration::from_micros(5)
        ));
        // Larger than a quarter of the budget: not kept.
        assert!(!compute(&cache, &key(one, "large"), 3_000, SLOW));
        assert!(!compute(&cache, &key(one, "large"), 3_000, SLOW));
        // Many parts: the budget holds, the least beneficial go first.
        for i in 0..100 {
            compute(&cache, &key(one, &format!("q{i}")), 100, SLOW);
            assert!(cache.stats().bytes <= 64 << 10);
        }
        assert!(cache.stats().evicted > 0);
        // A newer revision drops the older entries once it stores one.
        let two = identity(2);
        assert!(!compute(&cache, &key(two, "a"), 100, SLOW));
        assert_eq!(cache.stats().entries, 1);
        // A part computed on an outdated snapshot is not kept.
        assert!(matches!(cache.claim(&key(one, "b"), None), Claim::Bypass));
    }

    #[test]
    fn a_part_worth_less_than_what_it_would_evict_is_not_admitted() {
        let one = identity(1);
        // Room for four parts of 100 rows (1,597 bytes each with the bookkeeping).
        let cache = ResultCache::new(7_000);
        for name in ["a", "b", "c", "d"] {
            compute(&cache, &key(one, name), 100, Duration::from_millis(50));
        }
        let before = cache.stats();
        // As large, much cheaper: it would evict a costlier part.
        compute(&cache, &key(one, "x"), 100, Duration::from_micros(500));
        let after = cache.stats();
        assert_eq!(after.rejected, before.rejected + 1);
        assert_eq!(after.entries, before.entries);
        // As large, costlier: admitted, the cheapest goes.
        compute(&cache, &key(one, "e"), 100, Duration::from_millis(500));
        assert!(cache.stats().evicted > 0);
        assert!(compute(&cache, &key(one, "e"), 100, SLOW));
    }

    #[test]
    fn concurrent_requests_for_a_part_compute_it_once() {
        let one = identity(1);
        let cache = ResultCache::new(1 << 20);
        let key = key(one, "part");
        let threads = 8;
        let computed = AtomicUsize::new(0);
        let claimed = std::sync::Barrier::new(threads);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    // Every thread claims before the part is finished: the one that
                    // computes waits for the others' claims.
                    let claim = cache.claim(&key, None);
                    claimed.wait();
                    match claim {
                        Claim::Compute(computing) => {
                            computed.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(20));
                            computing.finish(Some(part(10)), SLOW).unwrap();
                        }
                        Claim::Wait(flight) => {
                            let part = flight.wait(|| false).unwrap().expect("shared");
                            assert_eq!(part.table.len(), 10);
                        }
                        _ => panic!("nothing cached yet, no outdated snapshot"),
                    }
                });
            }
        });
        assert_eq!(computed.load(Ordering::SeqCst), 1);
        let stats = cache.stats();
        assert_eq!((stats.misses, stats.entries), (1, 1));
    }

    #[test]
    fn waiters_compute_themselves_when_the_computation_fails() {
        let one = identity(1);
        let cache = ResultCache::new(1 << 20);
        let key = key(one, "part");
        let Claim::Compute(computing) = cache.claim(&key, None) else {
            panic!("a miss")
        };
        let flight = std::thread::scope(|scope| {
            scope
                .spawn(|| match cache.claim(&key, None) {
                    Claim::Wait(flight) => flight,
                    _ => panic!("computed elsewhere"),
                })
                .join()
                .unwrap()
        });
        // The same thread asking again (an evaluation inside its own) doesn't wait.
        assert!(matches!(cache.claim(&key, None), Claim::Bypass));
        drop(computing);
        assert!(flight.wait(|| false).unwrap().is_none());
        assert!(matches!(cache.claim(&key, None), Claim::Compute(_)));
    }

    #[test]
    fn pinned_results_stay_until_unpinned() {
        let one = identity(1);
        let cache = ResultCache::new(8_000);
        let pin = PinRequest {
            name: "big".into(),
            query: "SELECT …".into(),
        };
        // Pinned: kept though cheap.
        let Claim::Compute(computing) = cache.claim(&key(one, "pinned"), Some(&pin)) else {
            panic!("a miss")
        };
        computing
            .finish(Some(part(200)), Duration::from_micros(1))
            .unwrap();
        for i in 0..50 {
            compute(&cache, &key(one, &format!("q{i}")), 100, SLOW);
        }
        assert!(compute(&cache, &key(one, "pinned"), 200, SLOW));
        let pins = cache.pins();
        assert_eq!(pins.len(), 1);
        assert_eq!((pins[0].revision, pins[0].rows), (Some(1), 200));
        assert!(cache.stats().pinned_bytes > 1_600);
        // Too large to pin beside the budget: an error, and no pin.
        let huge = PinRequest {
            name: "huge".into(),
            query: "SELECT …".into(),
        };
        let Claim::Compute(computing) = cache.claim(&key(one, "huge"), Some(&huge)) else {
            panic!("a miss")
        };
        assert!(computing.finish(Some(part(2_000)), SLOW).is_err());
        assert_eq!(cache.pins().len(), 1);
        // Unpinned, it is ranked like any other entry.
        assert!(cache.unpin("big"));
        assert!(!cache.unpin("big"));
        assert_eq!(cache.stats().pinned_bytes, 0);
        cache.clear();
        assert_eq!(cache.stats().entries, 0);
    }
}
