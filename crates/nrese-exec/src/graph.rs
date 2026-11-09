//! Graph algorithms over id edge relations: reachability for SPARQL property paths
//! (`p+`, `p*`) and, in the reasoner, for hierarchies and transitive properties.
//!
//! [`Adjacency`] is a compressed sparse row index over `(source, target)` edges: sorted
//! targets per source, found by binary search over the distinct sources. [`reachable`]
//! walks it breadth-first with a visited set, so each node is expanded once: set semantics,
//! as SPARQL requires for `*` and `+`, and cycles terminate.
//!
//! The closures ([`transitive_closure_within`], [`closure_sizes_within`], [`closure_within`])
//! draw every buffer from the caller's [`Pool`]: their output, quadratic in the relation's
//! size where it is one strongly connected component (a 1,024-node cycle: 1,048,576 pairs),
//! and their reach sets, quadratic in the components where they form a chain, are charged
//! before they are allocated, and freed before they are released (the code-quality triage's
//! S-03; the executor memory stack's fix check r4, R4-F1). They poll their stop once per
//! [`crate::POLL_UNITS`] units of work per task, inside one component too ([`Ticker`]).

use std::ops::Deref;

use hashbrown::HashSet;

use crate::stop::{Stop, Ticker};
use crate::table::IdTable;
use crate::{Budget, BudgetExceeded, Charged, ChargedMap, Pool, Share};

type Hasher = foldhash::fast::FixedState;

/// A buffer of a closure, charged to the caller's pool.
pub type Buf<'p, T, B> = Charged<T, Share<'p, B>>;

/// A caller's stop condition: true to stop.
type StopFn<'s> = &'s (dyn Fn() -> bool + Sync);

/// Units of work for a [`Ticker`]: a count past `u32` is one full poll interval.
fn units(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

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
/// Uncharged and never stopped ([`closure_within`] on a pool of its own).
pub fn closure(
    adjacency: &Adjacency,
    starts: impl IntoIterator<Item = u64>,
    reflexive: bool,
) -> IdTable {
    let budget = Budget::unlimited();
    let pool = Pool::new(&budget);
    let pairs = closure_within(adjacency, starts, reflexive, &pool, &|| false)
        .expect("an unlimited closure never stopped");
    let mut out = IdTable::with_capacity(2, pairs.len());
    for &(start, end) in pairs.iter() {
        out.push_row(&[start, end]);
    }
    out
}

/// [`closure`]'s pairs, from each start in breadth-first order, every buffer charged to
/// `pool` before it grows (the pairs, the frontiers, the nodes seen) and `stop` polled once
/// per [`crate::POLL_UNITS`] edges walked: a closure from more starts than a pattern binds
/// can be quadratic too (4,097 starts into a 1,024-node cycle: 4.2 million pairs).
pub fn closure_within<'p, B: Deref<Target = Budget>>(
    adjacency: &Adjacency,
    starts: impl IntoIterator<Item = u64>,
    reflexive: bool,
    pool: &'p Pool<B>,
    stop: StopFn<'_>,
) -> Result<Buf<'p, (u64, u64), B>, Stop> {
    let mut ticker = Ticker::new(stop)?;
    let mut out = Charged::in_pool(pool);
    // Per node, the last start it was reached from: a stamp, so nothing is cleared between
    // starts.
    let mut seen: ChargedMap<u64, usize, Share<'p, B>> = ChargedMap::in_pool(pool);
    let mark = |seen: &mut ChargedMap<u64, usize, Share<'p, B>>, node: u64, start: usize| {
        let (at, new) = seen.get_or_insert_with(node, || start)?;
        let first = new || *at != start;
        *at = start;
        Ok::<_, BudgetExceeded>(first)
    };
    let mut frontier: Buf<'p, u64, B> = Charged::in_pool(pool);
    let mut next: Buf<'p, u64, B> = Charged::in_pool(pool);
    for (i, start) in starts.into_iter().enumerate() {
        if reflexive {
            mark(&mut seen, start, i)?;
            out.push((start, start))?;
        }
        frontier.push(start)?;
        while !frontier.is_empty() {
            for k in 0..frontier.len() {
                for &target in adjacency.neighbours(frontier[k]) {
                    ticker.tick(1)?;
                    if mark(&mut seen, target, i)? {
                        out.push((start, target))?;
                        next.push(target)?;
                    }
                }
            }
            std::mem::swap(&mut frontier, &mut next);
            next.clear();
        }
    }
    Ok(out)
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

