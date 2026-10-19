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

use nrese_exec::{Budget, BudgetExceeded, Reservation};
use nrese_vector::{Hit, Metric, Vectors};
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
#[derive(Debug, Clone, PartialEq)]
pub struct VectorQuery {
    pub vector: Vec<f32>,
    pub k: usize,
    pub metric: Metric,
    pub strategy: VectorStrategy,
    /// An approximate search's beam (at least `k`); an exact one has none.
    pub ef: usize,
    /// How many vectors the filter accepts, if the caller knows.
    pub accepted: Option<usize>,
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
    /// The vectors of each dimension.
    spaces: BTreeMap<usize, Vectors>,
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
            self.spaces
                .entry(vector.len())
                .or_insert_with(|| Vectors::new(vector.len()))
                .push(id, &vector);
        }
        self.covered = end;
    }

    /// Whether no space has a vector.
    pub(crate) fn is_empty(&self) -> bool {
        self.spaces.values().all(Vectors::is_empty)
    }

    /// Bytes of the vectors.
    pub(crate) fn memory_bytes(&self) -> usize {
        self.spaces.values().map(Vectors::memory_bytes).sum()
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
            spaces.insert(dimension, vectors);
        }
        input.is_done().then_some(Self { covered, spaces })
    }
}

/// The version of [`VectorIndex::write`]'s layout (1 held graphs).
const VECTOR_FORMAT: u32 = 2;

/// The `query.k` vectors nearest to `query.vector` that `accept` takes (by id), nearest
/// first (ties by id), compared with every one of them. The space's vectors are a cheap
/// copy (shared chunks) taken under `index`'s read lock and scanned after it: the scans
/// are parallel, and a pool thread waiting for the lock (a writer queued, a search's
/// `accept` re-entering) must never hold up a part of the search. What the scan holds
/// besides the vectors is charged to `budget` first, for the search's duration.
pub(crate) fn search(
    index: &RwLock<VectorIndex>,
    query: &VectorQuery,
    accept: &(dyn Fn(u64) -> bool + Sync),
    budget: &Arc<Budget>,
) -> Result<(Vec<Hit>, VectorSearchReport), BudgetExceeded> {
    let mut scan = Reservation::new(Arc::clone(budget));
    let vectors = {
        let index = index.read();
        match index.spaces.get(&query.vector.len()) {
            Some(vectors) => {
                scan.charge(nrese_vector::exact_bytes(vectors, query.k))?;
                Some(vectors.clone())
            }
            None => None,
        }
    };
    let Some(vectors) = vectors else {
        let report = VectorSearchReport {
            space: 0,
            graph: false,
            scanned: 0,
        };
        return Ok((Vec::new(), report));
    };
    let hits = nrese_vector::exact(&vectors, &query.vector, query.k, query.metric, accept);
    let report = VectorSearchReport {
        space: vectors.len(),
        graph: false,
        scanned: vectors.len(),
    };
    Ok((hits, report))
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
        let (hits, _) = search(&index, &query, &accept, &budget).unwrap();
        assert_eq!(hits.len(), 10);
        assert!(
            !held.load(Ordering::Relaxed),
            "the index's lock was held during the scan"
        );
    }
}
