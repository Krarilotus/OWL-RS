//! The vector literals of the dictionary (`"[0.1, 0.2]"^^nrv:vector`; [`nrese_vector`]),
//! by dimension: parsed the first time a query searches, and extended with the literals
//! the dictionary gained since before each later search. The dictionary is append-only,
//! so nothing indexed ever changes; whether a literal still occurs in the data is the
//! query's business (its filter).
//!
//! A space (the vectors of one dimension) is searched exactly while it is small, or when
//! the query's filter accepts few of its vectors; otherwise through an HNSW graph per
//! metric. A graph covers the space's first vectors: a search asks the graph for those
//! and scans the newer ones exactly (a base and a delta, as FreshDiskANN keeps them).
//!
//! - **Building:** a space under [`SYNC_BUILD`] vectors gets its graph at once (under a
//!   second); a larger one on a thread of its own (or the engine's [`VectorBuilder`]),
//!   the search scanning exactly until it is there. Once the vectors no graph covers pass a tenth of those it does, a copy of
//!   the graph is extended with them, again on a thread of its own, and replaces it.
//! - **Keeping:** the vectors and graphs are written with the other derived indexes after
//!   a checkpoint ([`super::derived`]) and read at the first search after a start.

use std::collections::BTreeMap;
use std::sync::Arc;

use nrese_vector::{Hit, Hnsw, HnswParameters, HnswParts, Metric, Vectors};
use parking_lot::RwLock;

/// Spaces with fewer vectors are always searched exactly: a scan of them is as fast.
pub const EXACT_BELOW: usize = 20_000;

/// Spaces with fewer vectors get their graph built at once, not on a thread.
pub const SYNC_BUILD: usize = 50_000;

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
    /// Whether the graph was searched (for the vectors it covers; the rest exactly).
    pub graph: bool,
    /// The vectors scanned exactly.
    pub scanned: usize,
}

#[derive(Debug, Default)]
pub(crate) struct VectorIndex {
    /// Dictionary entries up to here are in the spaces.
    covered: u64,
    spaces: BTreeMap<usize, Space>,
    /// The last reservation a background build was given ([`Work::Background`]).
    reservations: u64,
}

#[derive(Debug, Default)]
struct Space {
    vectors: Vectors,
    graphs: Vec<Slot>,
}

/// A space's graph for one metric.
#[derive(Debug)]
struct Slot {
    metric: Metric,
    /// Covers the space's first `graph.len()` vectors.
    graph: Option<Arc<Hnsw>>,
    /// The background build building or extending it, by its reservation: only that
    /// build's end (installed, or dropped unrun) ends it, so a warm-up's install or another
    /// build's end can't release a build still holding it (PR #79 review, R79-01).
    reserved: Option<u64>,
}

/// What a search needs done to the graph first.
pub(crate) enum Work {
    None,
    /// Build or extend it now.
    Now,
    /// Build or extend it on a thread of its own, under this reservation.
    Background(u64),
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

    /// What the graph `query` uses needs before the search: nothing, building or
    /// extending now (a small space), or on a thread (a large one; reserved for that build).
    pub(crate) fn work_for(&mut self, query: &VectorQuery) -> Work {
        let reservation = self.reservations + 1;
        let Some(space) = self.spaces.get_mut(&query.vector.len()) else {
            return Work::None;
        };
        if !uses_graph(space, query) {
            return Work::None;
        }
        let len = space.vectors.len();
        let slot = slot(space, query.metric);
        let covered = slot.graph.as_ref().map_or(0, |g| g.len());
        if covered == len || slot.reserved.is_some() {
            return Work::None;
        }
        let behind = len - covered;
        if len < SYNC_BUILD {
            return Work::Now;
        }
        // A graph that covers most of the space is extended only past a tenth more.
        if covered > 0 && behind * 10 < covered {
            return Work::None;
        }
        slot.reserved = Some(reservation);
        self.reservations = reservation;
        Work::Background(reservation)
    }