/// [`transitive_closure`], polling `stop` while it works, inside one component too; `None`
/// if it fired. The output of a large relation can be quadratic in its size, so this is what
/// makes a newly declared transitive property's closure cancellable. Uncharged
/// ([`transitive_closure_within`] on a pool of its own): the reasoner's materialization
/// budget isn't passed to it.
pub fn transitive_closure_until(
    edges: &[(u64, u64)],
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<(u64, u64)>> {
    let budget = Budget::unlimited();
    let pool = Pool::new(&budget);
    let pairs = transitive_closure_within(edges, &pool, stop).ok()?;
    Some(pairs.into_parts().0)
}

/// [`transitive_closure`] with every buffer charged to `pool` before it is allocated, its
/// output's exact size before any pair is written, and `stop` polled once per
/// [`crate::POLL_UNITS`] units of work per task: edges walked, reach entries, pairs
/// written. [`Stop::Memory`] where a charge was refused, [`Stop::Cancelled`] where `stop`
/// fired; then everything it made is freed, then released.
pub fn transitive_closure_within<'p, B: Deref<Target = Budget>>(
    edges: &[(u64, u64)],
    pool: &'p Pool<B>,
    stop: StopFn<'_>,
) -> Result<Buf<'p, (u64, u64), B>, Stop> {
    let mut ticker = Ticker::new(stop)?;
    let Condensation {
        nodes,
        offsets,
        targets,
        component,
        members,
        starts,
        cyclic,
        reach,
        reach_starts,
    } = Condensation::of(edges, pool, &mut ticker)?;
    // What only the condensation needed, freed before the output is charged.
    drop((offsets, targets, component, cyclic));
    let (nodes, members, starts): (&[u64], &[u32], &[usize]) = (&nodes, &members, &starts);
    let (reach, reach_starts): (&[u32], &[usize]) = (&reach, &reach_starts);
    let components = starts.len() - 1;
    let members_of = |c: usize| &members[starts[c]..starts[c + 1]];
    let reach_of = |c: usize| &reach[reach_starts[c]..reach_starts[c + 1]];
    // Component `c`'s pairs: its members times the members of the components it reaches.
    let pairs_of = |c: usize| {
        let row: usize = reach_of(c)
            .iter()
            .map(|&d| members_of(d as usize).len())
            .sum();
        members_of(c).len().checked_mul(row)
    };
    let mut total = Some(0usize);
    for c in 0..components {
        ticker.tick(units(reach_of(c).len()))?;
        total = total
            .zip(pairs_of(c))
            .and_then(|(total, own)| total.checked_add(own));
    }
    // Refused here, before a pair is written (a length past what a vector holds too).
    let mut out = Charged::with_capacity_in(pool, total.unwrap_or(usize::MAX))?;
    let total = total.unwrap_or(0);
    // Runs of whole components of about a quarter of a thread's share, written in parallel:
    // one component, however large, is one run, as it was one task.
    let target = (total / (4 * rayon::current_num_threads()))
        .max(crate::POLL_UNITS as usize)
        .max(1);
    let mut lengths: Buf<'p, usize, B> = Charged::in_pool(pool);
    let mut runs: Buf<'p, (usize, usize), B> = Charged::in_pool(pool);
    let (mut first, mut length) = (0, 0);
    for c in 0..components {
        length += pairs_of(c).unwrap_or(0);
        if length >= target || c + 1 == components {
            lengths.push(length)?;
            runs.push((first, c + 1))?;
            (first, length) = (c + 1, 0);
        }
    }
    let runs: &[(usize, usize)] = &runs;
    out.fill(0, |room| {
        room.try_par_fill_parts(&lengths, |i, part| {
            let mut ticker = Ticker::new(stop)?;
            let (first, end) = runs[i];
            for c in first..end {
                let reached = &reach[reach_starts[c]..reach_starts[c + 1]];
                for &x in members_of(c) {
                    for &d in reached {
                        for &y in members_of(d as usize) {
                            ticker.tick(1)?;
                            part.push((nodes[x as usize], nodes[y as usize]));
                        }
                    }
                }
            }
            Ok::<_, Stop>(())
        })
    })??;
    Ok(out)
}

