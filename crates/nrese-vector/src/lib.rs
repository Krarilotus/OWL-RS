//! Vector similarity search (research designs §2): the vectors of a store as literals of
//! the datatype [`DATATYPE`], the distances between them ([`Metric`]), and the indexes
//! that find the nearest ones: an exact scan ([`exact`]) and HNSW ([`Hnsw`]), both
//! taking a filter of the ids a query accepts.
//!
//! Nothing here knows terms, snapshots or queries: the engine keeps the vectors of its
//! dictionary in [`Vectors`] by term id, and the query engine decides which index to ask
//! and with which filter.

mod hnsw;
mod kernels;
mod literal;

pub use hnsw::{Hnsw, HnswParameters, HnswParts};
pub use kernels::{Metric, dot, squared_l2};
pub use literal::{DATATYPE, NAMESPACE, ParseError, lexical, parse};

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

/// Vectors per chunk: appending copies at most the last chunk when a snapshot of the
/// vectors is held elsewhere (a graph being built), not all of them.
pub const CHUNK: usize = 4096;

/// Vectors of one dimension, each with an id (a term id): appended, never changed.
/// Each vector is kept with its norm, so cosine similarity is one dot product. Cloning
/// is cheap: the chunks are shared, and the vectors appended to a clone are its own.
#[derive(Debug, Clone, Default)]
pub struct Vectors {
    dimension: usize,
    chunks: Vec<std::sync::Arc<Chunk>>,
    len: usize,
}

/// Up to [`CHUNK`] vectors, their norms and ids.
#[derive(Debug, Clone, Default)]
struct Chunk {
    data: Vec<f32>,
    norms: Vec<f32>,
    ids: Vec<u64>,
}

impl Vectors {
    pub fn new(dimension: usize) -> Self {
        Self {
            dimension,
            ..Self::default()
        }
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends `vector` (of this dimension) with `id`; its position.
    pub fn push(&mut self, id: u64, vector: &[f32]) -> u32 {
        debug_assert_eq!(vector.len(), self.dimension);
        if self.len.is_multiple_of(CHUNK) {
            self.chunks.push(std::sync::Arc::new(Chunk {
                data: Vec::with_capacity(CHUNK * self.dimension),
                norms: Vec::with_capacity(CHUNK),
                ids: Vec::with_capacity(CHUNK),
            }));
        }
        let chunk = std::sync::Arc::make_mut(self.chunks.last_mut().expect("a chunk"));
        chunk.data.extend_from_slice(vector);
        chunk.norms.push(dot(vector, vector).sqrt());
        chunk.ids.push(id);
        self.len += 1;
        (self.len - 1) as u32
    }

    fn chunk(&self, position: u32) -> (&Chunk, usize) {
        let position = position as usize;
        (&self.chunks[position / CHUNK], position % CHUNK)
    }

    /// The vector at `position`.
    pub fn get(&self, position: u32) -> &[f32] {
        let (chunk, at) = self.chunk(position);
        &chunk.data[at * self.dimension..(at + 1) * self.dimension]
    }

    /// The id at `position`.
    pub fn id(&self, position: u32) -> u64 {
        let (chunk, at) = self.chunk(position);
        chunk.ids[at]
    }

    /// The norm of the vector at `position`.
    pub fn norm(&self, position: u32) -> f32 {
        let (chunk, at) = self.chunk(position);
        chunk.norms[at]
    }

    /// The vector at `position` as a query (with its kept norm).
    pub fn query_at(&self, position: u32) -> Query<'_> {
        Query {
            vector: self.get(position),
            norm: self.norm(position),
        }
    }

    /// The distance from `query` (with its norm) to the vector at `position` under
    /// `metric`: smaller is nearer.
    pub fn distance(&self, metric: Metric, query: &Query<'_>, position: u32) -> f32 {
        let (chunk, at) = self.chunk(position);
        let vector = &chunk.data[at * self.dimension..(at + 1) * self.dimension];
        match metric {
            Metric::Cosine => {
                let norms = query.norm * chunk.norms[at];
                if norms == 0.0 {
                    1.0
                } else {
                    1.0 - dot(query.vector, vector) / norms
                }
            }
            Metric::Dot => -dot(query.vector, vector),
            Metric::L2 => squared_l2(query.vector, vector),
        }
    }

