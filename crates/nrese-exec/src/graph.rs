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
/// Computed per strongly connected component ([`Condensation`]). Apart from the output,
/// the cost is O(n + m) plus the total size of the components' reach sets. On a clique of
/// k nodes that's O(k²), the size of the output, where a search per node ([`closure`]) is
/// O(k³).
pub fn transitive_closure(edges: &[(u64, u64)]) -> Vec<(u64, u64)> {
    transitive_closure_until(edges, &|| false).expect("never stopped")
}

/// [`transitive_closure`], polling `stop` while it works; `None` if it fired. The
/// output of a large relation can be quadratic in its size, so this is what makes a
/// newly declared transitive property's closure cancellable.
pub fn transitive_closure_until(
    edges: &[(u64, u64)],
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<(u64, u64)>> {
    use rayon::prelude::*;

    let g = Condensation::of(edges, stop)?;
    let out: Vec<(u64, u64)> = (0..g.components())
        .into_par_iter()
        .flat_map_iter(|c| {
            let g = &g;
            // A stopped run produces nothing more; the caller discards what it has.
            let stopped = c % 256 == 0 && stop();
            g.members_of(if stopped { usize::MAX } else { c })
                .iter()
                .flat_map(move |&x| {
                    g.reach[c].iter().flat_map(move |&d| {
                        g.members_of(d as usize)
                            .iter()
                            .map(move |&y| (g.nodes[x as usize], g.nodes[y as usize]))
                    })
                })
        })
        .collect();
    (!stop()).then_some(out)
}

/// For every node of `edges` with at least one edge out, sorted: the node, the number of
/// nodes it reaches in one or more steps, and whether it reaches itself (it lies on a
/// cycle). The size of the transitive closure without building it, which is what `COUNT`
/// over `p+` and `p*` needs. Cost as [`transitive_closure`] without the output. `None` if
/// `stop` fired.
pub fn closure_sizes_until(
    edges: &[(u64, u64)],
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<(u64, u64, bool)>> {
    let g = Condensation::of(edges, stop)?;
    let size: Vec<u64> = (0..g.components())
        .map(|c| {
            g.reach[c]
                .iter()
                .map(|&d| g.members_of(d as usize).len() as u64)
                .sum()
        })
        .collect();
    // Dense ids follow the sorted node ids, so the output comes out sorted.
    Some(
        (0..g.nodes.len() as u32)
            .filter(|&x| g.has_successors(x))
            .map(|x| {
                let c = g.component[x as usize] as usize;
                (g.nodes[x as usize], size[c], g.cyclic[c])
            })
            .collect(),
    )
}

/// The strongly connected components of a graph over the nodes `0..n`: each node's
/// component, and each component's nodes. Components are numbered sinks first (reverse
/// topological order), so a component's successors come before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Components {
    /// Per node, its component.
    pub component: Vec<u32>,
    /// `members[starts[c]..starts[c + 1]]` are component c's nodes.
    pub members: Vec<u32>,
    pub starts: Vec<usize>,
}

impl Components {
    /// The number of components.
    pub fn len(&self) -> usize {
        self.starts.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The strongly connected components of the graph whose node `v` has the successors
/// `targets[offsets[v]..offsets[v + 1]]` (so `offsets` has one entry more than the
/// graph has nodes): Tarjan's algorithm (1972), iterative, in O(n + m). `None` if `stop`
/// fired. The SCC kernel the transitive closures here (the reasoner's transitive module,
/// SPARQL's property paths) and the reasoner's recursion analysis share (G7 of the
/// investigation of 6 October 2026).
pub fn components_csr(
    offsets: &[u32],
    targets: &[u32],
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Components> {
    const UNSEEN: u32 = u32::MAX;
    let n = offsets.len() - 1;
    let successors =
        |v: u32| &targets[offsets[v as usize] as usize..offsets[v as usize + 1] as usize];
    let mut out = Components {
        component: vec![0; n],
        members: Vec::with_capacity(n),
        starts: vec![0],
    };
    let mut order = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    // The depth-first path: each node with the index of its next successor.
    let mut calls: Vec<(u32, usize)> = Vec::new();
    let mut counter = 0u32;
    let mut visit = |v: u32,
                     order: &mut [u32],
                     low: &mut [u32],
                     on_stack: &mut [bool],
                     stack: &mut Vec<u32>,
                     calls: &mut Vec<(u32, usize)>| {
        order[v as usize] = counter;
        low[v as usize] = counter;
        counter += 1;
        stack.push(v);
        on_stack[v as usize] = true;
        calls.push((v, 0));
    };
    for root in 0..n as u32 {
        if root % 4096 == 0 && stop() {
            return None;
        }
        if order[root as usize] != UNSEEN {
            continue;
        }
        visit(
            root,
            &mut order,
            &mut low,
            &mut on_stack,
            &mut stack,
            &mut calls,
        );
        while let Some(&(v, next)) = calls.last() {
            if let Some(&w) = successors(v).get(next) {
                calls.last_mut().expect("not empty").1 += 1;
                if order[w as usize] == UNSEEN {
                    visit(
                        w,
                        &mut order,
                        &mut low,
                        &mut on_stack,
                        &mut stack,
                        &mut calls,
                    );
                } else if on_stack[w as usize] {
                    low[v as usize] = low[v as usize].min(order[w as usize]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(u, _)) = calls.last() {
                low[u as usize] = low[u as usize].min(low[v as usize]);
            }
            if low[v as usize] == order[v as usize] {
                let c = out.starts.len() as u32 - 1;
                loop {
                    let w = stack.pop().expect("v is on the stack");
                    on_stack[w as usize] = false;
                    out.component[w as usize] = c;
                    out.members.push(w);
                    if w == v {
                        break;
                    }
                }
                out.starts.push(out.members.len());
            }
        }
    }
    Some(out)
}

/// [`components_csr`] of the graph over the nodes `0..n` with `edges`.
pub fn components(n: usize, edges: &[(u32, u32)]) -> Components {
    let mut offsets = vec![0u32; n + 1];
    for &(a, _) in edges {
        offsets[a as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let mut targets = vec![0u32; edges.len()];
    let mut at: Vec<u32> = offsets[..n].to_vec();
    for &(a, b) in edges {
        targets[at[a as usize] as usize] = b;
        at[a as usize] += 1;
    }
    components_csr(&offsets, &targets, &|| false).expect("never stopped")
}

/// A relation's strongly connected components and, per component, the components it
/// reaches.
///
/// Nodes in one strongly connected component reach the same set, so reachability is
/// computed per component (Nuutila's approach). Tarjan's algorithm finds the components
/// in reverse topological order, so each component's reach is the union of its
/// successors' reach, computed after theirs. The cost is O(n + m) plus the total size of
/// the components' reach sets.
struct Condensation {
    /// The relation's nodes, sorted: dense node `x` is `nodes[x]`.
    nodes: Vec<u64>,
    offsets: Vec<u32>,
    targets: Vec<u32>,
    /// Per dense node, its component.
    component: Vec<u32>,
    /// `members[starts[c]..starts[c + 1]]` are component c's nodes.
    members: Vec<u32>,
    starts: Vec<usize>,
    /// Per component: whether it reaches itself (more than one node, or a self-loop).
    cyclic: Vec<bool>,
    /// Per component, the components it reaches in one or more steps (itself if cyclic).
    reach: Vec<Vec<u32>>,
}

impl Condensation {
    fn of(edges: &[(u64, u64)], stop: &(dyn Fn() -> bool + Sync)) -> Option<Self> {
        use rayon::prelude::*;

        // Dense node numbering and CSR over it.
        let mut nodes: Vec<u64> = edges.iter().flat_map(|&(a, b)| [a, b]).collect();
        nodes.par_sort_unstable();
        nodes.dedup();
        let n = nodes.len();
        let dense = |id: u64| nodes.binary_search(&id).expect("every endpoint is a node") as u32;
        let mut pairs: Vec<(u32, u32)> = edges
            .par_iter()
            .map(|&(a, b)| (dense(a), dense(b)))
            .collect();
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
        let Components {
            component,
            members,
            starts,
        } = components_csr(&offsets, &targets, stop)?;
        let mut g = Self {
            nodes,
            offsets,
            targets,
            component,
            members,
            starts,
            cyclic: Vec::new(),
            reach: Vec::new(),
        };
        g.reach(stop)?;
        Some(g)
    }

    fn successors(&self, v: u32) -> &[u32] {
        &self.targets[self.offsets[v as usize] as usize..self.offsets[v as usize + 1] as usize]
    }

    fn has_successors(&self, v: u32) -> bool {
        self.offsets[v as usize] < self.offsets[v as usize + 1]
    }

    fn components(&self) -> usize {
        self.starts.len() - 1
    }

    /// Component `c`'s nodes; none for an index past the last component.
    fn members_of(&self, c: usize) -> &[u32] {
        match self.starts.get(c..c.saturating_add(2)) {
            Some(&[start, end]) => &self.members[start..end],
            _ => &[],
        }
    }

    /// Reach per component, successors first.
    fn reach(&mut self, stop: &(dyn Fn() -> bool + Sync)) -> Option<()> {
        let components = self.components();
        let mut stamp = vec![u32::MAX; components];
        for c in 0..components {
            if c % 1024 == 0 && stop() {
                return None;
            }
            let mut set = Vec::new();
            let mut cyclic = self.members_of(c).len() > 1;
            for &x in self.members_of(c) {
                for &y in self.successors(x) {
                    let d = self.component[y as usize];
                    if d as usize == c {
                        cyclic = true;
                        continue;
                    }
                    if stamp[d as usize] != c as u32 {
                        stamp[d as usize] = c as u32;
                        set.push(d);
                        for &e in &self.reach[d as usize] {
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
            self.cyclic.push(cyclic);
            self.reach.push(set);
        }
        Some(())
    }
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
            let mut expected: Vec<(u64, u64)> =
                closure(&adjacency, adjacency.sources().to_vec(), false)
                    .rows()
                    .map(|r| (r[0], r[1]))
                    .collect();
            expected.sort_unstable();
            let mut got = transitive_closure(&edges);
            got.sort_unstable();
            assert_eq!(got, expected, "{edges:?}");
            let sizes: Vec<(u64, u64, bool)> = adjacency
                .sources()
                .iter()
                .map(|&s| {
                    let reached = expected.iter().filter(|&&(a, _)| a == s);
                    (
                        s,
                        reached.clone().count() as u64,
                        reached.into_iter().any(|&(_, b)| b == s),
                    )
                })
                .collect();
            assert_eq!(
                closure_sizes_until(&edges, &|| false).expect("never stopped"),
                sizes,
                "{edges:?}"
            );
        }
        // A clique: every pair, including each node with itself.
        let clique: Vec<(u64, u64)> = (0..50)
            .flat_map(|a| [(a, (a + 1) % 50), ((a + 1) % 50, a)])
            .collect();
        assert_eq!(transitive_closure(&clique).len(), 50 * 50);
        assert!(transitive_closure_until(&clique, &|| true).is_none());
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

    /// The SCC kernel: components, each once, sinks first.
    #[test]
    fn components_come_sinks_first() {
        // 0 -> 1 <-> 2 -> 3 <-> 4, 5 alone, 6 -> 6.
        let edges = [(0, 1), (1, 2), (2, 1), (2, 3), (3, 4), (4, 3), (6, 6)];
        let found = components(7, &edges);
        assert_eq!(found.len(), 5);
        let c = &found.component;
        assert_eq!((c[1], c[3]), (c[2], c[4]));
        assert!(c[3] < c[1] && c[1] < c[0], "{c:?}");
        let mut members: Vec<u32> = found.members.clone();
        members.sort_unstable();
        assert_eq!(members, (0..7).collect::<Vec<_>>());
        assert_eq!(components(0, &[]).len(), 0);
    }
}
