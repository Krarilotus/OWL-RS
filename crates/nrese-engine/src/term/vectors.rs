//! The vector literals of the dictionary (`"[0.1, 0.2]"^^nrv:vector`; [`nrese_vector`]),
//! by dimension: parsed the first time a query searches, and extended with the literals
//! the dictionary gained since before each later search. The dictionary is append-only,
//! so nothing indexed ever changes; which literals a reader sees is the caller's (the
//! reader's corpus of vectors, `Snapshot::vector_search_in`).
//!
//! A search compares the query with every vector its filter accepts (exact, recall 1):
//! the answer depends on the accepted vectors alone, never on another reader's (an open
//! transaction's, a later version's, a deleted one, one in a graph the reader may not
//! read), so replicas and readers of equal data agree. The vectors are written with the
//! other derived indexes after a checkpoint ([`super::derived`]) and read at the first
//! search after a start.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

use super::TermId;
use nrese_vector::{Metric, Vectors};
use parking_lot::RwLock;

/// How a search finds its vectors. Every search is exact over the vectors the reader
/// sees (the module's docs); the choice is kept for the queries that state it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorStrategy {
    #[default]
    Auto,
    /// Every vector compared: the true nearest.
    Exact,
    Approximate,
}

/// A vector search.
#[derive(Debug, Clone)]
pub struct VectorQuery {
    pub vector: Vec<f32>,
    pub k: usize,
    pub metric: Metric,
    pub strategy: VectorStrategy,
    /// A beam for an approximate search, taken as given; no search uses it while every
    /// search is exact (the module's docs).
    pub ef: usize,
    /// How many vectors the filter accepts, if the caller knows.
    pub accepted: Option<usize>,
    /// The most hits the search may hold at once (a query's memory budget, as a join's
    /// row limit): a search that would hold more is refused before it allocates
    /// ([`VectorStop::TooLarge`]). `None`: no limit.
    pub max_hits: Option<usize>,
    /// The caller's cancellation flag: a search that sees it set stops
    /// ([`VectorStop::Cancelled`]).
    pub stop: Option<Arc<AtomicBool>>,
}

/// Why a search gave no hits ([`VectorQuery::max_hits`], [`VectorQuery::stop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorStop {
    /// It would hold `hits` at once, more than [`VectorQuery::max_hits`].
    TooLarge { hits: usize },
    /// Its stop flag was set.
    Cancelled,
    /// What it would hold doesn't fit the request's budget.
    Memory(BudgetExceeded),
}

impl VectorQuery {
    fn stopped(&self) -> Result<(), VectorStop> {
        match &self.stop {
            Some(stop) if stop.load(Ordering::Relaxed) => Err(VectorStop::Cancelled),
            _ => Ok(()),
        }
    }

    /// Refused if holding `hits` at once is more than the query may.
    fn fits(&self, hits: usize) -> Result<(), VectorStop> {
        match self.max_hits {
            Some(max) if hits > max => Err(VectorStop::TooLarge { hits }),
            _ => Ok(()),
        }
    }
}

impl VectorQuery {
    pub fn new(vector: Vec<f32>, k: usize) -> Self {
        Self {
            vector,
            k,
            metric: Metric::default(),
            strategy: VectorStrategy::default(),
            ef: 64,
            accepted: None,
            max_hits: None,
            stop: None,
        }
    }
}

/// How a search went, for EXPLAIN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorSearchReport {
    /// The vectors of the query's dimension.
    pub space: usize,
    /// Whether a graph was searched (none is: every search is exact).
    pub graph: bool,
    /// The vectors scanned exactly.
    pub scanned: usize,
}

#[derive(Debug, Default)]
pub(crate) struct VectorIndex {
    /// Dictionary entries up to here are in the spaces.
    covered: u64,
    /// The vectors of each dimension, shared with the searches reading them (a search takes
    /// an `Arc`, so it copies nothing the size of the space; a later extension copies the
    /// space's chunk list if a search still holds it).
    spaces: BTreeMap<usize, Arc<Vectors>>,
}

impl VectorIndex {
    pub(crate) fn covered(&self) -> u64 {
        self.covered
    }

    /// Takes in the vector literals among `entries` (id, lexical form), up to entry `end`.
    pub(crate) fn extend(&mut self, entries: &[(u64, &str)], end: u64) {
        for &(id, lexical) in entries {
            let Ok(vector) = nrese_vector::parse(lexical) else {
                continue;
            };
            let space = self
                .spaces
                .entry(vector.len())
                .or_insert_with(|| Arc::new(Vectors::new(vector.len())));
            Arc::make_mut(space).push(id, &vector);
        }
        self.covered = end;
    }

