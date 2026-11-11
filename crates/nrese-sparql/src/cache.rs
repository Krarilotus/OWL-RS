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
//! even if it isn't admitted, and computes it itself if the other query fails. A request
//! on a Rayon worker never waits: it computes the part itself (research-designs.md §8).
//!
//! **Serialised answers.** A query's whole answer is also kept as the bytes of each
//! output format it was asked in ([`ResultCache::output`], [`CachedOutput`]), beside its
//! id columns, so a repeated query with a large answer is copied instead of serialised
//! again. They share the budget and the ranking; their cost is the time the request took
//! (what the cache didn't answer of the evaluation, and the serialisation), so they are
//! kept where writing the answer costs much more than copying it.
//!
//! **Pinned.** A named query's result can be pinned ([`ResultCache::pins`]): it is never
//! evicted while its revision is current, and counts against the budget. After a commit it
//! is pinned again the next time it is computed.
//!
//! **Published state only** (the lifecycle contract's S3). Everything that changes what
//! other queries find (a claim, a pin, a revision's advance, an offer, an output slot, the
//! insert and its evictions) takes the engine's [`Admission`] of the snapshot computed on,
//! and the key it stores under has that snapshot's identity. The store's cache admits only
//! a published snapshot's (`Shared`): a writer's pending state, a speculative or private
//! one, never reaches it. Looking a key up needs no admission.
//!
//! **An unpublished version's own cache.** What an unpublished snapshot computes is kept in
//! a cache its version owns ([`ResultCache::for_admission`]; the engine's `OwnState`): a
//! session reading its kept pending view again is answered from it, and nothing else reaches
//! it. It has the store's cache's capacity and policy, holds no pin, and everything it keeps
//! is charged to a part of the envelope of the request that made it, refused when that is
//! full and released when the version's last holder drops it.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::Duration;

use nrese_engine::{Admission, Snapshot, SnapshotIdentity};
use nrese_exec::IdTable;
use nrese_exec::{Budget, Envelope, Reservation};
use nrese_rdf::Term;

/// A part computed faster than this isn't kept: a hit would save less than looking it
/// up costs. An answer's bytes have no such floor (a hit replaces the whole request).
pub const MIN_COST: Duration = Duration::from_micros(20);

/// Bytes a hit copies per microsecond: a part must have taken longer to compute than its
/// copy takes, twice over. 10 GB/s, a fifth of what the main PC measured copying a cached
/// answer of 63.5 MB (1.2 ms, 6 October 2026); 2 GB/s before, which refused serialised
/// answers costing three times their copy.
pub const COPY_BYTES_PER_MICRO: u64 = 10_000;

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

impl Key {
    /// The key `part` is admitted under: the identity of the admission's own snapshot. Every
    /// key the cache stores, claims or pins under is made here.
    fn admitted(admission: Admission<'_>, part: Arc<[u8]>) -> Self {
        Self {
            snapshot: of(admission).identity(),
            part,
        }
    }
}

