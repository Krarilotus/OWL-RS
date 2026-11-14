//! Graph algorithms over id edge relations: reachability for SPARQL property paths
//! (`p+`, `p*`) and, in the reasoner, for hierarchies and transitive properties.
//!
//! [`Adjacency`] is a compressed sparse row index over `(source, target)` edges: sorted
//! targets per source, found by binary search over the distinct sources.
//! [`reachable_within`] and [`closure_within`] walk it, or a bound start's index probes
//! ([`Successors`]), breadth-first with a set of the nodes seen, so each node is expanded once:
//! set semantics, as SPARQL requires for `*` and `+`, and cycles terminate.
//!
//! The searches and closures ([`reachable_within`], [`closure_within`],
//! [`transitive_closure_within`], [`closure_sizes_within`]) draw every buffer from the caller's
//! [`Pool`]: a search's nodes seen and order, as long as a chain it walks; a closure's
//! output, quadratic in the relation's size where it is one strongly connected component (a
//! 1,024-node cycle: 1,048,576 pairs),
//! and their reach sets, quadratic in the components where they form a chain, are charged
//! before they are allocated, and freed before they are released (the code-quality triage's
//! S-03; the executor memory stack's fix check r4, R4-F1). They poll their stop once per
//! [`crate::POLL_UNITS`] units of work per task, inside one component too ([`Ticker`]).

use std::ops::Deref;

use crate::stop::{Stop, Ticker};
use crate::table::IdTable;
use crate::{Budget, BudgetExceeded, Charged, ChargedMap, Pool, Share};

/// A buffer of a closure, charged to the caller's pool.
pub type Buf<'p, T, B> = Charged<T, Share<'p, B>>;

/// A caller's stop condition: true to stop.
type StopFn<'s> = &'s (dyn Fn() -> bool + Sync);

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

/// A node's successors, as a search walks them: an [`Adjacency`]'s, or index probes for a
/// bound start ([`Probes`]).
pub trait Successors {
    /// Calls `visit` once with the successors of `node` (a successor may come more than once),
    /// and returns what it returns.
    fn each(
        &mut self,
        node: u64,
        visit: impl FnOnce(&[u64]) -> Result<(), Stop>,
    ) -> Result<(), Stop>;
}

impl Successors for &Adjacency {
    fn each(
        &mut self,
        node: u64,
        visit: impl FnOnce(&[u64]) -> Result<(), Stop>,
    ) -> Result<(), Stop> {
        visit(self.neighbours(node))
    }
}

/// Successors found by a function of the node: `Ok` with them (a buffer the caller charged),
/// or the stop that ends the search.
pub struct Probes<F>(pub F);

impl<F, S> Successors for Probes<F>
where
    F: FnMut(u64) -> Result<S, Stop>,
    S: Deref<Target = [u64]>,
{
    fn each(
        &mut self,
        node: u64,
        visit: impl FnOnce(&[u64]) -> Result<(), Stop>,
    ) -> Result<(), Stop> {
        visit(&(self.0)(node)?)
    }
}

/// Nodes reachable from `start` in one or more steps (`include_start == false`) or zero or
/// more (`include_start == true`), each once, in breadth-first order. Every buffer that grows
/// with the nodes reached (the nodes seen, the order, the frontiers) is charged to `pool`
/// before it grows, and `stop` polled once per [`crate::POLL_UNITS`] nodes expanded and
/// successors walked: a bound start's closure over a long chain is as long as the chain
/// (review r1 of the graph closure package, R1-F1). [`Stop::Memory`] where a charge was
/// refused, [`Stop::Cancelled`] where `stop` fired, or what `successors` returned; then
/// everything it made is freed.
pub fn reachable_within<'p, B: Deref<Target = Budget>>(
    start: u64,
    include_start: bool,
    successors: impl Successors,
    pool: &'p Pool<B>,
    stop: StopFn<'_>,
) -> Result<Buf<'p, u64, B>, Stop> {
    let ticker = Ticker::new(stop)?;
    let mut order = Charged::in_pool(pool);
    // One start: the nodes seen are a set, with nothing to stamp.
    let stamp = |_| ();
    search(
        [start],
        include_start,
        successors,
        pool,
        &ticker,
        stamp,
        |_, nodes| order.extend_from_slice(nodes),
    )?;
    ticker.finish()?;
    Ok(order)
}

