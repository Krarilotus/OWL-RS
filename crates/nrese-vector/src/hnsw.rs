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

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use parking_lot::RwLock;
use rayon::prelude::*;

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
    /// nearest first, with a beam of at least `ef`.
    pub fn search(
        &self,
        vectors: &Vectors,
        query: &[f32],
        k: usize,
        ef: usize,
        accept: &(dyn Fn(u64) -> bool + Sync),
    ) -> Vec<Hit> {
        let Some((entry, top)) = *self.entry.read() else {
            return Vec::new();
        };
        if k == 0 || query.len() != vectors.dimension() {
            return Vec::new();
        }
        let query = Query::new(query);
        let mut nearest = (self.distance(vectors, &query, entry), entry);
        for level in (1..=top).rev() {
            nearest = self.greedy(vectors, &query, nearest, level);
        }
        let accepted = |position: u32| accept(vectors.id(position));
        let mut found = self.search_level(vectors, &query, &[nearest], ef.max(k), 0, &accepted);
        found.truncate(k);
        found
            .into_iter()
            .map(|(distance, position)| Hit {
                id: vectors.id(position),
                distance,
            })
            .collect()
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
        let query = Query::new(vectors.get(position));
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
            let candidates = self.search_level(
                vectors,
                &query,
                &entries,
                self.parameters.ef_construction,
                current,
                &|_| true,
            );
            let candidates: Vec<(f32, u32)> = candidates
                .into_iter()
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
        let from = Query::new(vectors.get(node));
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
            let from = Query::new(vectors.get(candidate));
            if chosen
                .iter()
                .all(|&(_, kept)| self.distance(vectors, &from, kept) > distance)
            {
                chosen.push((distance, candidate));
            }
        }
        chosen
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
        loop {
            let mut improved = false;
            let links = self.links[nearest.1 as usize]
                .read()
                .get(usize::from(level))
                .cloned()
                .unwrap_or_default();
            for neighbour in links {
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
    fn search_level(
        &self,
        vectors: &Vectors,
        query: &Query<'_>,
        entries: &[(f32, u32)],
        ef: usize,
        level: u8,
        accept: &dyn Fn(u32) -> bool,
    ) -> Vec<(f32, u32)> {
        with_visited(self.links.len(), |visited| {
            let mut candidates: BinaryHeap<Reverse<Ordered<u32>>> = BinaryHeap::new();
            let mut results: BinaryHeap<Ordered<u32>> = BinaryHeap::new();
            for &(distance, node) in entries {
                if visited.insert(node) {
                    candidates.push(Reverse(Ordered(distance, node)));
                    if accept(node) {
                        results.push(Ordered(distance, node));
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
                let links = self.links[node as usize]
                    .read()
                    .get(usize::from(level))
                    .cloned()
                    .unwrap_or_default();
                for neighbour in links {
                    if !visited.insert(neighbour) {
                        continue;
                    }
                    let distance = self.distance(vectors, query, neighbour);
                    let full = results.len() >= ef;
                    if full && results.peek().is_some_and(|worst| distance >= worst.0) {
                        continue;
                    }
                    candidates.push(Reverse(Ordered(distance, neighbour)));
                    if accept(neighbour) {
                        results.push(Ordered(distance, neighbour));
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
            let mut found: Vec<(f32, u32)> = results
                .into_iter()
                .map(|Ordered(distance, node)| (distance, node))
                .collect();
            found.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            found
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
    static VISITED: RefCell<(Vec<u32>, u32)> = const { RefCell::new((Vec::new(), 0)) };
}

fn with_visited<R>(nodes: usize, f: impl FnOnce(&mut Visited<'_>) -> R) -> R {
    VISITED.with(|cell| {
        let mut cell = cell.borrow_mut();
        let (marks, stamp) = &mut *cell;
        if marks.len() < nodes {
            marks.resize(nodes, 0);
        }
        *stamp = stamp.wrapping_add(1);
        if *stamp == 0 {
            marks.iter_mut().for_each(|mark| *mark = 0);
            *stamp = 1;
        }
        let stamp = *stamp;
        f(&mut Visited { marks, stamp })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{exact, tests::random_vectors};

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
            let truth = exact(vectors, &query, k, graph.metric(), accept);
            let hits = graph.search(vectors, &query, k, ef, accept);
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

    #[test]
    fn small_and_empty_graphs_answer() {
        let mut graph = Hnsw::new(Metric::Cosine, HnswParameters::default());
        let mut vectors = Vectors::new(3);
        assert!(
            graph
                .search(&vectors, &[1.0, 0.0, 0.0], 5, 10, &|_| true)
                .is_empty()
        );
        vectors.push(7, &[1.0, 0.0, 0.0]);
        vectors.push(8, &[0.0, 1.0, 0.0]);
        graph.extend(&vectors);
        let hits = graph.search(&vectors, &[0.9, 0.1, 0.0], 5, 10, &|_| true);
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), [7, 8]);
        assert!(
            graph
                .search(&vectors, &[1.0, 0.0], 5, 10, &|_| true)
                .is_empty()
        );
    }
}