    /// Whether no space has a vector.
    pub(crate) fn is_empty(&self) -> bool {
        self.spaces.values().all(|vectors| vectors.is_empty())
    }

    /// Bytes of the vectors.
    pub(crate) fn memory_bytes(&self) -> usize {
        self.spaces
            .values()
            .map(|vectors| vectors.memory_bytes())
            .sum()
    }

    /// The index as bytes ([`super::derived`]).
    pub(crate) fn write<W: std::io::Write>(
        &self,
        out: &mut super::derived::Writer<W>,
    ) -> std::io::Result<()> {
        out.u32(VECTOR_FORMAT)?;
        out.u64(self.covered)?;
        out.u64(self.spaces.len() as u64)?;
        for (&dimension, vectors) in &self.spaces {
            out.u64(dimension as u64)?;
            out.u64(vectors.len() as u64)?;
            for (id, values) in vectors.iter() {
                out.u64(id)?;
                out.f32s(values)?;
            }
        }
        Ok(())
    }

    /// The index [`Self::write`] wrote; `None` if the bytes aren't one (a file of the
    /// layout with graphs is not: the vectors are read again from the dictionary).
    pub(crate) fn read(input: &mut super::derived::Reader<'_>) -> Option<Self> {
        if input.u32()? != VECTOR_FORMAT {
            return None;
        }
        let covered = input.u64()?;
        let mut spaces = BTreeMap::new();
        for _ in 0..input.len(16)? {
            let dimension = usize::try_from(input.u64()?).ok()?;
            let n = input.len(8 + 4 * dimension)?;
            let mut vectors = Vectors::new(dimension);
            for _ in 0..n {
                let id = input.u64()?;
                let values = input.f32s(dimension)?;
                vectors.push(id, &values);
            }
            spaces.insert(dimension, Arc::new(vectors));
        }
        input.is_done().then_some(Self { covered, spaces })
    }
}

/// The version of [`VectorIndex::write`]'s layout (1 held graphs).
const VECTOR_FORMAT: u32 = 2;

/// The vector literals a search found, nearest first, held on the budget their room was
/// charged to until they drop ([`search`]).
#[derive(Debug)]
pub struct VectorHits {
    /// Before the charge: fields drop in order, the hits first.
    hits: Vec<(TermId, f32)>,
    _charge: Reservation,
}

impl VectorHits {
    /// No hits (nothing allocated).
    fn empty(budget: &Arc<Budget>) -> Self {
        Self {
            hits: Vec::new(),
            _charge: Reservation::new(Arc::clone(budget)),
        }
    }
}

impl Default for VectorHits {
    /// No hits, held on no budget.
    fn default() -> Self {
        Self::empty(&Arc::new(Budget::unlimited()))
    }
}

impl std::ops::Deref for VectorHits {
    type Target = [(TermId, f32)];

    fn deref(&self) -> &[(TermId, f32)] {
        &self.hits
    }
}

impl PartialEq for VectorHits {
    fn eq(&self, other: &Self) -> bool {
        self.hits == other.hits
    }
}

impl<'a> IntoIterator for &'a VectorHits {
    type Item = &'a (TermId, f32);
    type IntoIter = std::slice::Iter<'a, (TermId, f32)>;

    fn into_iter(self) -> Self::IntoIter {
        self.hits.iter()
    }
}

