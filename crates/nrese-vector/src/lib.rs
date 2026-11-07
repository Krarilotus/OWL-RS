//! Vector similarity search (research designs §2): the vectors of a store as literals of
//! the datatype [`DATATYPE`], the distances between them ([`Metric`]), and the indexes
//! that find the nearest ones: an exact scan ([`exact`]) and HNSW ([`Hnsw`]), both
//! taking a filter of the ids a query accepts.
//!
//! Nothing here knows terms, snapshots or queries: the engine keeps the vectors of its
//! dictionary in [`Vectors`] by term id, and the query engine decides which index to ask
//! and with which filter.

mod charged;
mod hnsw;
mod kernels;
mod literal;

pub use hnsw::{Hnsw, HnswParameters, HnswParts};
pub use kernels::{Metric, dot, squared_l2};
pub use literal::{DATATYPE, NAMESPACE, ParseError, lexical, parse};

use charged::Heap;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
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
/// nearest first: every vector compared, on every core. What the scan holds (each part's
/// best, the merged best) is charged to `budget` before it is allocated; a scan the budget
/// has no room for is refused, with nothing left charged.
pub fn exact<'b>(
    vectors: &Vectors,
    query: &[f32],
    k: usize,
    metric: Metric,
    accept: &(dyn Fn(u64) -> bool + Sync),
    budget: &'b Budget,
) -> Result<ChargedVec<Hit, &'b Budget>, BudgetExceeded> {
    exact_from(vectors, 0, query, k, metric, accept, budget)
}

/// [`exact`] over the vectors from position `start` on: the ones a graph doesn't cover
/// yet.
pub fn exact_from<'b>(
    vectors: &Vectors,
    start: usize,
    query: &[f32],
    k: usize,
    metric: Metric,
    accept: &(dyn Fn(u64) -> bool + Sync),
    budget: &'b Budget,
) -> Result<ChargedVec<Hit, &'b Budget>, BudgetExceeded> {
    if k == 0 || start >= vectors.len() || query.len() != vectors.dimension {
        return Ok(ChargedVec::new(budget));
    }
    let query = Query::new(query);
    let chunks = start / CHUNK..vectors.chunks.len();
    type Part<'b> = Result<ChargedVec<Hit, &'b Budget>, BudgetExceeded>;
    // The parts' list itself, charged before rayon collects into it (one entry a part).
    let mut list = Reservation::new(budget);
    list.charge(charged::bytes::<Part<'b>>(budget, chunks.len())?)?;
    let parts: Vec<Part<'b>> = chunks
        .into_par_iter()
        .map(|c| {
            let mut best = TopK::new(k, budget)?;
            let first = (c * CHUNK).max(start);
            for position in first..((c + 1) * CHUNK).min(vectors.len()) {
                let position = position as u32;
                let id = vectors.id(position);
                if accept(id) {
                    best.offer(id, vectors.distance(metric, &query, position))?;
                }
            }
            best.into_sorted()
        })
        .collect();
    let mut best = TopK::new(k, budget)?;
    let mut refused = None;
    for part in parts {
        match part {
            // Each part's hits are freed as they are offered, then its charge released.
            Ok(hits) if refused.is_none() => {
                let (hits, held) = hits.into_parts();
                for hit in hits {
                    if let Err(error) = best.offer(hit.id, hit.distance) {
                        refused = Some(error);
                        break;
                    }
                }
                drop(held);
            }
            Ok(_) => {}
            Err(error) => refused = refused.or(Some(error)),
        }
    }
    drop(list);
    match refused {
        Some(error) => Err(error),
        None => best.into_sorted(),
    }
}

/// The `k` nearest of `hits`, nearest first, held as [`exact`] holds its merge.
pub fn merge(
    hits: impl IntoIterator<Item = Hit>,
    k: usize,
    budget: &Budget,
) -> Result<ChargedVec<Hit, &Budget>, BudgetExceeded> {
    let mut best = TopK::new(k, budget)?;
    for hit in hits {
        best.offer(hit.id, hit.distance)?;
    }
    best.into_sorted()
}

/// The `k` best hits offered, kept in a max-heap by distance, charged to a budget.
struct TopK<'b> {
    k: usize,
    heap: Heap<'b, Ordered<u64>>,
}