    /// Bytes of the vectors, norms and ids.
    pub fn memory_bytes(&self) -> usize {
        self.chunks
            .iter()
            .map(|c| c.data.capacity() * 4 + c.norms.capacity() * 4 + c.ids.capacity() * 8)
            .sum()
    }

    /// Every vector's id and values, in order (for writing them out).
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[f32])> + '_ {
        self.chunks.iter().flat_map(move |chunk| {
            chunk
                .ids
                .iter()
                .zip(chunk.data.chunks_exact(self.dimension.max(1)))
                .map(|(&id, values)| (id, values))
        })
    }
}

/// A query vector with its norm.
#[derive(Debug, Clone, Copy)]
pub struct Query<'a> {
    pub vector: &'a [f32],
    pub norm: f32,
}

impl<'a> Query<'a> {
    pub fn new(vector: &'a [f32]) -> Self {
        Self {
            vector,
            norm: dot(vector, vector).sqrt(),
        }
    }
}

/// A vector found: its id and its distance from the query (smaller is nearer; see
/// [`Metric::score`] for the score a query reports).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub id: u64,
    pub distance: f32,
}

/// The `k` vectors of `vectors` nearest to `query` among those `accept` takes (by id),
/// nearest first: every vector compared, on every core.
pub fn exact(
    vectors: &Vectors,
    query: &[f32],
    k: usize,
    metric: Metric,
    accept: &(dyn Fn(u64) -> bool + Sync),
) -> Vec<Hit> {
    exact_from(vectors, 0, query, k, metric, accept)
}

/// The most bytes [`exact`] holds besides the vectors, for `k` of `vectors`: a copy of
/// their chunk list, each chunk's best (a heap, then sorted), their merge and the answer.
pub fn exact_bytes(vectors: &Vectors, k: usize) -> usize {
    let chunks = vectors.chunks.len();
    let kept = k.min(CHUNK) + 1;
    chunks * std::mem::size_of::<std::sync::Arc<Chunk>>()
        + (chunks + 2) * kept * std::mem::size_of::<Hit>() * 3
        + chunks * std::mem::size_of::<Vec<Hit>>() * 2
}

/// [`exact`] over the vectors from position `start` on: the ones a graph doesn't cover
/// yet.
pub fn exact_from(
    vectors: &Vectors,
    start: usize,
    query: &[f32],
    k: usize,
    metric: Metric,
    accept: &(dyn Fn(u64) -> bool + Sync),
) -> Vec<Hit> {
    exact_from_until(vectors, start, query, k, metric, accept, None)
}

/// [`exact_from`], stopping early once `stop` is set: each part checks it before its
/// [`CHUNK`] vectors, and a stopped scan's hits are incomplete (the caller checks the
/// flag after it, as a join checks its row limit's).
pub fn exact_from_until(
    vectors: &Vectors,
    start: usize,
    query: &[f32],
    k: usize,
    metric: Metric,
    accept: &(dyn Fn(u64) -> bool + Sync),
    stop: Option<&AtomicBool>,
) -> Vec<Hit> {
    if k == 0 || start >= vectors.len() || query.len() != vectors.dimension {
        return Vec::new();
    }
    let query = Query::new(query);
    let parts: Vec<Vec<Hit>> = (start / CHUNK..vectors.chunks.len())
        .into_par_iter()
        .map(|c| {
            if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
                return Vec::new();
            }
            let first = (c * CHUNK).max(start);
            let end = ((c + 1) * CHUNK).min(vectors.len());
            let mut best = TopK::new(k, end - first);
            for position in first..end {
                let position = position as u32;
                let id = vectors.id(position);
                if accept(id) {
                    best.offer(id, vectors.distance(metric, &query, position));
                }
            }
            best.into_sorted()
        })
        .collect();
    let candidates = parts.iter().map(Vec::len).sum();
    TopK::new(k, candidates).collect(parts.into_iter().flatten())
}

/// The most parts [`exact_scan`] splits a space into, whatever the host's threads or the
/// space's size, so that what a scan holds depends on its query and its bound alone.
pub const SCAN_PARTS: usize = 64;