/// For every node of `edges` with at least one edge out, sorted: the node, the number of
/// nodes it reaches in one or more steps, and whether it reaches itself (it lies on a
/// cycle). The size of the transitive closure without building it, which is what `COUNT`
/// over `p+` and `p*` needs. Cost as [`transitive_closure`] without the output. `None` if
/// `stop` fired. Uncharged ([`closure_sizes_within`] on a pool of its own).
pub fn closure_sizes_until(
    edges: &[(u64, u64)],
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<(u64, u64, bool)>> {
    let budget = Budget::unlimited();
    let pool = Pool::new(&budget);
    let sizes = closure_sizes_within(edges, &pool, stop).ok()?;
    Some(sizes.into_parts().0)
}

/// [`closure_sizes_until`] with every buffer charged to `pool` before it is allocated and
/// `stop` polled as [`transitive_closure_within`] polls it.
pub fn closure_sizes_within<'p, B: Deref<Target = Budget>>(
    edges: &[(u64, u64)],
    pool: &'p Pool<B>,
    stop: StopFn<'_>,
) -> Result<Buf<'p, (u64, u64, bool), B>, Stop> {
    let mut ticker = Ticker::new(stop)?;
    let g = Condensation::of(edges, pool, &mut ticker)?;
    let mut size: Buf<'p, u64, B> = Charged::with_capacity_in(pool, g.components())?;
    for c in 0..g.components() {
        let reached = g.reach_of(c);
        ticker.tick(units(reached.len()))?;
        size.push(
            reached
                .iter()
                .map(|&d| g.members_of(d as usize).len() as u64)
                .sum(),
        )?;
    }
    // Dense ids follow the sorted node ids, so the output comes out sorted.
    let mut out = Charged::in_pool(pool);
    for x in 0..g.nodes.len() as u32 {
        if g.has_successors(x) {
            let c = g.component[x as usize] as usize;
            out.push((g.nodes[x as usize], size[c], g.cyclic[c]))?;
        }
    }
    Ok(out)
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

/// [`Components`] in buffers charged to a pool.
struct ComponentsIn<'p, B: Deref<Target = Budget>> {
    component: Buf<'p, u32, B>,
    members: Buf<'p, u32, B>,
    starts: Buf<'p, usize, B>,
}

