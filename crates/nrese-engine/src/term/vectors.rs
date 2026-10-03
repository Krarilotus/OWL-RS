//! The vector literals of the dictionary (`"[0.1, 0.2]"^^nrv:vector`; [`nrese_vector`]),
//! by dimension: parsed the first time a query searches, and extended with the literals
//! the dictionary gained since before each later search. The dictionary is append-only,
//! so nothing indexed ever changes; whether a literal still occurs in the data is the
//! query's business (its filter).
//!
//! A space (the vectors of one dimension) is searched exactly while it is small, or when
//! the query's filter accepts few of its vectors; otherwise through an HNSW graph per
//! metric, built at the first such search and extended at later ones.

use std::collections::BTreeMap;

use nrese_vector::{Hit, Hnsw, HnswParameters, Metric, Vectors};

/// Spaces with fewer vectors are always searched exactly: a scan of them is as fast.
pub const EXACT_BELOW: usize = 20_000;

/// A filter that accepts less than this share of a space is searched exactly.
const EXACT_SHARE: f64 = 0.02;

/// How a search finds its vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorStrategy {
    /// Exact for small spaces and selective filters, else the graph.
    #[default]
    Auto,
    /// Every vector compared: the true nearest.
    Exact,
    /// The graph, whatever the space's size.
    Approximate,
}

/// A vector search.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorQuery {
    pub vector: Vec<f32>,
    pub k: usize,
    pub metric: Metric,
    pub strategy: VectorStrategy,
    /// The graph search's beam (at least `k`).
    pub ef: usize,
    /// How many vectors the filter accepts, if the caller knows (it chooses the
    /// strategy).
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
    /// Whether the graph was searched (else every vector).
    pub graph: bool,
}

#[derive(Debug, Default)]
pub(crate) struct VectorIndex {
    /// Dictionary entries up to here are in the spaces.
    covered: u64,
    spaces: BTreeMap<usize, Space>,
}

#[derive(Debug)]
struct Space {
    vectors: Vectors,
    graphs: Vec<Hnsw>,
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
                .or_insert_with(|| Space {
                    vectors: Vectors::new(vector.len()),
                    graphs: Vec::new(),
                })
                .vectors
                .push(id, &vector);
        }
        self.covered = end;
    }

    /// Whether `query` would be answered through a graph that needs building or
    /// extending (the caller then holds the index for writing).
    pub(crate) fn needs_graph(&self, query: &VectorQuery) -> bool {
        let Some(space) = self.spaces.get(&query.vector.len()) else {
            return false;
        };
        uses_graph(space, query)
            && !space
                .graphs
                .iter()
                .any(|graph| graph.metric() == query.metric && graph.len() == space.vectors.len())
    }

    /// Builds or extends the graph `query` needs.
    pub(crate) fn prepare_graph(&mut self, query: &VectorQuery) {
        let Some(space) = self.spaces.get_mut(&query.vector.len()) else {
            return;
        };
        if !uses_graph(space, query) {
            return;
        }
        let at = match space
            .graphs
            .iter()
            .position(|graph| graph.metric() == query.metric)
        {
            Some(at) => at,
            None => {
                space
                    .graphs
                    .push(Hnsw::new(query.metric, HnswParameters::default()));
                space.graphs.len() - 1
            }
        };
        let started = std::time::Instant::now();
        let before = space.graphs[at].len();
        space.graphs[at].extend(&space.vectors);
        tracing::info!(
            dimension = space.vectors.dimension(),
            metric = query.metric.name(),
            linked = space.vectors.len() - before,
            vectors = space.vectors.len(),
            ms = started.elapsed().as_millis() as u64,
            "vector graph extended"
        );
    }

    /// The `query.k` vectors nearest to `query.vector` that `accept` takes (by id).
    pub(crate) fn search(
        &self,
        query: &VectorQuery,
        accept: &(dyn Fn(u64) -> bool + Sync),
    ) -> (Vec<Hit>, VectorSearchReport) {
        let Some(space) = self.spaces.get(&query.vector.len()) else {
            return (
                Vec::new(),
                VectorSearchReport {
                    space: 0,
                    graph: false,
                },
            );
        };
        let graph = space
            .graphs
            .iter()
            .find(|graph| graph.metric() == query.metric)
            .filter(|_| uses_graph(space, query));
        let report = VectorSearchReport {
            space: space.vectors.len(),
            graph: graph.is_some(),
        };
        let hits = match graph {
            Some(graph) => {
                // Over-fetch for the filter: the beam keeps accepted vectors only, so a
                // filter that rejects many needs a wider one.
                let share = query
                    .accepted
                    .map_or(1.0, |n| n as f64 / space.vectors.len().max(1) as f64);
                let ef = ((query.ef.max(query.k) as f64) / share.clamp(EXACT_SHARE, 1.0)).ceil()
                    as usize;
                graph.search(&space.vectors, &query.vector, query.k, ef, accept)
            }
            None => {
                nrese_vector::exact(&space.vectors, &query.vector, query.k, query.metric, accept)
            }
        };
        (hits, report)
    }

    /// Bytes of the vectors and graphs.
    pub(crate) fn memory_bytes(&self) -> usize {
        self.spaces
            .values()
            .map(|space| {
                space.vectors.memory_bytes()
                    + space.graphs.iter().map(Hnsw::memory_bytes).sum::<usize>()
            })
            .sum()
    }
}

/// Whether `query` is answered through a graph of `space`.
fn uses_graph(space: &Space, query: &VectorQuery) -> bool {
    match query.strategy {
        VectorStrategy::Exact => false,
        VectorStrategy::Approximate => true,
        VectorStrategy::Auto => {
            let size = space.vectors.len();
            let selective = query
                .accepted
                .is_some_and(|n| (n as f64) < EXACT_SHARE * size as f64);
            size >= EXACT_BELOW && !selective
        }
    }
}