/// The hits [`exact_scan`] holds at most at once for `k` over `eligible` vectors: the best of
/// each part that finds one (a heap, then sorted; at most `eligible` parts find one), their
/// merge, the answer.
pub fn scan_held(k: usize, eligible: usize) -> usize {
    let best = k.min(eligible);
    SCAN_PARTS
        .min(eligible)
        .saturating_mul(best)
        .saturating_add(best)
        .saturating_mul(2)
}

/// The most bytes [`exact_scan`] holds besides the vectors, for `k` over `eligible` vectors:
/// its hits ([`scan_held`]) and the parts' list.
pub fn scan_bytes(k: usize, eligible: usize) -> usize {
    scan_held(k, eligible)
        .saturating_mul(std::mem::size_of::<Hit>())
        .saturating_add(SCAN_PARTS * std::mem::size_of::<(Vec<Hit>, usize)>())
}

/// The `k` vectors nearest to `query` among those `eligible` takes that `accept` takes too
/// (by id), nearest first (ties by id), every one compared, and how many `eligible` took. At
/// most `bound` are eligible (the caller's bound, from what it may read): the space's chunks
/// in at most [`SCAN_PARTS`] runs of chunks, each its best of at most `min(k, bound)`, kept
/// from the first vector it takes on, so that what it holds ([`scan_bytes`]) depends on `k`
/// and `bound` alone. Eligibility is tested once a vector, before `accept`; its concrete
/// predicate is kept so callers' membership checks can be inlined into the scan.
/// Each part checks `stop` before each chunk; a stopped scan's hits are incomplete
/// (the caller checks the flag after it).
#[allow(clippy::too_many_arguments)]
pub fn exact_scan(
    vectors: &Vectors,
    query: &[f32],
    k: usize,
    metric: Metric,
    eligible: &(impl Fn(u64) -> bool + Sync + ?Sized),
    accept: &(dyn Fn(u64) -> bool + Sync),
    bound: usize,
    stop: Option<&AtomicBool>,
) -> (Vec<Hit>, usize) {
    if k == 0 || bound == 0 || query.len() != vectors.dimension {
        return (Vec::new(), 0);
    }
    let query = Query::new(query);
    let chunks = vectors.chunks.len();
    let per = chunks.div_ceil(SCAN_PARTS).max(1);
    let parts: Vec<(Vec<Hit>, usize)> = (0..chunks.div_ceil(per))
        .into_par_iter()
        .map(|part| {
            let (mut best, mut seen) = (None, 0);
            for c in part * per..((part + 1) * per).min(chunks) {
                if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
                    break;
                }
                for position in c * CHUNK..((c + 1) * CHUNK).min(vectors.len()) {
                    let position = position as u32;
                    let id = vectors.id(position);
                    if eligible(id) {
                        seen += 1;
                        if accept(id) {
                            best.get_or_insert_with(|| TopK::new(k, bound))
                                .offer(id, vectors.distance(metric, &query, position));
                        }
                    }
                }
            }
            (best.map_or_else(Vec::new, TopK::into_sorted), seen)
        })
        .collect();
    let seen = parts.iter().map(|(_, seen)| seen).sum();
    let candidates = parts.iter().map(|(hits, _)| hits.len()).sum();
    let hits = TopK::new(k, candidates).collect(parts.into_iter().flat_map(|(hits, _)| hits));
    (hits, seen)
}

/// The hits [`exact_from`] holds at most at once for `k` over the `len` vectors from
/// `start` on: each part's best, then the merged best.
pub fn exact_held(k: usize, start: usize, len: usize) -> usize {
    let n = len.saturating_sub(start);
    let parts = len.div_ceil(CHUNK).saturating_sub(start / CHUNK);
    k.saturating_mul(parts).min(n) + k.min(n)
}

/// The `k` nearest of `hits`, nearest first.
pub fn merge(hits: impl IntoIterator<Item = Hit>, k: usize) -> Vec<Hit> {
    let hits = hits.into_iter();
    // Reserved for the hits there are, not for a k of any size.
    TopK::new(k, hits.size_hint().0).collect(hits)
}

/// The `k` best hits offered, kept in a max-heap by distance.
struct TopK {
    k: usize,
    heap: std::collections::BinaryHeap<Ordered<u64>>,
}

impl TopK {
    /// The best `k` of at most `candidates` (at least of as many as are known: what is
    /// reserved, never a k of any size).
    fn new(k: usize, candidates: usize) -> Self {
        Self {
            k,
            heap: std::collections::BinaryHeap::with_capacity(k.min(candidates)),
        }
    }