/// The strongly connected components of the graph whose node `v` has the successors
/// `targets[offsets[v]..offsets[v + 1]]` (so `offsets` has one entry more than the
/// graph has nodes): Tarjan's algorithm (1972), iterative, in O(n + m). `None` if `stop`
/// fired. The SCC kernel the transitive closures here (the reasoner's transitive module,
/// SPARQL's property paths) and the reasoner's recursion analysis share (G7 of the
/// investigation of 6 October 2026). Uncharged ([`components_csr_in`] on a pool of its
/// own).
pub fn components_csr(
    offsets: &[u32],
    targets: &[u32],
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Components> {
    let budget = Budget::unlimited();
    let pool = Pool::new(&budget);
    let mut ticker = Ticker::new(stop).ok()?;
    let found = components_csr_in(offsets, targets, &pool, &mut ticker).ok()?;
    Some(Components {
        component: found.component.into_parts().0,
        members: found.members.into_parts().0,
        starts: found.starts.into_parts().0,
    })
}

/// [`components_csr`] with its buffers charged to `pool` before they grow, polling once per
/// root and once per edge walked.
fn components_csr_in<'p, B: Deref<Target = Budget>>(
    offsets: &[u32],
    targets: &[u32],
    pool: &'p Pool<B>,
    ticker: &mut Ticker<StopFn<'_>>,
) -> Result<ComponentsIn<'p, B>, Stop> {
    const UNSEEN: u32 = u32::MAX;
    let n = offsets.len() - 1;
    let successors =
        |v: u32| &targets[offsets[v as usize] as usize..offsets[v as usize + 1] as usize];
    let mut out = ComponentsIn {
        component: Charged::in_pool(pool),
        members: Charged::with_capacity_in(pool, n)?,
        starts: Charged::in_pool(pool),
    };
    out.component.resize(n, 0)?;
    out.starts.push(0)?;
    let mut order: Buf<'p, u32, B> = Charged::in_pool(pool);
    order.resize(n, UNSEEN)?;
    let mut low: Buf<'p, u32, B> = Charged::in_pool(pool);
    low.resize(n, 0)?;
    let mut on_stack: Buf<'p, bool, B> = Charged::in_pool(pool);
    on_stack.resize(n, false)?;
    let mut stack: Buf<'p, u32, B> = Charged::in_pool(pool);
    // The depth-first path: each node with the index of its next successor.
    let mut calls: Buf<'p, (u32, usize), B> = Charged::in_pool(pool);
    let mut counter = 0u32;
    let mut visit = |v: u32,
                     order: &mut [u32],
                     low: &mut [u32],
                     on_stack: &mut [bool],
                     stack: &mut Buf<'p, u32, B>,
                     calls: &mut Buf<'p, (u32, usize), B>| {
        order[v as usize] = counter;
        low[v as usize] = counter;
        counter += 1;
        stack.push(v)?;
        on_stack[v as usize] = true;
        calls.push((v, 0))
    };
    for root in 0..n as u32 {
        ticker.tick(1)?;
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
        )?;
        while let Some(&(v, next)) = calls.last() {
            if let Some(&w) = successors(v).get(next) {
                ticker.tick(1)?;
                let top = calls.len() - 1;
                calls[top].1 += 1;
                if order[w as usize] == UNSEEN {
                    visit(
                        w,
                        &mut order,
                        &mut low,
                        &mut on_stack,
                        &mut stack,
                        &mut calls,
                    )?;
                } else if on_stack[w as usize] {
                    low[v as usize] = low[v as usize].min(order[w as usize]);
                }
                continue;
            }
            calls.truncate(calls.len() - 1);
            if let Some(&(u, _)) = calls.last() {
                low[u as usize] = low[u as usize].min(low[v as usize]);
            }
            if low[v as usize] == order[v as usize] {
                let c = out.starts.len() as u32 - 1;
                loop {
                    let w = *stack.last().expect("v is on the stack");
                    stack.truncate(stack.len() - 1);
                    on_stack[w as usize] = false;
                    out.component[w as usize] = c;
                    out.members.push(w)?;
                    if w == v {
                        break;
                    }
                }
                out.starts.push(out.members.len())?;
            }
        }
    }
    Ok(out)
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

/// `values`, sorted, with each value once.
fn dedup_sorted<T: PartialEq + Copy, B: Deref<Target = Budget>>(values: &mut Buf<'_, T, B>) {
    let mut previous = None;
    values.retain(|&value| {
        let first = previous != Some(value);
        previous = Some(value);
        first
    });
}

/// A relation's strongly connected components and, per component, the components it
/// reaches, every buffer charged to a pool.
///
/// Nodes in one strongly connected component reach the same set, so reachability is
/// computed per component (Nuutila's approach). Tarjan's algorithm finds the components
/// in reverse topological order, so each component's reach is the union of its
/// successors' reach, computed after theirs. The cost is O(n + m) plus the total size of
/// the components' reach sets.
struct Condensation<'p, B: Deref<Target = Budget>> {
    /// The relation's nodes, sorted: dense node `x` is `nodes[x]`.
    nodes: Buf<'p, u64, B>,
    offsets: Buf<'p, u32, B>,
    targets: Buf<'p, u32, B>,
    /// Per dense node, its component.
    component: Buf<'p, u32, B>,
    /// `members[starts[c]..starts[c + 1]]` are component c's nodes.
    members: Buf<'p, u32, B>,
    starts: Buf<'p, usize, B>,
    /// Per component: whether it reaches itself (more than one node, or a self-loop).
    cyclic: Buf<'p, bool, B>,
    /// `reach[reach_starts[c]..reach_starts[c + 1]]` are the components component c reaches
    /// in one or more steps (itself if cyclic): quadratic in the components on a chain.
    reach: Buf<'p, u32, B>,
    reach_starts: Buf<'p, usize, B>,
}