impl<'b> TopK<'b> {
    /// Room for `k` hits up to a chunk's worth, grown past that only by hits offered: a
    /// part never holds more than its chunk, and a large `k` (a caller widening its
    /// search towards the whole space) allocates for the hits there are.
    fn new(k: usize, budget: &'b Budget) -> Result<Self, BudgetExceeded> {
        Ok(Self {
            k,
            heap: Heap::with_capacity(budget, k.min(CHUNK) + 1)?,
        })
    }

    fn offer(&mut self, id: u64, distance: f32) -> Result<(), BudgetExceeded> {
        if self.heap.len() < self.k {
            self.heap.push(Ordered(distance, id))?;
        } else if self.heap.peek().is_some_and(|worst| distance < worst.0) {
            self.heap.pop();
            self.heap.push(Ordered(distance, id))?;
        }
        Ok(())
    }

    /// Nearest first (by distance, then id).
    fn into_sorted(self) -> Result<ChargedVec<Hit, &'b Budget>, BudgetExceeded> {
        self.heap
            .into_sorted(|Ordered(distance, id)| Hit { id, distance })
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
    use super::*;

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
        let unlimited = Budget::unlimited();
        let vectors = random_vectors(40_000, 24, 7);
        let query: Vec<f32> = vectors.get(123).iter().map(|v| v + 0.01).collect();
        for metric in [Metric::Cosine, Metric::Dot, Metric::L2] {
            for odd_only in [false, true] {
                let accept = |id: u64| !odd_only || id % 2 == 1;
                let found = exact(&vectors, &query, 10, metric, &accept, &unlimited).unwrap();
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
                assert_eq!(*found, all, "{metric:?}, odd only {odd_only}");
            }
        }
    }

    /// A `k` past the vectors (a caller widening its search towards the whole space)
    /// returns every accepted vector, nearest first, from the scan and from the merge:
    /// the heaps hold the hits there are, not room for `k`.
    #[test]
    fn a_k_past_the_vectors_returns_them_all() {
        let unlimited = Budget::unlimited();
        let vectors = random_vectors(CHUNK + 7, 3, 7);
        let query = vectors.get(0);
        let truth = exact(
            &vectors,
            query,
            vectors.len(),
            Metric::L2,
            &|_| true,
            &unlimited,
        )
        .unwrap()
        .to_vec();
        assert_eq!(truth.len(), CHUNK + 7);
        for k in [usize::MAX / 2, usize::MAX] {
            assert_eq!(
                exact(&vectors, query, k, Metric::L2, &|_| true, &unlimited)
                    .unwrap()
                    .to_vec(),
                truth,
                "{k}"
            );
            assert_eq!(
                merge(truth.iter().rev().copied(), k, &unlimited)
                    .unwrap()
                    .to_vec(),
                truth,
                "{k}"
            );
        }
    }

    /// A scan and a merge under a budget with no room for their heaps are refused, and
    /// leave nothing charged; dropped, what they answered releases its charge; with room
    /// for twice what one held at its peak (the parts' order varies), they answer as
    /// unlimited.
    #[test]
    fn scans_and_merges_charge_what_they_hold_and_release_it() {
        let vectors = random_vectors(3 * CHUNK + 5, 4, 11);
        let query = vectors.get(17).to_vec();
        for k in [1, 10, CHUNK, 4 * CHUNK] {
            let unlimited = Budget::unlimited();
            let truth = exact(&vectors, &query, k, Metric::L2, &|_| true, &unlimited)
                .unwrap()
                .to_vec();
            assert_eq!(unlimited.used(), 0, "k {k}: released when dropped");
            assert!(
                unlimited.peak() >= truth.len() * size_of::<Hit>(),
                "k {k}: charged"
            );
            let tight = Budget::new(1);
            assert!(exact(&vectors, &query, k, Metric::L2, &|_| true, &tight).is_err());
            assert_eq!(tight.used(), 0, "k {k}: a refused scan holds nothing");
            assert!(merge(truth.iter().copied(), k, &tight).is_err());
            assert_eq!(tight.used(), 0, "k {k}: a refused merge holds nothing");
            let room = Budget::new(unlimited.peak() * 2);
            let found = exact(&vectors, &query, k, Metric::L2, &|_| true, &room).unwrap();
            assert_eq!(*found, truth, "k {k}");
            drop(found);
            assert_eq!(room.used(), 0);
        }
    }
}
