//! Graph algorithms over id edge relations: reachability for SPARQL property paths
//! (`p+`, `p*`) and, in the reasoner, for hierarchies and transitive properties.
//!
//! [`Adjacency`] is a compressed sparse row index over `(source, target)` edges: sorted
//! targets per source, found by binary search over the distinct sources. [`reachable`]
//! walks it breadth-first with a visited set, so each node is expanded once: set semantics,
//! as SPARQL requires for `*` and `+`, and cycles terminate.

use hashbrown::HashSet;

use crate::table::IdTable;

type Hasher = foldhash::fast::FixedState;

/// Outgoing edges per source node, in CSR form.
#[derive(Debug, Clone, Default)]
pub struct Adjacency {
    sources: Vec<u64>,
    /// `targets[offsets[i]..offsets[i + 1]]` are the targets of `sources[i]`, sorted, unique.
    offsets: Vec<usize>,
    targets: Vec<u64>,
}

impl Adjacency {
    /// Builds the index from `(source, target)` pairs; duplicates are removed.
    pub fn new(mut edges: Vec<(u64, u64)>) -> Self {
        edges.sort_unstable();
        edges.dedup();
        let mut adjacency = Self::default();
        for (source, target) in edges {
            if adjacency.sources.last() != Some(&source) {
                adjacency.sources.push(source);
                adjacency.offsets.push(adjacency.targets.len());
            }
            adjacency.targets.push(target);
        }
        adjacency.offsets.push(adjacency.targets.len());
        adjacency
    }

    /// Builds the index from two columns of a table (source column, target column).
    pub fn from_table(table: &IdTable, source: usize, target: usize) -> Self {
        Self::new(
            table
                .column(source)
                .iter()
                .copied()
                .zip(table.column(target).iter().copied())
                .collect(),
        )
    }

    /// The same edges reversed.
    pub fn reversed(&self) -> Self {
        Self::new(self.edges().map(|(s, t)| (t, s)).collect())
    }

    pub fn edges(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.sources
            .iter()
            .enumerate()
            .flat_map(move |(i, &source)| {
                self.targets[self.offsets[i]..self.offsets[i + 1]]
                    .iter()
                    .map(move |&target| (source, target))
            })
    }

    /// Sources with at least one edge, sorted.
    pub fn sources(&self) -> &[u64] {
        &self.sources
    }

    pub fn neighbours(&self, node: u64) -> &[u64] {
        match self.sources.binary_search(&node) {
            Ok(i) => &self.targets[self.offsets[i]..self.offsets[i + 1]],
            Err(_) => &[],
        }
    }
}

/// Nodes reachable from `start` in one or more steps (`include_start == false`) or zero or
/// more (`include_start == true`), each once, in breadth-first order. `neighbours` gives a
/// node's successors: an [`Adjacency`], or index probes for a bound start.
pub fn reachable(
    start: u64,
    include_start: bool,
    mut neighbours: impl FnMut(u64, &mut Vec<u64>),
) -> Vec<u64> {
    let mut visited: HashSet<u64, Hasher> = HashSet::with_hasher(Hasher::default());
    let mut order = Vec::new();
    if include_start {
        visited.insert(start);
        order.push(start);
    }
    let mut frontier = vec![start];
    let mut next = Vec::new();
    let mut buffer = Vec::new();
    while !frontier.is_empty() {
        for &node in &frontier {
            buffer.clear();
            neighbours(node, &mut buffer);
            for &target in &buffer {
                if visited.insert(target) {
                    order.push(target);
                    next.push(target);
                }
            }
        }
        std::mem::swap(&mut frontier, &mut next);
        next.clear();
    }
    order
}

/// Every `(start, end)` pair of the closure from each of `starts` over `adjacency`: `end`
/// reachable in one or more steps, or zero or more with `reflexive`. Pairs are unique.
pub fn closure(
    adjacency: &Adjacency,
    starts: impl IntoIterator<Item = u64>,
    reflexive: bool,
) -> IdTable {
    let mut out = IdTable::new(2);
    for start in starts {
        for end in reachable(start, reflexive, |node, buffer| {
            buffer.extend_from_slice(adjacency.neighbours(node))
        }) {
            out.push_row(&[start, end]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Floyd–Warshall-style fixpoint over a tiny graph as the reference.
    fn naive_closure(edges: &[(u64, u64)], nodes: &[u64], reflexive: bool) -> Vec<(u64, u64)> {
        let mut pairs: std::collections::BTreeSet<(u64, u64)> = edges.iter().copied().collect();
        loop {
            let extra: Vec<(u64, u64)> = pairs
                .iter()
                .flat_map(|&(a, b)| {
                    pairs
                        .iter()
                        .filter(move |&&(c, _)| c == b)
                        .map(move |&(_, d)| (a, d))
                })
                .filter(|p| !pairs.contains(p))
                .collect();
            if extra.is_empty() {
                break;
            }
            pairs.extend(extra);
        }
        if reflexive {
            pairs.extend(nodes.iter().map(|&n| (n, n)));
        }
        pairs
            .into_iter()
            .filter(|(a, _)| nodes.contains(a))
            .collect()
    }

    #[test]
    fn closure_matches_the_fixpoint_with_cycles_and_duplicates() {
        let mut state = 17u64;
        for _ in 0..200 {
            let mut next = || {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) % 9
            };
            let edges: Vec<(u64, u64)> = (0..next() * 2).map(|_| (next(), next())).collect();
            let nodes: Vec<u64> = (0..9).collect();
            let adjacency = Adjacency::new(edges.clone());
            for reflexive in [false, true] {
                let mut got: Vec<(u64, u64)> =
                    closure(&adjacency, nodes.iter().copied(), reflexive)
                        .rows()
                        .map(|r| (r[0], r[1]))
                        .collect();
                got.sort_unstable();
                assert_eq!(
                    got,
                    naive_closure(&edges, &nodes, reflexive),
                    "{edges:?} reflexive={reflexive}"
                );
            }
        }
    }

    #[test]
    fn reversed_and_neighbours() {
        let adjacency = Adjacency::new(vec![(1, 2), (1, 3), (1, 2), (4, 1)]);
        assert_eq!(adjacency.neighbours(1), &[2, 3]);
        assert_eq!(adjacency.neighbours(9), &[] as &[u64]);
        assert_eq!(adjacency.reversed().neighbours(1), &[4]);
        assert_eq!(adjacency.sources(), &[1, 4]);
    }
}