impl<'p, B: Deref<Target = Budget>> Condensation<'p, B> {
    fn of(
        edges: &[(u64, u64)],
        pool: &'p Pool<B>,
        ticker: &mut Ticker<StopFn<'_>>,
    ) -> Result<Self, Stop> {
        use rayon::prelude::*;

        // Dense node numbering and CSR over it.
        let mut nodes: Buf<'p, u64, B> =
            Charged::with_capacity_in(pool, edges.len().saturating_mul(2))?;
        nodes.fill(0, |room| {
            for &(a, b) in edges {
                room.push(a);
                room.push(b);
            }
        })?;
        nodes.par_sort_unstable();
        dedup_sorted(&mut nodes);
        // Room for both ends of every edge, of which the distinct nodes are often far fewer.
        nodes.shrink_to_fit()?;
        let n = nodes.len();
        let sorted: &[u64] = &nodes;
        let dense = |id: u64| sorted.binary_search(&id).expect("every endpoint is a node") as u32;
        let mut pairs: Buf<'p, (u32, u32), B> = Charged::with_capacity_in(pool, edges.len())?;
        pairs.fill(0, |room| {
            room.par_fill(edges.len(), |i| (dense(edges[i].0), dense(edges[i].1)))
        })?;
        pairs.par_sort_unstable();
        dedup_sorted(&mut pairs);
        let mut offsets: Buf<'p, u32, B> = Charged::in_pool(pool);
        offsets.resize(n + 1, 0)?;
        for &(a, _) in pairs.iter() {
            offsets[a as usize + 1] += 1;
        }
        for i in 0..n {
            offsets[i + 1] += offsets[i];
        }
        let mut targets: Buf<'p, u32, B> = Charged::with_capacity_in(pool, pairs.len())?;
        targets.fill(0, |room| room.extend(pairs.iter().map(|&(_, b)| b)))?;
        drop(pairs);
        let ComponentsIn {
            component,
            members,
            starts,
        } = components_csr_in(&offsets, &targets, pool, ticker)?;
        let mut g = Self {
            nodes,
            offsets,
            targets,
            component,
            members,
            starts,
            cyclic: Charged::in_pool(pool),
            reach: Charged::in_pool(pool),
            reach_starts: Charged::in_pool(pool),
        };
        g.reach(pool, ticker)?;
        Ok(g)
    }

    fn has_successors(&self, v: u32) -> bool {
        self.offsets[v as usize] < self.offsets[v as usize + 1]
    }

    fn components(&self) -> usize {
        self.starts.len() - 1
    }

    /// Component `c`'s nodes.
    fn members_of(&self, c: usize) -> &[u32] {
        &self.members[self.starts[c]..self.starts[c + 1]]
    }

    /// The components component `c` reaches.
    fn reach_of(&self, c: usize) -> &[u32] {
        &self.reach[self.reach_starts[c]..self.reach_starts[c + 1]]
    }

