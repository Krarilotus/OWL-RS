//! HNSW: a hierarchy of proximity graphs (Malkov and Yashunin, TPAMI 2020). A search
//! descends greedily through the sparse upper levels, then explores level 0 with a beam
//! of `ef` candidates. Links are chosen by the paper's heuristic (a candidate is linked
//! only if it is nearer to the new node than to the links chosen before it), which keeps
//! clustered data navigable.
//!
//! - **Building** inserts the first nodes one by one, then the rest on every core: each
//!   node's links have a lock of their own, and no insertion holds two.
//! - **Filters**: a search takes the ids a query accepts. The beam walks through every
//!   node but keeps only accepted ones as results, so a filter that accepts most ids
//!   costs little; for one that accepts few, an exact scan of the accepted ids is cheaper
//!   (the query engine decides, [`crate::exact`]).
//! - **Growth**: [`Hnsw::extend`] links the vectors appended since; nothing is removed
//!   (the engine's vectors are append-only, a query checks that a hit is still used).

use std::cell::Cell;
use std::cmp::Reverse;

use parking_lot::RwLock;
use rayon::prelude::*;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec};

use crate::charged::Heap;
use crate::{Hit, Metric, Ordered, Query, Vectors};

/// How a graph is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswParameters {
    /// Links per node above level 0 (twice as many at level 0).
    pub m: usize,
    /// The beam while inserting: larger builds slower and finds better links.
    pub ef_construction: usize,
    /// Seeds the levels nodes get.
    pub seed: u64,
}

impl Default for HnswParameters {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            seed: 0x5eed_4e53_e5e5_0001,
        }
    }
}

/// A graph's parts ([`Hnsw::to_parts`]).
#[derive(Debug, Clone, PartialEq)]
pub struct HnswParts {
    pub parameters: HnswParameters,
    pub metric: Metric,
    /// The entry node and its level.
    pub entry: Option<(u32, u8)>,
    /// Each node's links, per level from 0 up.
    pub links: Vec<Vec<Vec<u32>>>,
}

/// Nodes inserted one by one before the rest go in parallel.
const SEQUENTIAL: usize = 256;

/// The highest level a node gets.
const MAX_LEVEL: u8 = 16;

/// An HNSW graph over the positions of a [`Vectors`].
#[derive(Debug)]
pub struct Hnsw {
    parameters: HnswParameters,
    metric: Metric,
    /// Each node's links, per level from 0 up to its own.
    links: Vec<RwLock<Vec<Vec<u32>>>>,
    /// The node searches start from, and its level.
    entry: RwLock<Option<(u32, u8)>>,
}

