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
    if k == 0 || start >= vectors.len() || query.len() != vectors.dimension {
        return Vec::new();
    }
    let query = Query::new(query);
    let parts: Vec<Vec<Hit>> = (start / CHUNK..vectors.chunks.len())
        .into_par_iter()
        .map(|c| {
            let mut best = TopK::new(k);
            let first = (c * CHUNK).max(start);
            for position in first..((c + 1) * CHUNK).min(vectors.len()) {
                let position = position as u32;
                let id = vectors.id(position);
                if accept(id) {
                    best.offer(id, vectors.distance(metric, &query, position));
                }
            }
            best.into_sorted()
        })
        .collect();
    merge(parts.into_iter().flatten(), k)
}

/// The `k` nearest of `hits`, nearest first.
pub fn merge(hits: impl IntoIterator<Item = Hit>, k: usize) -> Vec<Hit> {
    let mut best = TopK::new(k);
    for hit in hits {
        best.offer(hit.id, hit.distance);
    }
    best.into_sorted()
}

/// The `k` best hits offered, kept in a max-heap by distance.
struct TopK {
    k: usize,
    heap: std::collections::BinaryHeap<Ordered<u64>>,
}

impl TopK {
    fn new(k: usize) -> Self {
        Self {
            k,
            heap: std::collections::BinaryHeap::with_capacity(k + 1),
        }
    }

    fn offer(&mut self, id: u64, distance: f32) {
        if self.heap.len() < self.k {
            self.heap.push(Ordered(distance, id));
        } else if self.heap.peek().is_some_and(|worst| distance < worst.0) {
            self.heap.pop();
            self.heap.push(Ordered(distance, id));
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