    /// Reach per component, successors first, each set's growth charged before it grows,
    /// a tick per successor walked and per entry taken over.
    fn reach(&mut self, pool: &'p Pool<B>, ticker: &mut Ticker<StopFn<'_>>) -> Result<(), Stop> {
        let components = self.components();
        let mut stamp: Buf<'p, u32, B> = Charged::in_pool(pool);
        stamp.resize(components, u32::MAX)?;
        self.cyclic.reserve_exact(components)?;
        self.reach_starts.reserve_exact(components + 1)?;
        self.reach_starts.push(0)?;
        for c in 0..components {
            let mut cyclic = self.members_of(c).len() > 1;
            for k in self.starts[c]..self.starts[c + 1] {
                let x = self.members[k] as usize;
                for j in self.offsets[x] as usize..self.offsets[x + 1] as usize {
                    ticker.tick(1)?;
                    let d = self.component[self.targets[j] as usize];
                    if d as usize == c {
                        cyclic = true;
                        continue;
                    }
                    if stamp[d as usize] != c as u32 {
                        stamp[d as usize] = c as u32;
                        self.reach.push(d)?;
                        let (from, to) = (
                            self.reach_starts[d as usize],
                            self.reach_starts[d as usize + 1],
                        );
                        for i in from..to {
                            ticker.tick(1)?;
                            let e = self.reach[i];
                            if stamp[e as usize] != c as u32 {
                                stamp[e as usize] = c as u32;
                                self.reach.push(e)?;
                            }
                        }
                    }
                }
            }
            if cyclic {
                self.reach.push(c as u32)?;
            }
            self.cyclic.push(cyclic)?;
            self.reach_starts.push(self.reach.len())?;
        }
        Ok(())
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

    use std::sync::atomic::Ordering;

    /// A cycle of `n` nodes: one strongly connected component whose closure is all n² pairs.
    fn cycle(n: u64) -> Vec<(u64, u64)> {
        (0..n).map(|i| (i, (i + 1) % n)).collect()
    }

    /// Spec (stop.rs, W1: interruptible work reaches a checkpoint within `POLL_UNITS` units
    /// per executing task; the code-quality triage's S-03): the closure of one strongly
    /// connected component polls its stop while it writes the component's pairs. A
    /// 1,024-node cycle has 1,048,576 pairs: a stop that never fires is asked at least once
    /// per 4,096 of them.
    #[test]
    fn a_closure_polls_its_stop_inside_one_component() {
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let stop = || {
            polls.fetch_add(1, Ordering::Relaxed);
            false
        };
        let pairs = transitive_closure_until(&cycle(1024), &stop).expect("never stopped");
        assert_eq!(pairs.len(), 1 << 20);
        let polls = polls.load(Ordering::Relaxed);
        assert!(
            polls >= (1 << 20) / crate::POLL_UNITS as usize,
            "polled {polls} times for 1,048,576 pairs"
        );
    }

    /// Spec (as above): a stop that fires while one component's pairs are written stops the
    /// closure there: at its 16th poll, of the 256 the 1,024-cycle's pairs take at least.
    #[test]
    fn a_closure_stops_inside_one_component() {
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let stop = || polls.fetch_add(1, Ordering::Relaxed) >= 16;
        assert!(transitive_closure_until(&cycle(1024), &stop).is_none());
    }

    /// The charged kernels answer what the uncharged ones answer, and charge exactly what
    /// they hold: the pool's budget holds the output's bytes while it lives, and nothing once
    /// it is dropped; under a limit below the output they are refused, holding nothing after.
    #[test]
    fn the_charged_closures_answer_the_same_and_hold_their_charge() {
        let edges: Vec<(u64, u64)> = cycle(64).into_iter().chain([(70, 71), (71, 72)]).collect();
        let budget = Budget::unlimited();
        let pool = Pool::new(&budget);
        let mut charged = transitive_closure_within(&edges, &pool, &|| false)
            .unwrap()
            .to_vec();
        let mut plain = transitive_closure(&edges);
        charged.sort_unstable();
        plain.sort_unstable();
        assert_eq!(charged, plain);
        let sizes = closure_sizes_within(&edges, &pool, &|| false)
            .unwrap()
            .to_vec();
        assert_eq!(sizes, closure_sizes_until(&edges, &|| false).unwrap());
        let adjacency = Adjacency::new(edges.clone());
        let from = closure_within(&adjacency, [0, 70], true, &pool, &|| false).unwrap();
        assert_eq!(from.len(), 64 + 3);
        drop(from);
        drop(pool);
        assert_eq!(budget.used(), 0);
        let small = Budget::new(4096);
        let pool = Pool::new(&small);
        let refused = transitive_closure_within(&cycle(64), &pool, &|| false);
        assert!(matches!(refused, Err(Stop::Memory(_))), "{refused:?}");
        drop(refused);
        drop(pool);
        assert_eq!(small.used(), 0);
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
