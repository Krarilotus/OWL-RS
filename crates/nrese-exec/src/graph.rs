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

/// The transitive closure of `edges`: every `(a, b)` with `b` reachable from `a` in one or
/// more steps, each once, in no particular order.
///
/// Nodes in one strongly connected component reach the same set, so reachability is
/// computed per component (Nuutila's approach). Tarjan's algorithm finds the components
/// in reverse topological order, so each component's reach is the union of its
/// successors' reach, computed after theirs. Apart from the output, the cost is
/// O(n + m) plus the total size of the components' reach sets. On a clique of k nodes
/// that's O(k²), the size of the output, where a search per node ([`closure`]) is O(k³).
pub fn transitive_closure(edges: &[(u64, u64)]) -> Vec<(u64, u64)> {
    use rayon::prelude::*;

    // Dense node numbering and CSR over it.
    let mut nodes: Vec<u64> = edges.iter().flat_map(|&(a, b)| [a, b]).collect();
    nodes.par_sort_unstable();
    nodes.dedup();
    let n = nodes.len();
    let dense = |id: u64| nodes.binary_search(&id).expect("every endpoint is a node") as u32;
    let mut pairs: Vec<(u32, u32)> = edges.par_iter().map(|&(a, b)| (dense(a), dense(b))).collect();
    pairs.par_sort_unstable();
    pairs.dedup();
    let mut offsets = vec![0u32; n + 1];
    for &(a, _) in &pairs {
        offsets[a as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let targets: Vec<u32> = pairs.iter().map(|&(_, b)| b).collect();
    let successors = |v: u32| &targets[offsets[v as usize] as usize..offsets[v as usize + 1] as usize];

    // Tarjan's algorithm, iterative. Components come out sinks first.
    const UNSEEN: u32 = u32::MAX;
    struct Tarjan {
        order: Vec<u32>,
        low: Vec<u32>,
        on_stack: Vec<bool>,
        stack: Vec<u32>,
        /// The depth-first path: each node with the index of its next successor.
        calls: Vec<(u32, usize)>,
        counter: u32,
    }
    impl Tarjan {
        fn visit(&mut self, v: u32) {
            self.order[v as usize] = self.counter;
            self.low[v as usize] = self.counter;
            self.counter += 1;
            self.stack.push(v);
            self.on_stack[v as usize] = true;
            self.calls.push((v, 0));
        }
    }
    let mut t = Tarjan {
        order: vec![UNSEEN; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        calls: Vec::new(),
        counter: 0,
    };
    let mut component = vec![0u32; n];
    // `members[starts[c]..starts[c + 1]]` are component c's nodes.
    let (mut members, mut starts) = (Vec::with_capacity(n), vec![0usize]);
    for root in 0..n as u32 {
        if t.order[root as usize] != UNSEEN {
            continue;
        }
        t.visit(root);
        while let Some(&(v, next)) = t.calls.last() {
            if let Some(&w) = successors(v).get(next) {
                t.calls.last_mut().expect("not empty").1 += 1;
                if t.order[w as usize] == UNSEEN {
                    t.visit(w);
                } else if t.on_stack[w as usize] {
                    t.low[v as usize] = t.low[v as usize].min(t.order[w as usize]);
                }
                continue;
            }
            t.calls.pop();
            if let Some(&(u, _)) = t.calls.last() {
                t.low[u as usize] = t.low[u as usize].min(t.low[v as usize]);
            }
            if t.low[v as usize] == t.order[v as usize] {
                let c = starts.len() as u32 - 1;
                loop {
                    let w = t.stack.pop().expect("v is on the stack");
                    t.on_stack[w as usize] = false;
                    component[w as usize] = c;
                    members.push(w);
                    if w == v {
                        break;
                    }
                }
                starts.push(members.len());
            }
        }
    }

    // Reach per component, successors first. A component reaches itself if it has a
    // cycle: more than one node, or a self-loop.
    let components = starts.len() - 1;
    let mut reach: Vec<Vec<u32>> = Vec::with_capacity(components);
    let mut stamp = vec![u32::MAX; components];
    for c in 0..components {
        let mut set = Vec::new();
        let nodes_of_c = &members[starts[c]..starts[c + 1]];
        let mut cyclic = nodes_of_c.len() > 1;
        for &x in nodes_of_c {
            for &y in successors(x) {
                let d = component[y as usize];
                if d as usize == c {
                    cyclic = true;
                    continue;
                }
                if stamp[d as usize] != c as u32 {
                    stamp[d as usize] = c as u32;
                    set.push(d);
                    for &e in &reach[d as usize] {
                        if stamp[e as usize] != c as u32 {
                            stamp[e as usize] = c as u32;
                            set.push(e);
                        }
                    }
                }
            }
        }
        if cyclic {
            set.push(c as u32);
        }
        reach.push(set);
    }

    (0..components)
        .into_par_iter()
        .flat_map_iter(|c| {
            let (members, starts, reach, nodes) = (&members, &starts, &reach, &nodes);
            members[starts[c]..starts[c + 1]].iter().flat_map(move |&x| {
                reach[c].iter().flat_map(move |&d| {
                    members[starts[d as usize]..starts[d as usize + 1]]
                        .iter()
                        .map(move |&y| (nodes[x as usize], nodes[y as usize]))
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitive_closure_matches_search_per_node() {
        let mut state = 99u64;
        for _ in 0..400 {
            let mut next = |n: u64| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) % n
            };
            let size = 2 + next(30);
            let edges: Vec<(u64, u64)> = (0..next(3 * size))
                .map(|_| (100 + next(size), 100 + next(size)))
                .collect();
            let adjacency = Adjacency::new(edges.clone());
            let mut expected: Vec<(u64, u64)> = closure(&adjacency, adjacency.sources().to_vec(), false)
                .rows()
                .map(|r| (r[0], r[1]))
                .collect();
            expected.sort_unstable();
            let mut got = transitive_closure(&edges);
            got.sort_unstable();
            assert_eq!(got, expected, "{edges:?}");
        }
        // A clique: every pair, including each node with itself.
        let clique: Vec<(u64, u64)> = (0..50).flat_map(|a| [(a, (a + 1) % 50), ((a + 1) % 50, a)]).collect();
        assert_eq!(transitive_closure(&clique).len(), 50 * 50);
    }

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