/// A breadth-first search from each of `starts` over `successors`, `found(start, nodes)` with
/// the nodes reached from it in one or more steps, or zero or more with `reflexive`, each once
/// per start. The nodes seen map to the `stamp(i)` of the `i`-th start that last reached them,
/// so one map serves every start without clearing (a single start's stamp is `()`: a set).
/// Successors are taken in batches of at most [`crate::POLL_UNITS`]: per batch, one tick
/// counting each, and the nodes new in it appended to the next frontier with one charge, then
/// passed to `found` at once. The nodes seen are charged before they grow.
fn search<'p, B: Deref<Target = Budget>, V: Copy + PartialEq>(
    starts: impl IntoIterator<Item = u64>,
    reflexive: bool,
    mut successors: impl Successors,
    pool: &'p Pool<B>,
    ticker: &Ticker<StopFn<'_>>,
    stamp: impl Fn(usize) -> V,
    mut found: impl FnMut(u64, &[u64]) -> Result<(), BudgetExceeded>,
) -> Result<(), Stop> {
    let mut seen: ChargedMap<u64, V, Share<'p, B>> = ChargedMap::in_pool(pool);
    let mut frontier: Buf<'p, u64, B> = Charged::in_pool(pool);
    let mut next: Buf<'p, u64, B> = Charged::in_pool(pool);
    for (i, start) in starts.into_iter().enumerate() {
        let own = stamp(i);
        if reflexive {
            seen.insert(start, own)?;
            found(start, &[start])?;
        }
        frontier.push(start)?;
        while !frontier.is_empty() {
            for k in 0..frontier.len() {
                ticker.tick(1)?;
                successors.each(frontier[k], |targets| {
                    for batch in targets.chunks(crate::POLL_UNITS as usize) {
                        ticker.tick(batch.len() as u32)?;
                        let (from, mut refused) = (next.len(), None);
                        let new = batch.iter().copied().filter(|&target| {
                            refused.is_none()
                                && match seen.insert(target, own) {
                                    Ok(old) => old != Some(own),
                                    Err(e) => {
                                        refused = Some(e);
                                        false
                                    }
                                }
                        });
                        next.extend_bounded(batch.len(), new)?;
                        if let Some(refused) = refused {
                            return Err(refused.into());
                        }
                        found(start, &next[from..])?;
                    }
                    Ok(())
                })?;
            }
            std::mem::swap(&mut frontier, &mut next);
            next.clear();
        }
    }
    Ok(())
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
    let ticker = Ticker::new(stop)?;
    let mut out = Charged::in_pool(pool);
    // Per node, the last start it was reached from: a stamp, so nothing is cleared between
    // starts.
    let stamp = |i: usize| i;
    search(
        starts,
        reflexive,
        adjacency,
        pool,
        &ticker,
        stamp,
        |start, ends| out.extend_bounded(ends.len(), ends.iter().map(|&end| (start, end))),
    )?;
    ticker.finish()?;
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
    let ticker = Ticker::new(stop)?;
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
        rows,
    } = Condensation::of(edges, pool, &ticker)?;
    // What only the condensation needed, freed before the output is charged.
    drop((offsets, targets, component, cyclic));
    let (nodes, members, starts): (&[u64], &[u32], &[usize]) = (&nodes, &members, &starts);
    let (reach, reach_starts): (&[u32], &[usize]) = (&reach, &reach_starts);
    let components = starts.len() - 1;
    let members_of = |c: usize| &members[starts[c]..starts[c + 1]];
    // Component `c`'s pairs: its members times the nodes it reaches (counted as its reach set
    // was made: sizing is one step a component, sol-loop r1 S1).
    let pairs_of = |c: usize| members_of(c).len().checked_mul(rows[c] as usize);
    let mut total = Some(0usize);
    for c in 0..components {
        ticker.tick(1)?;
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
        ticker.tick(1)?;
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
            let ticker = Ticker::new(stop)?;
            let (first, end) = runs[i];
            for c in first..end {
                ticker.tick(1)?;
                let reached = &reach[reach_starts[c]..reach_starts[c + 1]];
                for &x in members_of(c) {
                    ticker.tick(1)?;
                    for &d in reached {
                        for &y in members_of(d as usize) {
                            ticker.tick(1)?;
                            part.push((nodes[x as usize], nodes[y as usize]));
                        }
                    }
                }
            }
            ticker.finish()
        })
    })??;
    ticker.finish()?;
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
    let ticker = Ticker::new(stop)?;
    let g = Condensation::of(edges, pool, &ticker)?;
    // Dense ids follow the sorted node ids, so the output comes out sorted.
    let mut out = Charged::in_pool(pool);
    let size = &g.rows;
    for x in 0..g.nodes.len() as u32 {
        ticker.tick(1)?;
        if g.has_successors(x) {
            let c = g.component[x as usize] as usize;
            out.push((g.nodes[x as usize], u64::from(size[c]), g.cyclic[c]))?;
        }
    }
    ticker.finish()?;
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
    let ticker = Ticker::new(stop).ok()?;
    let found = components_csr_in(offsets, targets, &pool, &ticker).ok()?;
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
    ticker: &Ticker<StopFn<'_>>,
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
            ticker.tick(1)?;
            calls.truncate(calls.len() - 1);
            if let Some(&(u, _)) = calls.last() {
                low[u as usize] = low[u as usize].min(low[v as usize]);
            }
            if low[v as usize] == order[v as usize] {
                let c = out.starts.len() as u32 - 1;
                loop {
                    ticker.tick(1)?;
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

/// `work` over the indices `0..len` in runs, each as long as the ticker's allowance left
/// ([`Ticker::left`]) and ticked once after it: the tick then spends the allowance and polls,
/// so a stop is seen within [`crate::POLL_UNITS`] units of work, counted across passes, at one
/// tick a run (a tick an element cost the count-only closure +5.5 % instructions: the graph
/// closure package's counts; a run ticked before its work reset the allowance and let the
/// next run go unpolled: sol-loop r4 S5).
fn in_runs(
    ticker: &Ticker<StopFn<'_>>,
    len: usize,
    mut work: impl FnMut(std::ops::Range<usize>),
) -> Result<(), Stop> {
    let mut at = 0;
    while at < len {
        let end = at.saturating_add(ticker.left().max(1) as usize).min(len);
        work(at..end);
        ticker.tick((end - at) as u32)?;
        at = end;
    }
    Ok(())
}

/// `values`, sorted, with each value once, polled in runs ([`in_runs`]): a stop is seen within
/// [`crate::POLL_UNITS`] comparisons (sol-loop r2 of the graph closure package). Stopped, it
/// leaves `values` part-compacted: the kernels then drop them.
fn dedup_sorted<T: PartialEq + Copy, B: Deref<Target = Budget>>(
    values: &mut Buf<'_, T, B>,
    ticker: &Ticker<StopFn<'_>>,
) -> Result<(), Stop> {
    let mut kept = 0;
    in_runs(ticker, values.len(), |run| {
        for read in run {
            if kept == 0 || values[read] != values[kept - 1] {
                values[kept] = values[read];
                kept += 1;
            }
        }
    })?;
    values.truncate(kept);
    Ok(())
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
    /// Per component, the nodes it reaches: its row of the closure, counted as its reach set
    /// is made (a dense node count, within `u32`).
    rows: Buf<'p, u32, B>,
}

impl<'p, B: Deref<Target = Budget>> Condensation<'p, B> {
    fn of(
        edges: &[(u64, u64)],
        pool: &'p Pool<B>,
        ticker: &Ticker<StopFn<'_>>,
    ) -> Result<Self, Stop> {
        use rayon::prelude::*;

        // Dense node numbering and CSR over it.
        let mut nodes: Buf<'p, u64, B> =
            Charged::with_capacity_in(pool, edges.len().saturating_mul(2))?;
        // Sequential passes tick a unit an edge; the parallel sorts and the parallel
        // numbering are opaque calls, checked before and after (`Ticker::check`).
        nodes.fill(0, |room| {
            in_runs(ticker, edges.len(), |run| {
                for &(a, b) in &edges[run] {
                    room.push(a);
                    room.push(b);
                }
            })
        })??;
        ticker.check()?;
        nodes.par_sort_unstable();
        ticker.check()?;
        dedup_sorted(&mut nodes, ticker)?;
        // Room for both ends of every edge, of which the distinct nodes are often far fewer.
        nodes.shrink_to_fit()?;
        let n = nodes.len();
        let sorted: &[u64] = &nodes;
        let dense = |id: u64| sorted.binary_search(&id).expect("every endpoint is a node") as u32;
        let mut pairs: Buf<'p, (u32, u32), B> = Charged::with_capacity_in(pool, edges.len())?;
        ticker.check()?;
        pairs.fill(0, |room| {
            room.par_fill(edges.len(), |i| (dense(edges[i].0), dense(edges[i].1)))
        })?;
        ticker.check()?;
        pairs.par_sort_unstable();
        ticker.check()?;
        dedup_sorted(&mut pairs, ticker)?;
        let mut offsets: Buf<'p, u32, B> = Charged::in_pool(pool);
        offsets.resize(n + 1, 0)?;
        in_runs(ticker, pairs.len(), |run| {
            for &(a, _) in &pairs[run] {
                offsets[a as usize + 1] += 1;
            }
        })?;
        in_runs(ticker, n, |run| {
            for i in run {
                offsets[i + 1] += offsets[i];
            }
        })?;
        let mut targets: Buf<'p, u32, B> = Charged::with_capacity_in(pool, pairs.len())?;
        targets.fill(0, |room| {
            in_runs(ticker, pairs.len(), |run| {
                room.extend(pairs[run].iter().map(|&(_, b)| b))
            })
        })??;
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
            rows: Charged::in_pool(pool),
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

    /// Reach per component, successors first, each set's growth charged before it grows,
    /// a tick per successor walked and per run of entries taken over ([`in_runs`]). A set is
    /// made apart from `reach`, which it reads, and appended whole: with a tick and a charge
    /// check an entry, `reach` read while it grew, the reach sets of one OWL 2 RL
    /// rematerialisation of a 1,000-class spine took 4.1 G instructions, where the uncharged
    /// kernel took 1.7 G in all.
    fn reach(&mut self, pool: &'p Pool<B>, ticker: &Ticker<StopFn<'_>>) -> Result<(), Stop> {
        let components = self.components();
        let mut stamp: Buf<'p, u32, B> = Charged::in_pool(pool);
        stamp.resize(components, u32::MAX)?;
        let mut set: Buf<'p, u32, B> = Charged::in_pool(pool);
        self.cyclic.reserve_exact(components)?;
        self.reach_starts.reserve_exact(components + 1)?;
        self.rows.reserve_exact(components)?;
        self.reach_starts.push(0)?;
        let size = |starts: &[usize], d: u32| (starts[d as usize + 1] - starts[d as usize]) as u32;
        for c in 0..components {
            ticker.tick(1)?;
            let mut cyclic = self.members_of(c).len() > 1;
            let mut row = 0u32;
            for k in self.starts[c]..self.starts[c + 1] {
                ticker.tick(1)?;
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
                        let (starts, stamp) = (&self.starts, &mut stamp);
                        let taken = &self.reach
                            [self.reach_starts[d as usize]..self.reach_starts[d as usize + 1]];
                        // `d` and what it reaches: the room for all of them charged before
                        // the first is written.
                        set.fill(taken.len() + 1, |room| {
                            room.push(d);
                            row += size(starts, d);
                            in_runs(ticker, taken.len(), |run| {
                                for &e in &taken[run] {
                                    if stamp[e as usize] != c as u32 {
                                        stamp[e as usize] = c as u32;
                                        room.push(e);
                                        row += size(starts, e);
                                    }
                                }
                            })
                        })??;
                    }
                }
            }
            if cyclic {
                set.push(c as u32)?;
                row += size(&self.starts, c as u32);
            }
            self.reach.extend_from_slice(&set)?;
            set.clear();
            self.cyclic.push(cyclic)?;
            self.rows.push(row)?;
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

    /// Spec (stop.rs, W1; sol-loop r2 of the graph closure package): the kernels' dedup sees a
    /// stop within `POLL_UNITS` comparisons. Over 100,000 distinct values whose comparisons
    /// are counted, a stop that fires once 10,000 have been made stops it no more than
    /// `POLL_UNITS` comparisons later; scanned without a tick, it made all 99,999.
    #[test]
    fn a_dedup_stops_within_a_poll_interval() {
        static COMPARED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        #[derive(Clone, Copy)]
        struct Counted(u64);
        impl PartialEq for Counted {
            fn eq(&self, other: &Self) -> bool {
                COMPARED.fetch_add(1, Ordering::Relaxed);
                self.0 == other.0
            }
        }
        const AT: usize = 10_000;
        let budget = Budget::unlimited();
        let pool = Pool::new(&budget);
        let mut values: Buf<'_, Counted, &Budget> =
            Charged::with_capacity_in(&pool, 100_000).unwrap();
        for i in 0..100_000 {
            values.push(Counted(i)).unwrap();
        }
        let stop: StopFn<'_> = &|| COMPARED.load(Ordering::Relaxed) >= AT;
        let ticker = Ticker::new(stop).unwrap();
        assert_eq!(dedup_sorted(&mut values, &ticker), Err(Stop::Cancelled));
        let compared = COMPARED.load(Ordering::Relaxed);
        assert!(
            compared <= AT + crate::POLL_UNITS as usize,
            "{compared} comparisons, the stop at {AT}"
        );
    }

    /// Spec (stop.rs, W1; sol-loop r4 S5 of the graph closure package): linear passes in runs
    /// see a stop within `POLL_UNITS` units of work, counted across passes. A pass of 4,096
    /// units then one of 4,095, with a stop that turns true once 4,096 are done: stopped, and
    /// no more than `POLL_UNITS` units after it turned. Ticked before its work, the first run
    /// reset the allowance and the second ran unpolled: 8,191 units, never stopped.
    #[test]
    fn linear_passes_see_a_stop_across_passes() {
        let done = std::sync::atomic::AtomicUsize::new(0);
        let stop = || done.load(Ordering::Relaxed) >= crate::POLL_UNITS as usize;
        let stop: StopFn<'_> = &stop;
        let ticker = Ticker::new(stop).unwrap();
        let work = |run: std::ops::Range<usize>| {
            done.fetch_add(run.len(), Ordering::Relaxed);
        };
        let first = in_runs(&ticker, crate::POLL_UNITS as usize, work);
        let stopped =
            first.is_err() || in_runs(&ticker, crate::POLL_UNITS as usize - 1, work).is_err();
        let done = done.load(Ordering::Relaxed);
        assert!(stopped, "never stopped, {done} units done");
        assert!(done <= 2 * crate::POLL_UNITS as usize, "{done} units");
    }

    /// Spec (SPARQL 1.1 §9.3: `p+` and `p*` from a bound start are the nodes reached in one or
    /// more, or zero or more, steps, each once; review r1 of the graph closure package, R1-F1):
    /// a search from one start answers the fixpoint's ends of that start, over an
    /// [`Adjacency`] and over index probes alike, each node once, in the same order.
    #[test]
    fn a_search_from_one_start_answers_the_fixpoint() {
        let mut state = 23u64;
        let budget = Budget::unlimited();
        let pool = Pool::new(&budget);
        for _ in 0..200 {
            let mut next = || {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) % 9
            };
            let edges: Vec<(u64, u64)> = (0..next() * 2).map(|_| (next(), next())).collect();
            let adjacency = Adjacency::new(edges.clone());
            for (start, reflexive) in (0..9).flat_map(|s| [(s, false), (s, true)]) {
                let expected: Vec<u64> = naive_closure(&edges, &[start], reflexive)
                    .into_iter()
                    .map(|(_, end)| end)
                    .collect();
                let walked = reachable_within(start, reflexive, &adjacency, &pool, &|| false)
                    .unwrap()
                    .to_vec();
                let probes = Probes(|node| Ok::<_, Stop>(adjacency.neighbours(node).to_vec()));
                let probed = reachable_within(start, reflexive, probes, &pool, &|| false)
                    .unwrap()
                    .to_vec();
                assert_eq!(walked, probed, "{edges:?} from {start}");
                let mut sorted = walked.clone();
                sorted.sort_unstable();
                assert_eq!(
                    sorted, expected,
                    "{edges:?} from {start} reflexive={reflexive}"
                );
            }
        }
        drop(pool);
        assert_eq!(budget.used(), 0);
    }

    /// A chain `0 → 1 → … → n` probed one node at a time.
    fn chain_probes(n: u64) -> Probes<impl FnMut(u64) -> Result<Vec<u64>, Stop>> {
        Probes(move |node| Ok(if node < n { vec![node + 1] } else { Vec::new() }))
    }

    /// Spec (stop.rs, W1; R1-F1): a search from one start polls its stop while it walks. Over
    /// a 1,000,000-node chain, a stop that fires at its second poll ends it within
    /// 2 × `POLL_UNITS` nodes expanded (a node and its successor are a unit each). Searched
    /// without a stop, it walked the whole chain.
    #[test]
    fn a_search_from_one_start_stops_while_it_walks() {
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let stop = || polls.fetch_add(1, Ordering::Relaxed) >= 1;
        let expanded = std::cell::Cell::new(0usize);
        let budget = Budget::unlimited();
        let pool = Pool::new(&budget);
        let mut chain = chain_probes(1_000_000);
        let counted = Probes(|node| {
            expanded.set(expanded.get() + 1);
            (chain.0)(node)
        });
        let reached = reachable_within(0, false, counted, &pool, &stop);
        assert!(matches!(reached, Err(Stop::Cancelled)), "{reached:?}");
        let expanded = expanded.get();
        assert!(
            expanded <= 2 * crate::POLL_UNITS as usize,
            "{expanded} nodes expanded"
        );
    }

    /// Spec (R1-F1): a search's buffers (the nodes seen, the order, the frontiers) are charged
    /// before they grow: over a 100,000-node chain, 800 KB of order alone, under a 64 KiB
    /// budget, it is refused, and holds nothing once it is dropped.
    #[test]
    fn a_search_from_one_start_is_refused_past_its_budget() {
        let small = Budget::new(64 << 10);
        let pool = Pool::new(&small);
        let reached = reachable_within(0, false, chain_probes(100_000), &pool, &|| false);
        assert!(matches!(reached, Err(Stop::Memory(_))), "{reached:?}");
        drop(reached);
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