    /// The vectors and graph to build on (a cheap copy of the vectors, shared chunks).
    pub(crate) fn build_inputs(&self, query: &VectorQuery) -> Option<(Vectors, Option<Arc<Hnsw>>)> {
        let space = self.spaces.get(&query.vector.len())?;
        let graph = space
            .graphs
            .iter()
            .find(|slot| slot.metric == query.metric)
            .and_then(|slot| slot.graph.clone());
        Some((space.vectors.clone(), graph))
    }

    /// Installs `graph` for `metric` in the space of `dimension`, unless it already has
    /// one covering more; a background build's `reservation` ends with it (a warm-up's
    /// build, `None`, holds none and ends none).
    pub(crate) fn install(
        &mut self,
        dimension: usize,
        metric: Metric,
        graph: Hnsw,
        reservation: Option<u64>,
    ) {
        let Some(space) = self.spaces.get_mut(&dimension) else {
            return;
        };
        let slot = slot(space, metric);
        if reservation.is_some() && slot.reserved == reservation {
            slot.reserved = None;
        }
        if slot.graph.as_ref().is_none_or(|g| g.len() < graph.len()) {
            slot.graph = Some(Arc::new(graph));
        }
    }

    /// Whether [`Self::work_for`] may find work: the search uses a graph that doesn't cover
    /// the space and none is being built.
    pub(crate) fn may_need_work(&self, query: &VectorQuery) -> bool {
        let Some(space) = self.spaces.get(&query.vector.len()) else {
            return false;
        };
        uses_graph(space, query)
            && !space.graphs.iter().any(|slot| {
                slot.metric == query.metric
                    && (slot.reserved.is_some()
                        || slot
                            .graph
                            .as_ref()
                            .is_some_and(|g| g.len() == space.vectors.len()))
            })
    }