    fn collect(mut self, hits: impl Iterator<Item = Hit>) -> Vec<Hit> {
        for hit in hits {
            self.offer(hit.id, hit.distance);
        }
        self.into_sorted()
    }

    /// Admitted by the order the answer is sorted by, (distance, id): a hit at the k-th's
    /// distance with a smaller id replaces it, whatever order the hits come in.
    fn offer(&mut self, id: u64, distance: f32) {
        let hit = Ordered(distance, id);
        if self.heap.len() < self.k {
            self.heap.push(hit);
        } else if self.heap.peek().is_some_and(|worst| hit < *worst) {
            self.heap.pop();
            self.heap.push(hit);
        }
    }

    fn into_sorted(self) -> Vec<Hit> {
        let mut hits: Vec<Hit> = self
            .heap
            .into_iter()
            .map(|Ordered(distance, id)| Hit { id, distance })
            .collect();
        hits.sort_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id)));
        hits
    }
}

/// A distance and a payload, ordered by distance (then payload), NaN last.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ordered<T>(f32, T);

impl<T: Ord> Eq for Ordered<T> {}

impl<T: Ord> PartialOrd for Ordered<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Ord> Ord for Ordered<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .total_cmp(&other.0)
            .then_with(|| self.1.cmp(&other.1))
    }
}

#[cfg(test)]
mod tests {
    /// Ties at the k-th distance go to the smaller id, whichever comes first (the review of
    /// 20 October: an equal distance was refused once k were held).
    #[test]
    fn ties_at_the_kth_go_to_the_smaller_id() {
        let mut vectors = Vectors::new(1);
        for id in [9u64, 7, 3, 5] {
            vectors.push(id, &[1.0]);
        }
        let found: Vec<u64> = exact(&vectors, &[1.0], 2, Metric::L2, &|_| true)
            .iter()
            .map(|hit| hit.id)
            .collect();
        assert_eq!(found, [3, 5]);
    }

    use super::*;

    /// A k of any size reserves for the candidates there are and returns all of them,
    /// from exact-size and unknown-size inputs alike.
    #[test]
    fn a_large_k_allocates_for_candidates_and_returns_all_available_hits() {
        let vectors = random_vectors(7, 3, 7);
        let query = vectors.get(0);
        let truth = exact(&vectors, query, vectors.len(), Metric::L2, &|_| true);
        assert_eq!(
            exact(&vectors, query, usize::MAX, Metric::L2, &|_| true),
            truth
        );
        assert_eq!(merge(truth.clone(), usize::MAX), truth);
        assert_eq!(
            merge(truth.iter().copied().filter(|_| true), usize::MAX),
            truth
        );
        assert!(merge(std::iter::empty(), usize::MAX).is_empty());
        // At most each part's best and the merged best, never more than the vectors.
        assert_eq!(exact_held(usize::MAX, 0, 10_000), 20_000);
        assert_eq!(exact_held(10, 0, 10_000), 30 + 10);
        assert_eq!(exact_held(10, 9_000, 10_000), 10 + 10);
        assert_eq!(exact_held(10, 10_000, 10_000), 0);
        // A scan's: of its query and its bound alone, the parts that find one at most.
        assert_eq!(scan_held(10, 1), 2 * (1 + 1));
        assert_eq!(scan_held(10, 10_000), 2 * (SCAN_PARTS * 10 + 10));
        assert_eq!(scan_held(usize::MAX, 10), 2 * (10 * 10 + 10));
    }

    /// A set stop flag ends a scan at its parts' starts: a stopped scan compares nothing.
    #[test]
    fn a_stopped_scan_compares_nothing() {
        let vectors = random_vectors(3 * CHUNK, 2, 11);
        let stop = AtomicBool::new(true);
        let compared = std::sync::atomic::AtomicUsize::new(0);
        let accept = |_: u64| {
            compared.fetch_add(1, Ordering::Relaxed);
            true
        };
        let query = vectors.get(0).to_vec();
        assert!(
            exact_from_until(&vectors, 0, &query, 5, Metric::L2, &accept, Some(&stop)).is_empty()
        );
        assert_eq!(compared.load(Ordering::Relaxed), 0);
        assert_eq!(
            exact_scan(
                &vectors,
                &query,
                5,
                Metric::L2,
                &accept,
                &accept,
                vectors.len(),
                Some(&stop),
            ),
            (Vec::new(), 0)
        );
        assert_eq!(compared.load(Ordering::Relaxed), 0);
        stop.store(false, Ordering::Relaxed);
        let found = exact_from_until(&vectors, 0, &query, 5, Metric::L2, &accept, Some(&stop));
        assert_eq!(found, exact(&vectors, &query, 5, Metric::L2, &|_| true));
    }