/// The `query.k` vectors nearest to `query.vector` among those `eligible` takes that `accept`
/// takes too (by id), nearest first (ties by id), compared with every one of them. Only what
/// the reader may read decides anything a caller sees: its admission (refused past
/// [`VectorQuery::max_hits`] or the budget) and what it holds from `bound`, its number of
/// documents (at least its eligible vectors; the corpus's own count), its report from the
/// eligible vectors the scan counts as it tests them, its hits from them; the others cost
/// the scan's time alone. The
/// space's vectors are shared (an `Arc`), taken under `index`'s read lock and scanned after
/// it: the scans are parallel, and a pool thread waiting for the lock (a writer queued, a
/// search's `accept` re-entering) must never hold up a part of the search. What the scan
/// holds besides the vectors is charged to `budget` before it is allocated; the hits keep
/// their charge until they drop. Stopped at the next chunk once [`VectorQuery::stop`] is set.
pub(crate) fn search(
    index: &RwLock<VectorIndex>,
    query: &VectorQuery,
    eligible: &(dyn Fn(u64) -> bool + Sync),
    bound: usize,
    accept: &(dyn Fn(u64) -> bool + Sync),
    budget: &Arc<Budget>,
) -> Result<(VectorHits, VectorSearchReport), VectorStop> {
    query.stopped()?;
    let vectors = index.read().spaces.get(&query.vector.len()).cloned();
    let none = VectorSearchReport {
        space: 0,
        graph: false,
        scanned: 0,
    };
    let Some(vectors) = vectors else {
        return Ok((VectorHits::empty(budget), none));
    };
    query.fits(nrese_vector::scan_held(query.k, bound))?;
    let mut scan = Reservation::new(Arc::clone(budget));
    scan.charge(nrese_vector::scan_bytes(query.k, bound))
        .map_err(VectorStop::Memory)?;
    let stop = query.stop.as_deref();
    let (hits, n) = nrese_vector::exact_scan(
        &vectors,
        &query.vector,
        query.k,
        query.metric,
        eligible,
        accept,
        bound,
        stop,
    );
    query.stopped()?;
    // The answer's own room, charged before it is made; the scan's goes after its hits.
    let mut found =
        ChargedVec::with_capacity(Arc::clone(budget), hits.len()).map_err(VectorStop::Memory)?;
    found
        .extend_bounded(
            hits.len(),
            hits.iter()
                .map(|hit| (TermId::from_raw(hit.id), hit.distance)),
        )
        .map_err(VectorStop::Memory)?;
    drop(hits);
    drop(scan);
    let (hits, charge) = found.into_parts();
    let report = VectorSearchReport {
        space: n,
        graph: false,
        scanned: n,
    };
    Ok((
        VectorHits {
            hits,
            _charge: charge,
        },
        report,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search's scans are parallel and run its filter on the pool's threads: there the
    /// index's lock must be free, or a part that wanted it (a job the thread took on
    /// meanwhile, behind a queued writer) would never return. The filter checks it on every
    /// call; the search ran under the read lock before, which it found held.
    #[test]
    fn a_search_scans_without_the_index_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let lexical: Vec<String> = (0..40_000u64)
            .map(|i| format!("[{}, {}, {}, 1]", i % 97, i % 89, i % 83))
            .collect();
        let entries: Vec<(u64, &str)> = lexical
            .iter()
            .enumerate()
            .map(|(i, text)| (i as u64, text.as_str()))
            .collect();
        let mut index = VectorIndex::default();
        index.extend(&entries, entries.len() as u64);
        let index = Arc::new(RwLock::new(index));
        // One check at a time: a failed try then means the search holds the lock.
        let (checking, held) = (parking_lot::Mutex::new(()), AtomicBool::new(false));
        let accept = |_: u64| {
            let _one = checking.lock();
            if index.try_write().is_none() {
                held.store(true, Ordering::Relaxed);
            }
            true
        };
        let query = VectorQuery::new(vec![1.0, 2.0, 3.0, 1.0], 10);
        let budget = Arc::new(Budget::unlimited());
        let (hits, _) = search(&index, &query, &|_| true, lexical.len(), &accept, &budget).unwrap();
        assert_eq!(hits.len(), 10);
        assert!(
            !held.load(Ordering::Relaxed),
            "the index's lock was held during the scan"
        );
    }

    /// A search that would hold more hits than its query may is refused before it
    /// allocates; within the limit it answers; a set stop flag stops it.
    #[test]
    fn searches_past_their_hit_limit_or_stopped_answer_nothing() {
        let n = 10_000;
        let mut vectors = Vectors::new(1);
        for id in 0..n {
            vectors.push(id as u64, &[id as f32]);
        }
        let mut index = VectorIndex::default();
        index.spaces.insert(1, Arc::new(vectors));
        let index = RwLock::new(index);
        let budget = Arc::new(Budget::unlimited());
        let search =
            |query: &VectorQuery| super::search(&index, query, &|_| true, n, &|_| true, &budget);
        let exact = |k, max_hits, stop: Option<bool>| VectorQuery {
            max_hits,
            stop: stop.map(|s| Arc::new(AtomicBool::new(s))),
            ..VectorQuery::new(vec![0.5], k)
        };
        let held = nrese_vector::scan_held(usize::MAX, n);
        assert_eq!(
            search(&exact(usize::MAX, Some(held - 1), None)).unwrap_err(),
            VectorStop::TooLarge { hits: held }
        );
        assert_eq!(
            search(&exact(usize::MAX, Some(held), None))
                .unwrap()
                .0
                .len(),
            n
        );
        let held = nrese_vector::scan_held(10, n);
        assert_eq!(search(&exact(10, Some(held), None)).unwrap().0.len(), 10);
        assert_eq!(
            search(&exact(10, None, Some(true))).unwrap_err(),
            VectorStop::Cancelled
        );
        assert_eq!(
            search(&exact(10, None, Some(false))).unwrap(),
            search(&exact(10, None, None)).unwrap()
        );
    }
}