    /// The build holding `reservation` for `metric` in the space of `dimension` ended
    /// without a graph: its reservation ends, and no other's.
    fn release(&mut self, dimension: usize, metric: Metric, reservation: u64) {
        if let Some(space) = self.spaces.get_mut(&dimension) {
            let slot = slot(space, metric);
            if slot.reserved == Some(reservation) {
                slot.reserved = None;
            }
        }
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
                    scanned: 0,
                },
            );
        };
        let len = space.vectors.len();
        let graph = space
            .graphs
            .iter()
            .find(|slot| slot.metric == query.metric)
            .and_then(|slot| slot.graph.as_ref())
            .filter(|_| uses_graph(space, query));
        let Some(graph) = graph else {
            let hits =
                nrese_vector::exact(&space.vectors, &query.vector, query.k, query.metric, accept);
            return (
                hits,
                VectorSearchReport {
                    space: len,
                    graph: false,
                    scanned: len,
                },
            );
        };
        // Over-fetch for the filter: the beam keeps accepted vectors only, so a filter
        // that rejects many needs a wider one.
        let share = query.accepted.map_or(1.0, |n| n as f64 / len.max(1) as f64);
        let ef = ((query.ef.max(query.k) as f64) / share.clamp(EXACT_SHARE, 1.0)).ceil() as usize;
        let from_graph = graph.search(&space.vectors, &query.vector, query.k, ef, accept);
        let tail = nrese_vector::exact_from(
            &space.vectors,
            graph.len(),
            &query.vector,
            query.k,
            query.metric,
            accept,
        );
        let hits = nrese_vector::merge(from_graph.into_iter().chain(tail), query.k);
        (
            hits,
            VectorSearchReport {
                space: len,
                graph: true,
                scanned: len - graph.len(),
            },
        )
    }

    /// Whether no space has a vector.
    pub(crate) fn is_empty(&self) -> bool {
        self.spaces.values().all(|space| space.vectors.is_empty())
    }

    /// The vectors the graphs cover, summed: grows when a graph is built or extended.
    pub(crate) fn graph_coverage(&self) -> usize {
        self.spaces
            .values()
            .flat_map(|space| space.graphs.iter())
            .filter_map(|slot| slot.graph.as_ref())
            .map(|graph| graph.len())
            .sum()
    }

    /// Bytes of the vectors and graphs.
    pub(crate) fn memory_bytes(&self) -> usize {
        self.spaces
            .values()
            .map(|space| {
                space.vectors.memory_bytes()
                    + space
                        .graphs
                        .iter()
                        .filter_map(|slot| slot.graph.as_ref())
                        .map(|graph| graph.memory_bytes())
                        .sum::<usize>()
            })
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
        for (&dimension, space) in &self.spaces {
            out.u64(dimension as u64)?;
            out.u64(space.vectors.len() as u64)?;
            for (id, values) in space.vectors.iter() {
                out.u64(id)?;
                out.f32s(values)?;
            }
            let graphs: Vec<&Arc<Hnsw>> = space
                .graphs
                .iter()
                .filter_map(|slot| slot.graph.as_ref())
                .collect();
            out.u64(graphs.len() as u64)?;
            for graph in graphs {
                let parts = graph.to_parts();
                out.u32(metric_code(parts.metric))?;
                out.u64(parts.parameters.m as u64)?;
                out.u64(parts.parameters.ef_construction as u64)?;
                out.u64(parts.parameters.seed)?;
                match parts.entry {
                    Some((node, level)) => {
                        out.u32(1)?;
                        out.u32(node)?;
                        out.u32(u32::from(level))?;
                    }
                    None => out.u32(0)?,
                }
                out.u64(parts.links.len() as u64)?;
                for levels in &parts.links {
                    out.u32(levels.len() as u32)?;
                    for links in levels {
                        out.u32(links.len() as u32)?;
                        for &link in links {
                            out.u32(link)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The index [`Self::write`] wrote; `None` if the bytes aren't one.
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
            let mut graphs = Vec::new();
            for _ in 0..input.len(4)? {
                let metric = metric_of(input.u32()?)?;
                let parameters = HnswParameters {
                    m: usize::try_from(input.u64()?).ok()?,
                    ef_construction: usize::try_from(input.u64()?).ok()?,
                    seed: input.u64()?,
                };
                let entry = match input.u32()? {
                    0 => None,
                    _ => Some((input.u32()?, u8::try_from(input.u32()?).ok()?)),
                };
                let nodes = input.len(4)?;
                let mut links = Vec::with_capacity(nodes);
                for _ in 0..nodes {
                    let levels = input.u32()? as usize;
                    let mut node = Vec::with_capacity(levels.min(32));
                    for _ in 0..levels {
                        let count = input.u32()? as usize;
                        let mut level = Vec::with_capacity(count.min(1024));
                        for _ in 0..count {
                            level.push(input.u32()?);
                        }
                        node.push(level);
                    }
                    links.push(node);
                }
                if nodes > n {
                    return None;
                }
                let graph = Hnsw::from_parts(HnswParts {
                    parameters,
                    metric,
                    entry,
                    links,
                })?;
                graphs.push(Slot {
                    metric,
                    graph: Some(Arc::new(graph)),
                    reserved: None,
                });
            }
            spaces.insert(dimension, Space { vectors, graphs });
        }
        input.is_done().then_some(Self {
            covered,
            spaces,
            reservations: 0,
        })
    }
}

/// The version of [`VectorIndex::write`]'s layout.
const VECTOR_FORMAT: u32 = 1;

fn metric_code(metric: Metric) -> u32 {
    match metric {
        Metric::Cosine => 0,
        Metric::Dot => 1,
        Metric::L2 => 2,
    }
}

fn metric_of(code: u32) -> Option<Metric> {
    match code {
        0 => Some(Metric::Cosine),
        1 => Some(Metric::Dot),
        2 => Some(Metric::L2),
        _ => None,
    }
}

/// The slot of `metric` in `space`, added if missing.
fn slot(space: &mut Space, metric: Metric) -> &mut Slot {
    let at = match space.graphs.iter().position(|slot| slot.metric == metric) {
        Some(at) => at,
        None => {
            space.graphs.push(Slot {
                metric,
                graph: None,
                reserved: None,
            });
            space.graphs.len() - 1
        }
    };
    &mut space.graphs[at]
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

/// Builds or extends the graph `query` uses, from what `index` holds now, and installs
/// it, ending `reservation` if it still holds the space (a background build's; a warm-up
/// or a small space's build has none).
pub(crate) fn build(index: &RwLock<VectorIndex>, query: &VectorQuery, reservation: Option<u64>) {
    let Some((vectors, base)) = index.read().build_inputs(query) else {
        return;
    };
    let started = std::time::Instant::now();
    let mut graph = match base {
        Some(base) => base.duplicate(),
        None => Hnsw::new(query.metric, HnswParameters::default()),
    };
    let before = graph.len();
    graph.extend(&vectors);
    tracing::info!(
        dimension = vectors.dimension(),
        metric = query.metric.name(),
        linked = graph.len() - before,
        vectors = graph.len(),
        ms = started.elapsed().as_millis() as u64,
        "vector graph built"
    );
    index
        .write()
        .install(vectors.dimension(), query.metric, graph, reservation);
}

/// Runs the graph builds searches start in the background: each [`VectorBuild`] it is
/// handed, now or later, or refuses it with an error. Without one, each build gets a
/// thread of its own ([`crate::Engine::run_vector_builds_with`]).
pub type VectorBuilder = Arc<dyn Fn(VectorBuild) -> std::io::Result<()> + Send + Sync>;

/// A graph build a search started, holding its space's reservation meanwhile:
/// [`Self::run`] builds the graph and installs it. However it ends (run, refused, its
/// build panicked), its own reservation ends with it, and no other's: a later search
/// may start another.
pub struct VectorBuild {
    index: Arc<RwLock<VectorIndex>>,
    query: VectorQuery,
    reservation: u64,
}

impl VectorBuild {
    /// Builds the graph from what the index holds now and installs it, on this thread.
    pub fn run(self) {
        build(&self.index, &self.query, Some(self.reservation));
    }
}

impl Drop for VectorBuild {
    fn drop(&mut self) {
        // Nothing if its install ended the reservation already.
        let mut index = self.index.write();
        index.release(self.query.vector.len(), self.query.metric, self.reservation);
    }
}

/// [`build`] under `reservation` ([`Work::Background`]), through `builder` or on a thread
/// of its own.
pub(crate) fn build_in_background(
    index: Arc<RwLock<VectorIndex>>,
    query: VectorQuery,
    reservation: u64,
    builder: Option<&VectorBuilder>,
) {
    let job = VectorBuild {
        index,
        query,
        reservation,
    };
    let started = match builder {
        Some(builder) => builder(job),
        None => std::thread::Builder::new()
            .name("nrese-vector-graph".to_owned())
            .spawn(move || {
                let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()));
                if built.is_err() {
                    tracing::error!("building a vector graph failed; searches stay exact");
                }
            })
            .map(drop),
    };
    if let Err(error) = started {
        tracing::warn!(%error, "vector graph not built");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A space of [`SYNC_BUILD`] one-dimensional vectors (its graph built in the
    /// background), and a search that uses its graph.
    fn large_space() -> (Arc<RwLock<VectorIndex>>, VectorQuery) {
        let literals: Vec<String> = (0..SYNC_BUILD)
            .map(|i| nrese_vector::lexical(&[i as f32]))
            .collect();
        let entries: Vec<(u64, &str)> = literals
            .iter()
            .enumerate()
            .map(|(id, literal)| (id as u64, literal.as_str()))
            .collect();
        let mut index = VectorIndex::default();
        index.extend(&entries, entries.len() as u64);
        (Arc::new(RwLock::new(index)), VectorQuery::new(vec![1.0], 1))
    }

    /// A background build holds its space's reservation until it ends, and only its own
    /// end releases it (PR #79 review, R79-01). The schedule of the public calls: a
    /// search starts background build A; a warm-up builds the graph meanwhile; more
    /// vectors arrive, and a search finds the graph behind again; A is cancelled. No
    /// second build starts while A holds the reservation (the warm-up's install doesn't
    /// end it), and after A's cancellation exactly one does, B, beside which no third
    /// starts until B ends. B installs its graph; more vectors arrive and a search starts
    /// C before B's end (its drop) runs, which leaves C's reservation standing (Sol r1).
    #[test]
    fn a_build_s_reservation_is_ended_by_that_build_alone() {
        let (index, query) = large_space();
        let held: Arc<parking_lot::Mutex<Vec<VectorBuild>>> = Arc::default();
        let builder: VectorBuilder = {
            let held = Arc::clone(&held);
            Arc::new(move |build| {
                held.lock().push(build);
                Ok(())
            })
        };
        // A search that needs the graph: what search_vectors does.
        let search = || {
            let work = index.write().work_for(&query);
            if let Work::Background(reservation) = work {
                build_in_background(
                    Arc::clone(&index),
                    query.clone(),
                    reservation,
                    Some(&builder),
                );
            }
        };
        search();
        assert_eq!(held.lock().len(), 1, "build A starts");
        // A warm-up (prepare_vector_graph) builds and installs the graph on its own.
        build(&index, &query, None);
        // A tenth more vectors: the graph is behind enough to be extended.
        let grow = || {
            let start = index.read().covered();
            let more: Vec<String> = (start..start + start / 10 + 1)
                .map(|i| nrese_vector::lexical(&[i as f32]))
                .collect();
            let entries: Vec<(u64, &str)> = more
                .iter()
                .enumerate()
                .map(|(at, literal)| (start + at as u64, literal.as_str()))
                .collect();
            index.write().extend(&entries, start + entries.len() as u64);
        };
        grow();
        search();
        assert_eq!(
            held.lock().len(),
            1,
            "no build beside A, whose reservation stands"
        );
        drop(held.lock().remove(0));
        search();
        assert_eq!(held.lock().len(), 1, "A cancelled: build B starts");
        search();
        assert_eq!(held.lock().len(), 1, "no build C beside B");
        // B's run up to its end: its install.
        let b = held.lock().remove(0);
        build(&b.index, &b.query, Some(b.reservation));
        grow();
        search();
        assert_eq!(held.lock().len(), 1, "B installed: build C starts");
        drop(b);
        search();
        assert_eq!(held.lock().len(), 1, "B's end leaves C's reservation: no D");
        held.lock().remove(0).run();
        assert!(
            !index.read().may_need_work(&query),
            "C's graph covers the space"
        );
    }

    /// A background build that couldn't start, or ended without a graph, leaves the space
    /// to the next search to start another (B-401: a refused spawn kept it marked as
    /// building, and no later search built its graph).
    #[test]
    fn a_build_that_ends_without_a_graph_is_started_again() {
        let (index, query) = large_space();
        let refuse: VectorBuilder = Arc::new(|_| Err(std::io::Error::other("no thread")));
        let abandon: VectorBuilder = Arc::new(|build| {
            drop(build);
            Ok(())
        });
        for (builder, ended) in [(refuse, "refused"), (abandon, "abandoned")] {
            let Work::Background(reservation) = index.write().work_for(&query) else {
                panic!("{ended}: a build starts");
            };
            build_in_background(
                Arc::clone(&index),
                query.clone(),
                reservation,
                Some(&builder),
            );
            assert!(
                index.read().may_need_work(&query),
                "{ended}: the next search starts another"
            );
        }
    }
}