    /// Eligibility still counts every readable vector before the caller's filter, across
    /// chunk boundaries, for concrete and dynamically dispatched predicates alike.
    #[test]
    fn scoped_scans_match_sorted_distances_and_count_before_filtering() {
        use std::sync::atomic::AtomicUsize;

        for n in [CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK] {
            let vectors = random_vectors(n, 3, 7);
            let query = Query::new(vectors.get(123));
            let readable = |id: u64| !id.is_multiple_of(5);
            let accepted = |id: u64| id.is_multiple_of(7);
            let bound = vectors.iter().filter(|(id, _)| readable(*id)).count();
            for metric in [Metric::Cosine, Metric::Dot, Metric::L2] {
                let tested = AtomicUsize::new(0);
                let filtered = AtomicUsize::new(0);
                let eligible = |id| {
                    tested.fetch_add(1, Ordering::Relaxed);
                    readable(id)
                };
                let accept = |id| {
                    assert!(readable(id));
                    filtered.fetch_add(1, Ordering::Relaxed);
                    accepted(id)
                };
                let mut truth: Vec<Hit> = (0..vectors.len() as u32)
                    .filter(|&p| readable(vectors.id(p)) && accepted(vectors.id(p)))
                    .map(|p| Hit {
                        id: vectors.id(p),
                        distance: vectors.distance(metric, &query, p),
                    })
                    .collect();
                truth.sort_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id)));
                truth.truncate(10);
                let found = exact_scan(
                    &vectors,
                    query.vector,
                    10,
                    metric,
                    &eligible,
                    &accept,
                    bound,
                    None,
                );
                assert_eq!(found, (truth, bound), "{n}, {metric:?}");
                assert_eq!(tested.load(Ordering::Relaxed), n);
                assert_eq!(filtered.load(Ordering::Relaxed), bound);
                let erased: &(dyn Fn(u64) -> bool + Sync) = &eligible;
                assert_eq!(
                    exact_scan(
                        &vectors,
                        query.vector,
                        10,
                        metric,
                        erased,
                        &accept,
                        bound,
                        None,
                    ),
                    found
                );
                assert_eq!(tested.load(Ordering::Relaxed), 2 * n);
                assert_eq!(filtered.load(Ordering::Relaxed), 2 * bound);
            }
        }
    }

    /// Pseudo-random vectors, the same every run.
    pub(crate) fn random_vectors(n: usize, dimension: usize, seed: u64) -> Vectors {
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        let mut vectors = Vectors::new(dimension);
        let mut vector = vec![0.0; dimension];
        for id in 0..n {
            for value in &mut vector {
                *value = next();
            }
            vectors.push(id as u64 * 3 + 1, &vector);
        }
        vectors
    }

    /// The exact scan agrees with sorting every distance, with and without a filter,
    /// for every metric.
    #[test]
    fn exact_scans_find_the_nearest() {
        let vectors = random_vectors(40_000, 24, 7);
        let query: Vec<f32> = vectors.get(123).iter().map(|v| v + 0.01).collect();
        for metric in [Metric::Cosine, Metric::Dot, Metric::L2] {
            for odd_only in [false, true] {
                let accept = |id: u64| !odd_only || id % 2 == 1;
                let found = exact(&vectors, &query, 10, metric, &accept);
                let q = Query::new(&query);
                let mut all: Vec<Hit> = (0..vectors.len() as u32)
                    .filter(|&p| accept(vectors.id(p)))
                    .map(|p| Hit {
                        id: vectors.id(p),
                        distance: vectors.distance(metric, &q, p),
                    })
                    .collect();
                all.sort_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id)));
                all.truncate(10);
                assert_eq!(found, all, "{metric:?}, odd only {odd_only}");
            }
        }
    }
}