/// The snapshot `admission` was taken from.
fn of(admission: Admission<'_>) -> &Snapshot {
    match admission {
        Admission::Shared(state) => state.snapshot(),
        Admission::Own(own) => own.snapshot(),
    }
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
pub fn term_heap(term: &Term) -> usize {
    match term {
        Term::NamedNode(n) => n.as_str().len(),
        Term::BlankNode(b) => b.as_str().len(),
        Term::Literal(l) => {
            l.value().len() + l.datatype().as_str().len() + l.language().map_or(0, str::len)
        }
        // Its box and what that holds, counted without writing the term out (D16-039: a
        // nested term's text written out only to be sized was itself an uncharged copy).
        Term::Triple(_) => term.heap_bytes(),
    }
}

/// What an entry holds.
#[derive(Debug, Clone)]
pub(crate) enum Payload {
    /// A part's result as id columns.
    Part(Arc<Part>),
    /// A whole query's answer in one output format.
    Output(Arc<[u8]>),
}

impl Payload {
    fn bytes(&self) -> usize {
        match self {
            Self::Part(part) => part.bytes(),
            Self::Output(bytes) => bytes.len(),
        }
    }
}

struct Entry {
    payload: Payload,
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
    fn new(owner: ThreadId) -> Arc<Self> {
        Arc::new(Self {
            owner,
            waiters: AtomicUsize::new(0),
            done: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

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
    /// An unpublished version's own cache: the bytes of its entries, charged to its budget
    /// ([`ResultCache::owned`]); `None` for the store's.
    held: Option<Reservation>,
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
        let Some((resident, entry)) = self.entries.get_key_value(key) else {
            return;
        };
        let (cost, hits, bytes, old) = (entry.cost, entry.hits, entry.bytes, entry.rank);
        if let Some(old) = old {
            self.ranking.remove(&old);
        }
        let rank = match self.pinned(&key.part) {
            true => None,
            false => {
                // The entry's own key: the caller's holds a context of its own, which the
                // budget doesn't count.
                let resident = resident.clone();
                let rank = self.rank(cost, hits, bytes);
                self.ranking.insert(rank, resident);
                Some(rank)
            }
        };
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
        if let Some(held) = &mut self.held {
            held.release(entry.bytes);
        }
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
    /// Entries holding a whole answer's serialised bytes (the others hold id columns).
    pub answers: usize,
    pub capacity: usize,
}

/// The result cache of a store's queries (module docs). Cheap to share: queries hold it in
/// an `Arc` through [`crate::QueryOptions::result_cache`].
pub struct ResultCache {
    capacity: usize,
    /// Admit every part that fits, however cheap ([`Self::admitting_all`]).
    admit_all: bool,
    /// An unpublished version's own cache: what its entries are charged to
    /// ([`Self::owned`]); `None` for the store's.
    budget: Option<Arc<Budget>>,
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

/// A query's whole answer in one output format, as the result cache holds it
/// ([`crate::query::cached_output`]), for the snapshot `'s` borrows.
pub enum CachedOutput<'s> {
    /// The answer's bytes, as written before.
    Hit(Arc<[u8]>),
    /// Not cached: write the answer, then offer its bytes to the slot.
    Miss(OutputSlot<'s>),
    /// Not cached and not to be (no cache, a transaction view, a volatile query, a pin).
    Off,
}

/// Where a written answer is offered to the result cache ([`CachedOutput::Miss`]): it holds
/// the admission of the snapshot it was made for, so its offer admits for that snapshot and
/// no other, into the cache that admission reaches, and it can't outlive the snapshot.
///
/// ```
/// # use nrese_sparql::{CachedOutput, QueryOptions, ResultCache, cached_output};
/// let engine = nrese_engine::Engine::new(Default::default()).unwrap();
/// let options = QueryOptions {
///     result_cache: Some(std::sync::Arc::new(ResultCache::new(1 << 20))),
///     ..QueryOptions::default()
/// };
/// let query = nrese_sparql_syntax::SparqlParser::new()
///     .parse_query("SELECT * WHERE { ?s ?p ?o }")
///     .unwrap();
/// let snapshot = engine.snapshot();
/// let slot = match cached_output(&snapshot, &query, &options, "text/csv", "") {
///     CachedOutput::Miss(slot) => slot,
///     _ => unreachable!("nothing is cached yet"),
/// };
/// slot.offer(b"s,p,o\r\n".to_vec(), std::time::Duration::from_millis(1));
/// ```
///
/// Kept past its snapshot, it doesn't compile:
///
/// ```compile_fail
/// # use nrese_sparql::{CachedOutput, QueryOptions, ResultCache, cached_output};
/// let engine = nrese_engine::Engine::new(Default::default()).unwrap();
/// let options = QueryOptions {
///     result_cache: Some(std::sync::Arc::new(ResultCache::new(1 << 20))),
///     ..QueryOptions::default()
/// };
/// let query = nrese_sparql_syntax::SparqlParser::new()
///     .parse_query("SELECT * WHERE { ?s ?p ?o }")
///     .unwrap();
/// let slot = {
///     let snapshot = engine.snapshot();
///     match cached_output(&snapshot, &query, &options, "text/csv", "") {
///         CachedOutput::Miss(slot) => slot,
///         _ => unreachable!("nothing is cached yet"),
///     }
/// };
/// slot.offer(b"s,p,o\r\n".to_vec(), std::time::Duration::from_millis(1));
/// ```
pub struct OutputSlot<'s> {
    cache: Arc<ResultCache>,
    state: Admission<'s>,
    part: Arc<[u8]>,
}

impl OutputSlot<'_> {
    /// The largest answer the cache could keep: a copy of more isn't worth making.
    pub fn limit(&self) -> usize {
        self.cache.capacity / 4
    }

    /// Offers the answer's `bytes`, which took `cost` to evaluate and write: kept if that
    /// is worth their bytes (module docs).
    pub fn offer(self, bytes: Vec<u8>, cost: Duration) {
        // Not a part computed: no miss (the query's parts counted theirs). No floor: a hit
        // replaces the whole request, whose parsing and key it pays as well.
        let len = bytes.len();
        let key = Key::admitted(self.state, self.part);
        self.cache.admit(&key, len, cost, Duration::ZERO, || {
            Some(Payload::Output(bytes.into()))
        });
    }
}

/// What a query does about a part ([`ResultCache::claim`]).
pub(crate) enum Claim<'c> {
    Hit(Arc<Part>),
    /// Compute it; others wait for it, unless another query computes it already and this
    /// one is on a Rayon worker, which never waits.
    Compute(Computing<'c>),
    /// Another query computes it.
    Wait(Arc<Flight>),
    /// Compute it without the cache: this thread computes it already (an evaluation
    /// inside its own), or the snapshot is outdated.
    Bypass,
}

/// A part this query computes ([`Claim::Compute`]), for the snapshot whose admission
/// claimed it: its key is that snapshot's, and it finishes for it alone. Dropped without
/// [`Computing::finish`], it lets its waiters compute the part themselves.
pub(crate) struct Computing<'c> {
    cache: &'c ResultCache,
    /// The admission it was claimed with: its key is this snapshot's ([`Self::key`]).
    state: Admission<'c>,
    part: Arc<[u8]>,
    flight: Arc<Flight>,
    finished: bool,
}

impl Computing<'_> {
    fn key(&self) -> Key {
        Key::admitted(self.state, Arc::clone(&self.part))
    }

    /// Whether a part of `bytes` that took `cost` to compute is wanted: the cache would
    /// admit it, or a query waits for it.
    pub(crate) fn wants(&self, bytes: usize, cost: Duration) -> bool {
        self.flight.waiters.load(Ordering::Acquire) > 0
            || self.cache.state().pinned(&self.part)
            || self.cache.worth(bytes, cost, MIN_COST)
    }

    /// Hands `part` (if it was computed and wanted) to the waiters and offers it to the
    /// cache. An error if the part is pinned but doesn't fit in the budget beside the
    /// other pinned ones.
    pub(crate) fn finish(mut self, part: Option<Part>, cost: Duration) -> Result<(), String> {
        self.finished = true;
        let key = self.key();
        let part = part.map(Arc::new);
        let stored = match &part {
            Some(part) => self
                .cache
                .store(&key, Payload::Part(Arc::clone(part)), cost),
            None => Ok(()),
        };
        self.cache.land(&key, &self.flight, part);
        stored
    }
}

impl Drop for Computing<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.cache.land(&self.key(), &self.flight, None);
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
            budget: None,
            state: Mutex::default(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            shared: AtomicU64::new(0),
            stored: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            evicted: AtomicU64::new(0),
        }
    }

    /// An unpublished version's own cache beside the store's `shared` one (module docs): its
    /// capacity and admission policy, everything it keeps charged to `budget`.
    pub(crate) fn owned(shared: &ResultCache, budget: Arc<Budget>) -> Self {
        let state = State {
            held: Some(Reservation::new(Arc::clone(&budget))),
            ..State::default()
        };
        Self {
            admit_all: shared.admit_all,
            budget: Some(budget),
            state: Mutex::new(state),
            ..Self::new(shared.capacity)
        }
    }

    /// The cache `admission` admits into, beside the store's `shared` one: that one for a
    /// published snapshot; for an unpublished one, its version's own, made on first use with
    /// a part of `memory` (the envelope of the request making it) as its budget.
    pub(crate) fn for_admission(
        shared: &Arc<ResultCache>,
        admission: Admission<'_>,
        memory: &Envelope,
    ) -> Arc<ResultCache> {
        match admission {
            Admission::Shared(_) => Arc::clone(shared),
            Admission::Own(own) => {
                own.get_or_insert_with(|| ResultCache::owned(shared, Arc::new(memory.part())))
            }
        }
    }

    /// Whether `bytes` more fit the budget of a version's own cache (always, for the store's).
    fn room(&self, bytes: usize) -> bool {
        self.budget
            .as_ref()
            .is_none_or(|budget| budget.remaining() >= bytes)
    }

    /// Whether this cache admits for `admission`: the store's for a published snapshot, a
    /// version's own for an unpublished one. No caller passes the other kind (the scope and
    /// every admission come from one context's snapshot); one that did is refused.
    fn admits(&self, admission: Admission<'_>) -> bool {
        let fits = matches!(
            (self.budget.is_some(), admission),
            (false, Admission::Shared(_)) | (true, Admission::Own(_))
        );
        debug_assert!(fits, "an admission of the other kind");
        fits
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

    /// Whether an entry is worth keeping: it took longer than `floor` and than copying it
    /// twice, and takes at most a quarter of the budget.
    fn worth(&self, bytes: usize, cost: Duration, floor: Duration) -> bool {
        let micros = cost.as_micros() as u64;
        if self.admit_all {
            return bytes <= self.capacity / 4;
        }
        bytes <= self.capacity / 4
            && cost >= floor
            && micros.saturating_mul(COPY_BYTES_PER_MICRO) >= 2 * bytes as u64
    }

    /// Looks `part` up on the admission's snapshot; on a miss, the caller computes it or
    /// waits for whoever does. `pin` names the query whose whole result `part` is, to pin it
    /// (the store's cache alone holds pins: a pin on unpublished state is refused before it
    /// is claimed, [`crate::native`]'s `eval_root`).
    pub(crate) fn claim<'c>(
        &'c self,
        token: Admission<'c>,
        part: &Arc<[u8]>,
        pin: Option<&PinRequest>,
    ) -> Claim<'c> {
        if !self.admits(token) {
            return Claim::Bypass;
        }
        let key = &Key::admitted(token, Arc::clone(part));
        let mut state = self.state();
        // A query on a newer revision: what older ones computed is unreachable now.
        state.advance(key.snapshot.revision);
        if let Some(pin) = pin.filter(|_| self.budget.is_none()) {
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
        if let Some(Payload::Part(part)) = self.hit(&mut state, key) {
            return Claim::Hit(part);
        }
        let me = std::thread::current().id();
        let computing = |flight| {
            Claim::Compute(Computing {
                cache: self,
                state: token,
                part: Arc::clone(part),
                flight,
                finished: false,
            })
        };
        if let Some(flight) = state.flights.get(key) {
            if flight.owner == me {
                return Claim::Bypass;
            }
            // A Rayon worker never blocks on a flight: the stack it holds may be what the
            // flight's producer waits for (research-designs.md §8). It computes the part
            // beside the flight, under a flight of its own that nobody finds, and offers
            // the result like any other.
            if rayon::current_thread_index().is_some() {
                self.misses.fetch_add(1, Ordering::Relaxed);
                return computing(Flight::new(me));
            }
            flight.waiters.fetch_add(1, Ordering::AcqRel);
            return Claim::Wait(Arc::clone(flight));
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        if key.snapshot.revision < state.revision {
            return Claim::Bypass;
        }
        let flight = Flight::new(me);
        state.flights.insert(key.clone(), Arc::clone(&flight));
        computing(flight)
    }

    /// The part under `key`, if it is cached (for a request that won't store one).
    pub(crate) fn lookup(&self, key: &Key) -> Option<Arc<Part>> {
        match self.hit(&mut self.state(), key)? {
            Payload::Part(part) => Some(part),
            Payload::Output(_) => None,
        }
    }

    fn hit(&self, state: &mut State, key: &Key) -> Option<Payload> {
        let entry = state.entries.get_mut(key)?;
        entry.hits += 1;
        let payload = entry.payload.clone();
        state.rerank(key);
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(payload)
    }

    /// A query's whole answer on the admission's snapshot in the format `part` names: the
    /// bytes if cached, else a slot to offer them to once written
    /// ([`crate::query::cached_output`]).
    pub(crate) fn output<'s>(
        self: &Arc<Self>,
        token: Admission<'s>,
        part: Arc<[u8]>,
    ) -> CachedOutput<'s> {
        if !self.admits(token) {
            return CachedOutput::Off;
        }
        match self.hit(&mut self.state(), &Key::admitted(token, Arc::clone(&part))) {
            Some(Payload::Output(bytes)) => CachedOutput::Hit(bytes),
            _ => CachedOutput::Miss(OutputSlot {
                cache: Arc::clone(self),
                state: token,
                part,
            }),
        }
    }

    /// Offers a part computed on the admission's snapshot without a claim (a prefix of a
    /// basic graph pattern's joins, which no other query waits for): `payload` is made only
    /// if one of `bytes` that took `cost` is worth keeping.
    pub(crate) fn offer(
        &self,
        token: Admission<'_>,
        part: &Arc<[u8]>,
        bytes: usize,
        cost: Duration,
        payload: impl FnOnce() -> Option<Payload>,
    ) {
        if !self.admits(token) {
            return;
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let key = Key::admitted(token, Arc::clone(part));
        self.admit(&key, bytes, cost, MIN_COST, payload);
    }

    /// Stores what `payload` makes if one of `bytes` that took `cost` (at least `floor`)
    /// is worth keeping.
    fn admit(
        &self,
        key: &Key,
        bytes: usize,
        cost: Duration,
        floor: Duration,
        payload: impl FnOnce() -> Option<Payload>,
    ) {
        // Nothing is made for a version's own cache whose budget can't take it.
        if !self.worth(bytes, cost, floor) || !self.room(bytes) {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if let Some(payload) = payload() {
            // Not pinned (pins are whole queries' id columns): never an error.
            let _ = self.store(key, payload, cost);
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
    fn store(&self, key: &Key, payload: Payload, cost: Duration) -> Result<(), String> {
        let bytes = payload.bytes() + key.part.len() + ENTRY_OVERHEAD;
        let cost = cost.as_micros() as u64;
        let mut state = self.state();
        let pinned = state.pinned(&key.part);
        let floor = match payload {
            Payload::Part(_) => MIN_COST,
            Payload::Output(_) => Duration::ZERO,
        };
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
            false if !self.worth(bytes, Duration::from_micros(cost), floor) => None,
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
        // A version's own cache keeps what its budget takes. Checked first, so that the
        // envelope it draws from, another request's, isn't marked refused by a charge that
        // can't fit (`Budget::refused`, which the DL status path reads).
        if !self.room(bytes) {
            return reject(&mut state, None);
        }
        if let Some(held) = &mut state.held
            && held.charge(bytes).is_err()
        {
            return reject(&mut state, None);
        }
        state.bytes += bytes;
        state.entries.insert(
            key.clone(),
            Entry {
                payload,
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
        let answers = state
            .entries
            .values()
            .filter(|entry| matches!(entry.payload, Payload::Output(_)))
            .count();
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
            answers,
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
                    rows: held.map_or(0, |(_, entry)| match &entry.payload {
                        Payload::Part(part) => part.table.len(),
                        Payload::Output(_) => 0,
                    }),
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
    use std::sync::atomic::AtomicBool;

    /// A published snapshot at `revision` (one engine's, after that many commits).
    fn published(revision: u64) -> nrese_engine::Snapshot {
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
        let snapshot = engine.snapshot();
        assert_eq!(snapshot.revision(), revision);
        snapshot
    }

    /// A part on a snapshot: what a query admits it with (its snapshot's admission) and looks
    /// it up by ([`Self::key`]).
    struct Probe {
        snapshot: nrese_engine::Snapshot,
        part: Arc<[u8]>,
    }

    impl Probe {
        fn key(&self) -> Key {
            Key {
                snapshot: self.snapshot.identity(),
                part: Arc::clone(&self.part),
            }
        }

        fn token(&self) -> Admission<'_> {
            self.snapshot.admission()
        }
    }

    /// A writer's pending snapshot: unpublished, so admitted into a version's own cache.
    fn pending() -> nrese_engine::Snapshot {
        let engine = nrese_engine::Engine::new(nrese_engine::EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        tx.insert(nrese_rdf::QuadRef::new(
            nrese_rdf::NamedNodeRef::new_unchecked("http://e/s"),
            nrese_rdf::NamedNodeRef::new_unchecked("http://e/p"),
            nrese_rdf::Literal::from(1_i64).as_ref(),
            nrese_rdf::GraphNameRef::DefaultGraph,
        ));
        let pending = tx.pending_snapshot();
        assert!(matches!(pending.admission(), Admission::Own(_)));
        pending
    }

    fn key(snapshot: &nrese_engine::Snapshot, part: &str) -> Probe {
        Probe {
            snapshot: snapshot.clone(),
            part: part.as_bytes().into(),
        }
    }

    fn claim<'c>(cache: &'c ResultCache, at: &'c Probe, pin: Option<&PinRequest>) -> Claim<'c> {
        cache.claim(at.token(), &at.part, pin)
    }

    fn output<'s>(cache: &Arc<ResultCache>, at: &'s Probe) -> CachedOutput<'s> {
        cache.output(at.token(), Arc::clone(&at.part))
    }

    fn part(rows: usize) -> Part {
        Part {
            vars: vec![0],
            table: IdTable::from_columns(vec![vec![7; rows]]),
            ordered: false,
            computed: Vec::new(),
        }
    }

    fn compute(cache: &ResultCache, key: &Probe, rows: usize, cost: Duration) -> bool {
        match claim(cache, key, None) {
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
        let one = published(1);
        let cache = ResultCache::new(64 << 10);
        assert!(!compute(&cache, &key(&one, "a"), 100, SLOW));
        assert!(compute(&cache, &key(&one, "a"), 100, SLOW), "a hit");
        // Too cheap to recompute: not kept.
        assert!(!compute(
            &cache,
            &key(&one, "cheap"),
            100,
            Duration::from_micros(5)
        ));
        assert!(!compute(
            &cache,
            &key(&one, "cheap"),
            100,
            Duration::from_micros(5)
        ));
        // Larger than a quarter of the budget: not kept.
        assert!(!compute(&cache, &key(&one, "large"), 3_000, SLOW));
        assert!(!compute(&cache, &key(&one, "large"), 3_000, SLOW));
        // Many parts: the budget holds, the least beneficial go first.
        for i in 0..100 {
            compute(&cache, &key(&one, &format!("q{i}")), 100, SLOW);
            assert!(cache.stats().bytes <= 64 << 10);
        }
        assert!(cache.stats().evicted > 0);
        // A newer revision drops the older entries once it stores one.
        let two = published(2);
        assert!(!compute(&cache, &key(&two, "a"), 100, SLOW));
        assert_eq!(cache.stats().entries, 1);
        // A part computed on an outdated snapshot is not kept.
        assert!(matches!(
            claim(&cache, &key(&one, "b"), None),
            Claim::Bypass
        ));
    }

    /// Item 7 of the token design check (F3): a part claimed on a published snapshot and
    /// finished after a newer revision stored its own is kept for neither: not as the newer
    /// revision's (its key is its own snapshot's), nor under its old revision.
    #[test]
    fn a_part_finished_after_a_newer_revision_is_kept_for_neither() {
        let (one, two) = (published(1), published(2));
        let cache = ResultCache::new(1 << 20);
        let old = key(&one, "part");
        let Claim::Compute(computing) = claim(&cache, &old, None) else {
            panic!("a miss")
        };
        let new = key(&two, "part");
        assert!(!compute(&cache, &new, 10, SLOW));
        computing.finish(Some(part(20)), SLOW).unwrap();
        assert!(
            cache.lookup(&old.key()).is_none(),
            "not kept for revision 1"
        );
        assert_eq!(cache.lookup(&new.key()).unwrap().table.len(), 10);
        assert_eq!(cache.stats().entries, 1);
        assert!(cache.state().flights.is_empty());
    }

    /// 3.6's design note §11, O4 and O5: a version's own cache keeps what its budget takes.
    ///
    /// - Input: an own cache charged to a part of a 4,000-byte envelope (about two parts of
    ///   100 rows, 1,597 bytes each with their bookkeeping); four such parts computed on a
    ///   pending snapshot; then `clear`; then the cache dropped.
    /// - Expected: two are kept and the others refused; the envelope holds exactly what the
    ///   cache holds, and isn't marked refused (another request's, whose status reads it);
    ///   `clear` and the drop give it all back.
    #[test]
    fn a_versions_own_cache_keeps_what_its_budget_takes() {
        let view = pending();
        let envelope = Envelope::new(Some(4_000), None);
        let store = ResultCache::new(1 << 20);
        let own = ResultCache::owned(&store, Arc::new(envelope.part()));
        for i in 0..4 {
            compute(&own, &key(&view, &format!("q{i}")), 100, SLOW);
        }
        let stats = own.stats();
        assert_eq!((stats.entries, stats.rejected), (2, 2), "{stats:?}");
        assert_eq!(envelope.used(), stats.bytes, "charged as held");
        assert!(!envelope.refused(), "no refusal recorded on the envelope");
        assert_eq!(store.stats().entries, 0, "the store's cache holds nothing");
        own.clear();
        assert_eq!(envelope.used(), 0, "cleared, released");
        compute(&own, &key(&view, "again"), 100, SLOW);
        assert!(envelope.used() > 0);
        drop(own);
        assert_eq!(envelope.used(), 0, "dropped with its version, released");
    }

    /// 3.6's design note §11, O7: the store's cache admits only published state, a version's
    /// own cache only unpublished state; the other kind is refused (and asserts in debug
    /// builds; no caller passes one).
    #[test]
    fn each_cache_admits_only_its_own_kind() {
        let (published, pending) = (published(1), pending());
        let store = ResultCache::new(1 << 20);
        let own = ResultCache::owned(&store, Arc::new(Envelope::unlimited().part()));
        assert!(store.admits(published.admission()) && own.admits(pending.admission()));
        let wrong = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (
                store.admits(pending.admission()),
                own.admits(published.admission()),
            )
        }));
        match wrong {
            // Debug builds assert; anything else is a failure of its own.
            Err(_) if cfg!(debug_assertions) => {}
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(admitted) => assert_eq!(admitted, (false, false)),
        }
    }

    #[test]
    fn a_part_worth_less_than_what_it_would_evict_is_not_admitted() {
        let one = published(1);
        // Room for four parts of 100 rows (1,597 bytes each with the bookkeeping).
        let cache = ResultCache::new(7_000);
        for name in ["a", "b", "c", "d"] {
            compute(&cache, &key(&one, name), 100, Duration::from_millis(50));
        }
        let before = cache.stats();
        // As large, much cheaper: it would evict a costlier part.
        compute(&cache, &key(&one, "x"), 100, Duration::from_micros(500));
        let after = cache.stats();
        assert_eq!(after.rejected, before.rejected + 1);
        assert_eq!(after.entries, before.entries);
        // As large, costlier: admitted, the cheapest goes.
        compute(&cache, &key(&one, "e"), 100, Duration::from_millis(500));
        assert!(cache.stats().evicted > 0);
        assert!(compute(&cache, &key(&one, "e"), 100, SLOW));
    }

    /// - Input: a part computed on a miss in 1 µs, under [`MIN_COST`]: the computing query
    ///   isn't wanted to make it ([`Computing::wants`]) and finishes without it.
    /// - Expected: counted as refused, as `store` counts a cheap part it is given: a part
    ///   computed on a miss is either admitted or counted in `rejected`.
    #[test]
    fn a_part_too_cheap_to_be_wanted_is_counted_as_refused() {
        let one = published(1);
        let cache = ResultCache::new(1 << 20);
        let at = key(&one, "cheap");
        let cost = Duration::from_micros(1);
        let before = cache.stats();
        let Claim::Compute(computing) = claim(&cache, &at, None) else {
            panic!("no other query runs");
        };
        assert!(!computing.wants(part(10).table.memory_bytes(), cost));
        computing.finish(None, cost).unwrap();
        let after = cache.stats();
        assert_eq!(
            (after.misses, after.stored, after.rejected),
            (before.misses + 1, before.stored, before.rejected + 1),
            "{after:?}"
        );
    }

    #[test]
    fn concurrent_requests_for_a_part_compute_it_once() {
        let one = published(1);
        let cache = ResultCache::new(1 << 20);
        let key = key(&one, "part");
        let threads = 8;
        let computed = AtomicUsize::new(0);
        let claimed = std::sync::Barrier::new(threads);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    // Every thread claims before the part is finished: the one that
                    // computes waits for the others' claims.
                    let claim = claim(&cache, &key, None);
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

    /// Runs `f` with a watchdog that raises the flag `f`'s waits poll after five seconds;
    /// returns `f`'s result and whether the watchdog had to raise it.
    fn watched<R>(f: impl FnOnce(&AtomicBool) -> R) -> (R, bool) {
        let cancel = AtomicBool::new(false);
        let (done, finished) = std::sync::mpsc::channel::<()>();
        std::thread::scope(|scope| {
            let flag = &cancel;
            let watchdog = scope.spawn(move || {
                let rescued = finished.recv_timeout(Duration::from_secs(5)).is_err();
                flag.store(rescued, Ordering::SeqCst);
                rescued
            });
            let result = f(&cancel);
            let _ = done.send(());
            (result, watchdog.join().unwrap())
        })
    }

    /// A query's request for `key` (10 rows when it computes them), as `eval_cached`
    /// makes it; `Err` if the watchdog had to stop its wait.
    fn request(
        cache: &ResultCache,
        key: &Probe,
        pin: Option<&PinRequest>,
        cancel: &AtomicBool,
    ) -> Result<usize, ()> {
        match claim(cache, key, pin) {
            Claim::Hit(part) => Ok(part.table.len()),
            Claim::Compute(computing) => {
                computing.finish(Some(part(10)), SLOW).unwrap();
                Ok(10)
            }
            Claim::Wait(flight) => flight
                .wait(|| cancel.load(Ordering::SeqCst))
                .map(|part| part.map_or(0, |part| part.table.len())),
            Claim::Bypass => panic!("another query computes the part"),
        }
    }

    fn pool(threads: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    }

    /// The forced reproduction of research-designs.md §8 (`flight-diagnostic.patch`,
    /// campaign-20261010-bebada2), which hung until its watchdog cancelled the waiter: a
    /// producer on a worker of a two-worker pool claims a part and joins a kernel; the
    /// kernel's helper, yielding, runs another query's request for that part. That
    /// request now computes the part itself instead of blocking the producer's join.
    #[test]
    fn a_request_stolen_by_its_producers_kernel_computes_the_part_itself() {
        use std::sync::OnceLock;
        let workers = pool(2);
        let cache = ResultCache::new(1 << 20);
        let key = key(&published(1), "nested-flight");
        let (helper_ready, waiter_entered) = (AtomicBool::new(false), AtomicBool::new(false));
        let (producer, helper, waiter) = (OnceLock::new(), OnceLock::new(), OnceLock::new());
        let (outcome, rescued) = watched(|cancel| {
            let stop = || cancel.load(Ordering::SeqCst);
            std::thread::scope(|scope| {
                let request = scope.spawn(|| {
                    while !helper_ready.load(Ordering::Acquire) && !stop() {
                        std::thread::yield_now();
                    }
                    workers.install(|| {
                        waiter.set(std::thread::current().id()).unwrap();
                        waiter_entered.store(true, Ordering::Release);
                        request(&cache, &key, None, cancel)
                    })
                });
                workers.install(|| {
                    producer.set(std::thread::current().id()).unwrap();
                    let Claim::Compute(computing) = claim(&cache, &key, None) else {
                        panic!("the first request computes the part");
                    };
                    rayon::join(
                        || {
                            while !waiter_entered.load(Ordering::Acquire) && !stop() {
                                std::thread::yield_now();
                            }
                        },
                        || {
                            helper.set(std::thread::current().id()).unwrap();
                            helper_ready.store(true, Ordering::Release);
                            while !waiter_entered.load(Ordering::Acquire) && !stop() {
                                if rayon::yield_now() != Some(rayon::Yield::Executed) {
                                    std::thread::yield_now();
                                }
                            }
                        },
                    );
                    computing.finish(Some(part(10)), SLOW).unwrap();
                });
                request.join().unwrap()
            })
        });
        assert!(!rescued, "progress needed the watchdog: {outcome:?}");
        assert_eq!(outcome, Ok(10));
        assert_ne!(producer.get(), helper.get());
        assert_eq!(helper.get(), waiter.get(), "the helper ran the request");
        assert_eq!(cache.lookup(&key.key()).unwrap().table.len(), 10);
        assert!(cache.state().flights.is_empty());
    }

    /// The one-worker variant: a query computing a part on its caller runs a kernel on a
    /// pool's only worker, which asks for the same part (a callback evaluating a cacheable
    /// query). Waiting, the worker and the caller would wait for each other.
    #[test]
    fn a_kernel_on_one_worker_computes_the_part_its_caller_computes() {
        let cache = ResultCache::new(1 << 20);
        let key = key(&published(1), "one-worker");
        let Claim::Compute(computing) = claim(&cache, &key, None) else {
            panic!("a miss")
        };
        let (outcome, rescued) =
            watched(|cancel| pool(1).install(|| request(&cache, &key, None, cancel)));
        assert!(!rescued, "progress needed the watchdog: {outcome:?}");
        assert_eq!(outcome, Ok(10));
        computing.finish(Some(part(10)), SLOW).unwrap();
        assert!(cache.state().flights.is_empty());
        assert_eq!(cache.stats().entries, 1, "computed twice, kept once");
    }

    /// The nested-kernel variant: requests in kernels nested within a kernel, on several
    /// workers, one of them pinning the part; and a request outside any pool still waits
    /// for the computation (computed once).
    #[test]
    fn requests_in_nested_kernels_progress_and_pin() {
        let cache = ResultCache::new(1 << 20);
        let key = key(&published(1), "nested-kernels");
        let pin = PinRequest {
            name: "nested".into(),
            query: "SELECT …".into(),
        };
        let Claim::Compute(computing) = claim(&cache, &key, None) else {
            panic!("a miss")
        };
        let ((outcomes, waited), rescued) = watched(|cancel| {
            std::thread::scope(|scope| {
                // A caller, not a worker: it waits for the computation. It claims before
                // the kernels run, which may store the part beside the flight.
                let waiting = scope.spawn(|| request(&cache, &key, None, cancel));
                while cache
                    .state()
                    .flights
                    .get(&key.key())
                    .is_some_and(|flight| flight.waiters.load(Ordering::Acquire) == 0)
                    && !cancel.load(Ordering::SeqCst)
                {
                    std::thread::yield_now();
                }
                let outcomes = pool(2).install(|| {
                    let ((a, b), c) = rayon::join(
                        || {
                            rayon::join(
                                || request(&cache, &key, None, cancel),
                                || request(&cache, &key, Some(&pin), cancel),
                            )
                        },
                        || request(&cache, &key, None, cancel),
                    );
                    [a, b, c]
                });
                computing.finish(Some(part(10)), SLOW).unwrap();
                (outcomes, waiting.join().unwrap())
            })
        });
        assert!(!rescued, "progress needed the watchdog: {outcomes:?}");
        assert_eq!(outcomes, [Ok(10); 3]);
        assert_eq!(waited, Ok(10));
        assert_eq!(cache.pins()[0].rows, 10, "pinned beside the computation");
        assert!(cache.state().flights.is_empty());
    }

    #[test]
    fn waiters_compute_themselves_when_the_computation_fails() {
        let one = published(1);
        let cache = ResultCache::new(1 << 20);
        let key = key(&one, "part");
        let Claim::Compute(computing) = claim(&cache, &key, None) else {
            panic!("a miss")
        };
        let flight = std::thread::scope(|scope| {
            scope
                .spawn(|| match claim(&cache, &key, None) {
                    Claim::Wait(flight) => flight,
                    _ => panic!("computed elsewhere"),
                })
                .join()
                .unwrap()
        });
        // The same thread asking again (an evaluation inside its own) doesn't wait.
        assert!(matches!(claim(&cache, &key, None), Claim::Bypass));
        drop(computing);
        assert!(flight.wait(|| false).unwrap().is_none());
        assert!(matches!(claim(&cache, &key, None), Claim::Compute(_)));
    }

    #[test]
    fn an_answers_bytes_are_kept_where_writing_costs_more_than_copying() {
        let one = published(1);
        let cache = Arc::new(ResultCache::new(1 << 20));
        // A part this cheap isn't kept: looking it up costs about as much.
        assert!(!compute(
            &cache,
            &key(&one, "part"),
            10,
            Duration::from_micros(5)
        ));
        assert!(!compute(
            &cache,
            &key(&one, "part"),
            10,
            Duration::from_micros(5)
        ));
        // An answer's bytes replace the whole request: kept however cheap.
        let answer = key(&one, "answer");
        let CachedOutput::Miss(slot) = output(&cache, &answer) else {
            panic!("nothing cached")
        };
        slot.offer(vec![b'x'; 100], Duration::from_micros(5));
        assert!(matches!(
            output(&cache, &answer),
            CachedOutput::Hit(bytes) if bytes.len() == 100
        ));
        // Unless copying them costs as much as writing them did.
        let large = key(&one, "large");
        let CachedOutput::Miss(slot) = output(&cache, &large) else {
            panic!("nothing cached")
        };
        slot.offer(vec![b'x'; 100_000], Duration::from_micros(5));
        assert!(matches!(output(&cache, &large), CachedOutput::Miss(_)));
        let stats = cache.stats();
        assert_eq!((stats.answers, stats.entries), (1, 1));
    }

    #[test]
    fn pinned_results_stay_until_unpinned() {
        let one = published(1);
        let cache = ResultCache::new(8_000);
        let pin = PinRequest {
            name: "big".into(),
            query: "SELECT …".into(),
        };
        // Pinned: kept though cheap.
        let pinned = key(&one, "pinned");
        let Claim::Compute(computing) = claim(&cache, &pinned, Some(&pin)) else {
            panic!("a miss")
        };
        computing
            .finish(Some(part(200)), Duration::from_micros(1))
            .unwrap();
        for i in 0..50 {
            compute(&cache, &key(&one, &format!("q{i}")), 100, SLOW);
        }
        assert!(compute(&cache, &key(&one, "pinned"), 200, SLOW));
        let pins = cache.pins();
        assert_eq!(pins.len(), 1);
        assert_eq!((pins[0].revision, pins[0].rows), (Some(1), 200));
        assert!(cache.stats().pinned_bytes > 1_600);
        // Too large to pin beside the budget: an error, and no pin.
        let huge = PinRequest {
            name: "huge".into(),
            query: "SELECT …".into(),
        };
        let too_large = key(&one, "huge");
        let Claim::Compute(computing) = claim(&cache, &too_large, Some(&huge)) else {
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