impl Hnsw {
    pub fn new(metric: Metric, parameters: HnswParameters) -> Self {
        Self {
            parameters,
            metric,
            links: Vec::new(),
            entry: RwLock::new(None),
        }
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// The nodes linked: the first positions of the vectors.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// The graph's parts, to write it out ([`Self::from_parts`] reads them back).
    pub fn to_parts(&self) -> HnswParts {
        HnswParts {
            parameters: self.parameters,
            metric: self.metric,
            entry: *self.entry.read(),
            links: self
                .links
                .iter()
                .map(|links| links.read().clone())
                .collect(),
        }
    }

    /// The graph of `parts`, if they make one: every link to a node of it, the entry
    /// one of them at its level.
    pub fn from_parts(parts: HnswParts) -> Option<Self> {
        let nodes = parts.links.len();
        let sound = parts.links.iter().all(|levels| {
            !levels.is_empty()
                && levels.len() <= usize::from(MAX_LEVEL) + 1
                && levels.iter().flatten().all(|&n| (n as usize) < nodes)
        });
        let entry_sound = match parts.entry {
            None => nodes == 0,
            Some((node, level)) => parts
                .links
                .get(node as usize)
                .is_some_and(|levels| levels.len() == usize::from(level) + 1),
        };
        (sound && entry_sound).then(|| Self {
            parameters: parts.parameters,
            metric: parts.metric,
            links: parts.links.into_iter().map(RwLock::new).collect(),
            entry: RwLock::new(parts.entry),
        })
    }

    /// A copy, to extend while searches use this one.
    pub fn duplicate(&self) -> Self {
        Self {
            parameters: self.parameters,
            metric: self.metric,
            links: self
                .links
                .iter()
                .map(|links| RwLock::new(links.read().clone()))
                .collect(),
            entry: RwLock::new(*self.entry.read()),
        }
    }

    /// Links the vectors appended to `vectors` since the last call.
    pub fn extend(&mut self, vectors: &Vectors) {
        let start = self.links.len();
        let end = vectors.len();
        for position in start..end {
            let level = self.level_of(position as u64);
            self.links
                .push(RwLock::new(vec![Vec::new(); usize::from(level) + 1]));
        }
        let sequential = end.min(start.max(SEQUENTIAL));
        for position in start..sequential {
            self.insert(vectors, position as u32);
        }
        (sequential..end)
            .into_par_iter()
            .for_each(|position| self.insert(vectors, position as u32));
    }

    /// The `k` nearest of the linked vectors to `query` that `accept` takes (by id),
    /// nearest first, with a beam of at least `ef`. The beam's heaps and the hits are
    /// charged to `budget` before they are allocated; a search the budget has no room for
    /// is refused, with nothing left charged. (The per-thread visited marks and one node's
    /// links are reused scratch of the graph's size, not of the search's.)
    pub fn search<'b>(
        &self,
        vectors: &Vectors,
        query: &[f32],
        k: usize,
        ef: usize,
        accept: &(dyn Fn(u64) -> bool + Sync),
        budget: &'b Budget,
    ) -> Result<ChargedVec<Hit, &'b Budget>, BudgetExceeded> {
        let Some((entry, top)) = *self.entry.read() else {
            return Ok(ChargedVec::new(budget));
        };
        if k == 0 || query.len() != vectors.dimension() {
            return Ok(ChargedVec::new(budget));
        }
        let query = Query::new(query);
        let mut nearest = (self.distance(vectors, &query, entry), entry);
        for level in (1..=top).rev() {
            nearest = self.greedy(vectors, &query, nearest, level);
        }
        let accepted = |position: u32| accept(vectors.id(position));
        let mut found =
            self.search_level(vectors, &query, &[nearest], ef.max(k), 0, &accepted, budget)?;
        found.truncate(k);
        let mut hits = ChargedVec::with_capacity(budget, found.len())?;
        for &(distance, position) in found.iter() {
            hits.push(Hit {
                id: vectors.id(position),
                distance,
            })?;
        }
        Ok(hits)
    }

    /// Bytes of the links.
    pub fn memory_bytes(&self) -> usize {
        self.links
            .iter()
            .map(|links| {
                let links = links.read();
                std::mem::size_of::<RwLock<Vec<Vec<u32>>>>()
                    + links
                        .iter()
                        .map(|level| level.capacity() * 4 + std::mem::size_of::<Vec<u32>>())
                        .sum::<usize>()
            })
            .sum()
    }

    /// A node's level: geometric with ratio 1/m, from a hash of its position.
    fn level_of(&self, position: u64) -> u8 {
        let mut z = position
            .wrapping_add(self.parameters.seed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let uniform = ((z >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let scale = 1.0 / (self.parameters.m.max(2) as f64).ln();
        ((-uniform.ln() * scale).floor() as u64).min(u64::from(MAX_LEVEL)) as u8
    }

    fn max_links(&self, level: u8) -> usize {
        match level {
            0 => self.parameters.m * 2,
            _ => self.parameters.m,
        }
    }

    fn distance(&self, vectors: &Vectors, query: &Query<'_>, position: u32) -> f32 {
        vectors.distance(self.metric, query, position)
    }

    fn insert(&self, vectors: &Vectors, position: u32) {
        let level = (self.links[position as usize].read().len() - 1) as u8;
        let query = vectors.query_at(position);
        let (entry, top) = {
            let current = *self.entry.read();
            match current {
                Some(entry) => entry,
                None => {
                    let mut entry = self.entry.write();
                    match *entry {
                        Some(found) => found,
                        None => {
                            *entry = Some((position, level));
                            return;
                        }
                    }
                }
            }
        };
        let mut nearest = (self.distance(vectors, &query, entry), entry);
        for upper in (level.saturating_add(1)..=top).rev() {
            nearest = self.greedy(vectors, &query, nearest, upper);
        }
        let mut entries = vec![nearest];
        for current in (0..=level.min(top)).rev() {
            // Building is the graph's own memory, outside any query (the store's derived
            // indexes): an unlimited budget, as before.
            let unlimited = Budget::unlimited();
            let candidates = self
                .search_level(
                    vectors,
                    &query,
                    &entries,
                    self.parameters.ef_construction,
                    current,
                    &|_| true,
                    &unlimited,
                )
                .expect("an unlimited budget refuses nothing");
            let candidates: Vec<(f32, u32)> = candidates
                .iter()
                .copied()
                .filter(|&(_, found)| found != position)
                .collect();
            let chosen = self.select(vectors, &candidates, self.parameters.m);
            self.links[position as usize].write()[usize::from(current)] =
                chosen.iter().map(|&(_, found)| found).collect();
            for &(distance, neighbour) in &chosen {
                self.link(vectors, neighbour, position, distance, current);
            }
            if !candidates.is_empty() {
                entries = candidates;
            }
        }
        if level > top {
            let mut entry = self.entry.write();
            if entry.is_none_or(|(_, highest)| level > highest) {
                *entry = Some((position, level));
            }
        }
    }

    /// Adds `new` (at `distance`) to `node`'s links at `level`, choosing again among
    /// them if there are too many.
    fn link(&self, vectors: &Vectors, node: u32, new: u32, distance: f32, level: u8) {
        let mut links = self.links[node as usize].write();
        let Some(own) = links.get_mut(usize::from(level)) else {
            return;
        };
        if own.contains(&new) {
            return;
        }
        if own.len() < self.max_links(level) {
            own.push(new);
            return;
        }
        let from = vectors.query_at(node);
        let mut candidates: Vec<(f32, u32)> = own
            .iter()
            .map(|&other| (self.distance(vectors, &from, other), other))
            .collect();
        candidates.push((distance, new));
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let chosen = self.select(vectors, &candidates, self.max_links(level));
        *own = chosen.into_iter().map(|(_, other)| other).collect();
    }

    /// The paper's heuristic: of `candidates` (nearest first), each one nearer to the
    /// node than to every one chosen before it, at most `m`.
    fn select(&self, vectors: &Vectors, candidates: &[(f32, u32)], m: usize) -> Vec<(f32, u32)> {
        let mut chosen: Vec<(f32, u32)> = Vec::with_capacity(m);
        for &(distance, candidate) in candidates {
            if chosen.len() == m {
                break;
            }
            let from = vectors.query_at(candidate);
            if chosen
                .iter()
                .all(|&(_, kept)| self.distance(vectors, &from, kept) > distance)
            {
                chosen.push((distance, candidate));
            }
        }
        chosen
    }

    /// O(degree): copies ordered links into reusable storage, releasing the lock before
    /// the caller processes neighbours.
    fn snapshot_links(&self, node: u32, level: u8, out: &mut Vec<u32>) {
        let links = self.links[node as usize].read();
        match links.get(usize::from(level)) {
            Some(neighbours) => out.clone_from(neighbours),
            None => out.clear(),
        }
    }

    /// The nearest node to `query` reachable from `start` by steps that get nearer, at
    /// `level`.
    fn greedy(
        &self,
        vectors: &Vectors,
        query: &Query<'_>,
        start: (f32, u32),
        level: u8,
    ) -> (f32, u32) {
        let mut nearest = start;
        let mut links = Vec::new();
        loop {
            let mut improved = false;
            self.snapshot_links(nearest.1, level, &mut links);
            for &neighbour in &links {
                let distance = self.distance(vectors, query, neighbour);
                if distance < nearest.0 {
                    nearest = (distance, neighbour);
                    improved = true;
                }
            }
            if !improved {
                return nearest;
            }
        }
    }

    /// The `ef` nearest nodes to `query` at `level` that `accept` takes, nearest first:
    /// a beam search from `entries`.
    #[allow(clippy::too_many_arguments)]
    fn search_level<'b>(
        &self,
        vectors: &Vectors,
        query: &Query<'_>,
        entries: &[(f32, u32)],
        ef: usize,
        level: u8,
        accept: &dyn Fn(u32) -> bool,
        budget: &'b Budget,
    ) -> Result<ChargedVec<(f32, u32), &'b Budget>, BudgetExceeded> {
        with_visited(self.links.len(), |visited| {
            let mut candidates: Heap<'b, Reverse<Ordered<u32>>> = Heap::with_capacity(budget, 0)?;
            let mut results: Heap<'b, Ordered<u32>> = Heap::with_capacity(budget, 0)?;
            let mut links = Vec::new();
            for &(distance, node) in entries {
                if visited.insert(node) {
                    candidates.push(Reverse(Ordered(distance, node)))?;
                    if accept(node) {
                        results.push(Ordered(distance, node))?;
                    }
                }
            }
            while results.len() > ef {
                results.pop();
            }
            while let Some(Reverse(Ordered(distance, node))) = candidates.pop() {
                let full = results.len() >= ef;
                if full && results.peek().is_some_and(|worst| distance > worst.0) {
                    break;
                }
                self.snapshot_links(node, level, &mut links);
                for &neighbour in &links {
                    if !visited.insert(neighbour) {
                        continue;
                    }
                    let distance = self.distance(vectors, query, neighbour);
                    let full = results.len() >= ef;
                    if full && results.peek().is_some_and(|worst| distance >= worst.0) {
                        continue;
                    }
                    candidates.push(Reverse(Ordered(distance, neighbour)))?;
                    if accept(neighbour) {
                        results.push(Ordered(distance, neighbour))?;
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
            drop(candidates);
            // Nearest first, by distance then node.
            results.into_sorted(|Ordered(distance, node)| (distance, node))
        })
    }
}

/// Nodes visited by one search: a stamp per node, reused by the thread's next search.
struct Visited<'a> {
    marks: &'a mut Vec<u32>,
    stamp: u32,
}

impl Visited<'_> {
    /// Whether `node` is new to this search (it is visited from now on).
    fn insert(&mut self, node: u32) -> bool {
        let mark = &mut self.marks[node as usize];
        if *mark == self.stamp {
            return false;
        }
        *mark = self.stamp;
        true
    }
}

thread_local! {
    /// The thread's visited marks and their last stamp, kept between its searches.
    static VISITED: Cell<(Vec<u32>, u32)> = const { Cell::new((Vec::new(), 0)) };
}

/// Runs `f` with visited marks for `nodes` nodes, none visited yet. The marks are taken
/// out of the thread's for the run and put back after, so a search started inside `f` on
/// this thread (by `accept`, or a Rayon part taken on while it waits) gets marks of its
/// own: the thread's then hold none, and it allocates.
fn with_visited<R>(nodes: usize, f: impl FnOnce(&mut Visited<'_>) -> R) -> R {
    let (mut marks, mut stamp) = VISITED.take();
    if marks.len() < nodes {
        marks.resize(nodes, 0);
    }
    stamp = stamp.wrapping_add(1);
    if stamp == 0 {
        marks.iter_mut().for_each(|mark| *mark = 0);
        stamp = 1;
    }
    let result = f(&mut Visited {
        marks: &mut marks,
        stamp,
    });
    // The larger of these marks and any a nested search put back is kept.
    let kept = VISITED.take();
    VISITED.set(if kept.0.len() > marks.len() {
        kept
    } else {
        (marks, stamp)
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{exact, tests::random_vectors};

    #[test]
    fn adjacency_snapshots_reuse_storage_and_release_the_lock() {
        let ordered = vec![2, 1, 2, 0, 1, 0, 2, 1];
        let graph = Hnsw::from_parts(HnswParts {
            parameters: HnswParameters::default(),
            metric: Metric::L2,
            entry: Some((0, 0)),
            links: vec![vec![ordered.clone()], vec![vec![0]], vec![Vec::new()]],
        })
        .expect("sound parts");
        let mut snapshot = Vec::new();
        graph.snapshot_links(0, 0, &mut snapshot);
        let pointer = snapshot.as_ptr();
        let capacity = snapshot.capacity();
        for (node, level, expected) in [
            (0, 0, ordered.as_slice()),
            (1, 0, &[0]),
            (2, 0, &[]),
            (0, 0, ordered.as_slice()),
            (1, 1, &[]),
        ] {
            graph.snapshot_links(node, level, &mut snapshot);
            assert_eq!(snapshot, expected);
            assert_eq!(snapshot.as_ptr(), pointer);
            assert_eq!(snapshot.capacity(), capacity);
            let mut levels = graph.links[node as usize]
                .try_write()
                .expect("snapshot released its read lock");
            if let Some(neighbours) = levels.get_mut(usize::from(level)) {
                neighbours.reverse();
                assert_eq!(snapshot, expected, "snapshot owns its copied links");
                neighbours.reverse();
            }
        }
    }

    /// The share of the exact `k` nearest that HNSW finds, over `queries` queries.
    fn recall(
        vectors: &Vectors,
        graph: &Hnsw,
        k: usize,
        ef: usize,
        accept: &(dyn Fn(u64) -> bool + Sync),
    ) -> f64 {
        let queries = 50;
        let mut found = 0;
        for q in 0..queries {
            let query: Vec<f32> = vectors
                .get((q * 97) as u32)
                .iter()
                .enumerate()
                .map(|(i, v)| v + 0.05 * ((i + q) as f32).sin())
                .collect();
            let truth = exact(
                vectors,
                &query,
                k,
                graph.metric(),
                accept,
                &Budget::unlimited(),
            )
            .unwrap()
            .to_vec();
            let hits = graph
                .search(vectors, &query, k, ef, accept, &Budget::unlimited())
                .unwrap()
                .to_vec();
            assert!(hits.iter().all(|hit| accept(hit.id)));
            found += hits
                .iter()
                .filter(|hit| truth.iter().any(|t| t.id == hit.id))
                .count();
        }
        found as f64 / (queries * k) as f64
    }

    #[test]
    fn hnsw_finds_nearly_all_nearest_neighbours() {
        let vectors = random_vectors(20_000, 16, 3);
        for metric in [Metric::Cosine, Metric::L2] {
            let mut graph = Hnsw::new(metric, HnswParameters::default());
            // In two parts: the second extends the graph.
            let mut first = Vectors::new(16);
            for position in 0..10_000 {
                first.push(vectors.id(position), vectors.get(position));
            }
            graph.extend(&first);
            graph.extend(&vectors);
            assert_eq!(graph.len(), vectors.len());
            let all = recall(&vectors, &graph, 10, 64, &|_| true);
            assert!(all >= 0.95, "{metric:?}: recall {all}");
            // A filter that accepts half of the ids.
            let half = recall(&vectors, &graph, 10, 64, &|id| id % 2 == 0);
            assert!(half >= 0.9, "{metric:?}: recall with a filter {half}");
        }
    }

    /// Under every budget from none to what it held at its peak, a graph search either
    /// answers as unlimited or is refused, and holds nothing afterwards either way (review
    /// fix check, S2).
    #[test]
    fn graph_searches_refused_partway_release_everything() {
        let vectors = random_vectors(2_000, 8, 5);
        let mut graph = Hnsw::new(Metric::L2, HnswParameters::default());
        graph.extend(&vectors);
        let query = vectors.get(7).to_vec();
        let unlimited = Budget::unlimited();
        let truth = graph
            .search(&vectors, &query, 50, 200, &|_| true, &unlimited)
            .unwrap()
            .to_vec();
        let peak = unlimited.peak();
        let (mut answered, mut refused) = (0, 0);
        // The peak itself is one of the limits: the search is on this thread alone, so its
        // peak is the same every run, and that run answers.
        for limit in (0..peak).step_by((peak / 64).max(1)).chain([peak]) {
            let budget = Budget::new(limit);
            match graph.search(&vectors, &query, 50, 200, &|_| true, &budget) {
                Ok(found) => {
                    assert_eq!(*found, truth, "limit {limit}");
                    answered += 1;
                }
                Err(_) => refused += 1,
            }
            assert_eq!(budget.used(), 0, "limit {limit}: released");
        }
        assert!(
            answered > 0 && refused > 0,
            "{answered} answered, {refused} refused"
        );
    }

    #[test]
    fn small_and_empty_graphs_answer() {
        let mut graph = Hnsw::new(Metric::Cosine, HnswParameters::default());
        let mut vectors = Vectors::new(3);
        assert!(
            graph
                .search(
                    &vectors,
                    &[1.0, 0.0, 0.0],
                    5,
                    10,
                    &|_| true,
                    &Budget::unlimited()
                )
                .unwrap()
                .to_vec()
                .is_empty()
        );
        vectors.push(7, &[1.0, 0.0, 0.0]);
        vectors.push(8, &[0.0, 1.0, 0.0]);
        graph.extend(&vectors);
        let hits = graph
            .search(
                &vectors,
                &[0.9, 0.1, 0.0],
                5,
                10,
                &|_| true,
                &Budget::unlimited(),
            )
            .unwrap()
            .to_vec();
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), [7, 8]);
        assert!(
            graph
                .search(
                    &vectors,
                    &[1.0, 0.0],
                    5,
                    10,
                    &|_| true,
                    &Budget::unlimited()
                )
                .unwrap()
                .to_vec()
                .is_empty()
        );
    }

    /// A search's `accept` may search the graph again on its own thread (an embedder's
    /// filter that searches, or a Rayon part a waiting thread takes on): both answer as a
    /// search alone does. Each search had borrowed its thread's visited marks for its whole
    /// run, and the nested one panicked on the second borrow.
    #[test]
    fn a_search_s_accept_may_search_again_on_its_thread() {
        let vectors = random_vectors(500, 8, 7);
        let mut graph = Hnsw::new(Metric::L2, HnswParameters::default());
        graph.extend(&vectors);
        let query = vectors.get(42).to_vec();
        let unlimited = Budget::unlimited();
        let ids = |hits: Result<ChargedVec<Hit, &Budget>, BudgetExceeded>| {
            hits.unwrap().iter().map(|hit| hit.id).collect::<Vec<_>>()
        };
        let alone = ids(graph.search(&vectors, &query, 10, 64, &|_| true, &unlimited));
        let nested = std::sync::Mutex::new(None);
        let outer = ids(graph.search(
            &vectors,
            &query,
            10,
            64,
            &|_| {
                let mut nested = nested.lock().expect("unpoisoned");
                if nested.is_none() {
                    *nested = Some(ids(graph.search(
                        &vectors,
                        &query,
                        10,
                        64,
                        &|_| true,
                        &unlimited,
                    )));
                }
                true
            },
            &unlimited,
        ));
        assert_eq!(
            nested.into_inner().expect("unpoisoned"),
            Some(alone.clone())
        );
        assert_eq!(outer, alone);
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;
    use crate::tests::random_vectors;
    use crate::{exact, exact_from, merge};

    /// A graph's parts read back give the same answers; a copy extended with new vectors
    /// finds them, the original stays as it was; a graph over a prefix plus an exact scan
    /// of the rest answers as one graph does.
    #[test]
    fn graphs_round_trip_grow_and_cover_a_prefix() {
        let all = random_vectors(6_000, 8, 11);
        let mut prefix = Vectors::new(8);
        for p in 0..4_000u32 {
            prefix.push(all.id(p), all.get(p));
        }
        let mut graph = Hnsw::new(Metric::L2, HnswParameters::default());
        graph.extend(&prefix);
        let query = all.get(5_000).to_vec();
        let read = Hnsw::from_parts(graph.to_parts()).expect("sound parts");
        assert_eq!(
            read.search(&prefix, &query, 5, 64, &|_| true, &Budget::unlimited())
                .unwrap()
                .to_vec(),
            graph
                .search(&prefix, &query, 5, 64, &|_| true, &Budget::unlimited())
                .unwrap()
                .to_vec()
        );
        // Prefix graph + exact tail: the true nearest is the queried vector itself.
        let from_graph = graph
            .search(&all, &query, 5, 64, &|_| true, &Budget::unlimited())
            .unwrap()
            .to_vec();
        let tail = exact_from(
            &all,
            graph.len(),
            &query,
            5,
            Metric::L2,
            &|_| true,
            &Budget::unlimited(),
        )
        .unwrap()
        .to_vec();
        let merged = merge(from_graph.into_iter().chain(tail), 5, &Budget::unlimited())
            .unwrap()
            .to_vec();
        assert_eq!(merged[0].id, all.id(5_000));
        // The graph's part is approximate (and its parallel build not deterministic): at
        // least 4 of the exact 5 nearest, the exact tail's own hit first.
        let truth: Vec<u64> = exact(&all, &query, 5, Metric::L2, &|_| true, &Budget::unlimited())
            .unwrap()
            .to_vec()[..5]
            .iter()
            .map(|hit| hit.id)
            .collect();
        let found = merged.iter().filter(|hit| truth.contains(&hit.id)).count();
        assert!(found >= 4, "{merged:?} against {truth:?}");
        // A copy extended to every vector finds it through the graph; the original
        // still covers the prefix.
        let mut grown = graph.duplicate();
        grown.extend(&all);
        assert_eq!(grown.len(), 6_000);
        assert_eq!(graph.len(), 4_000);
        assert_eq!(
            grown
                .search(&all, &query, 1, 64, &|_| true, &Budget::unlimited())
                .unwrap()
                .to_vec()[0]
                .id,
            all.id(5_000)
        );
        // Unsound parts are refused.
        let mut bad = graph.to_parts();
        bad.links[0][0].push(1_000_000);
        assert!(Hnsw::from_parts(bad).is_none());
    }
}
