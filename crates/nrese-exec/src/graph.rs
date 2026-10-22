//! Graph algorithms over id edge relations: reachability for SPARQL property paths
//! (`p+`, `p*`) and, in the reasoner, for hierarchies and transitive properties.
//!
//! [`Adjacency`] is a compressed sparse row index over `(source, target)` edges: sorted
//! targets per source, found by binary search over the distinct sources. [`reachable`]
//! walks it breadth-first with a visited set, so each node is expanded once: set semantics,
//! as SPARQL requires for `*` and `+`, and cycles terminate.

use hashbrown::HashSet;

use crate::table::IdTable;
use crate::{Budget, Reservation, Scratch, Stop, StopCheck, Ticker};
#[cfg(test)]
use std::sync::Arc;

type Hasher = foldhash::fast::FixedState;

#[cfg(test)]
thread_local! { static REACH_WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

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
/// The output is charged to `budget` as it grows.
pub fn closure(
    adjacency: &Adjacency,
    starts: impl IntoIterator<Item = u64>,
    reflexive: bool,
    budget: &std::sync::Arc<crate::Budget>,
) -> Result<IdTable, crate::BudgetExceeded> {
    let mut out = IdTable::charged(2, 0, budget)?;
    for start in starts {
        for end in reachable(start, reflexive, |node, buffer| {
            buffer.extend_from_slice(adjacency.neighbours(node))
        }) {
            out.push_row(&[start, end], budget)?;
        }
    }
    Ok(out)
}

/// The transitive closure of `edges`: every `(a, b)` with `b` reachable from `a` in one or
/// more steps, each once, in no particular order.
///
/// Computed per strongly connected component ([`Condensation`]). Apart from the output,
/// the cost is O(n + m) plus the total size of the components' reach sets. On a clique of
/// k nodes that's O(kÂ²), the size of the output, where a search per node ([`closure`]) is
/// O(kÂ³).
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
    let mut work = Ticker::new(stop).ok()?;
    let mut size = Vec::with_capacity(g.components());
    for c in 0..g.components() {
        work.tick(1).ok()?;
        let mut reached = 0;
        for &d in &g.reach[c] {
            work.tick(1).ok()?;
            reached += g.members_of(d as usize).len() as u64;
        }
        size.push(reached);
    }
    // Dense ids follow sorted node ids, including nodes rejected for no successors.
    let mut out = Vec::new();
    for x in 0..g.nodes.len() as u32 {
        work.tick(1).ok()?;
        if g.has_successors(x) {
            let c = g.component[x as usize] as usize;
            out.push((g.nodes[x as usize], size[c], g.cyclic[c]));
        }
    }
    work.finish().ok()?;
    Some(out)
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
    let mut ticker = Ticker::new(stop).ok()?;
    let (out, _) = components_within(offsets, targets, None, &mut ticker).ok()?;
    ticker.finish().ok()?;
    Some(out)
}

// Fixed graph buffers use the existing Scratch admission and move its actual token.
// None is only the unchanged, explicitly unbudgeted legacy graph entrypoints.
fn graph_buffer<T>(
    held: &mut Option<Reservation<&Budget>>,
    capacity: usize,
) -> Result<Vec<T>, Stop> {
    match held {
        Some(held) => {
            let budget = *held.handle();
            let mut allocation = Scratch::with_capacity(budget, capacity)?.into_parts();
            held.absorb_same_origin(&mut allocation.1);
            Ok(allocation.0)
        }
        None => Ok(Vec::with_capacity(capacity)),
    }
}

// Initialize/compact only admitted storage; each chunk performs at most the remaining
// work allowance, without a ticker branch in the element loop.
fn initialize<T: Clone, C: StopCheck>(
    items: &mut Vec<T>,
    len: usize,
    value: T,
    ticker: &mut Ticker<C>,
) -> Result<(), Stop> {
    while items.len() < len {
        let n = (len - items.len()).min(ticker.left() as usize);
        items.resize(items.len() + n, value.clone());
        ticker.tick(n as u32)?;
    }
    Ok(())
}

fn compact<T: Copy + Eq, C: StopCheck>(
    items: &mut Vec<T>,
    ticker: &mut Ticker<C>,
) -> Result<(), Stop> {
    let (mut at, mut kept) = (0, 0);
    while at < items.len() {
        let end = at + (items.len() - at).min(ticker.left() as usize);
        for i in at..end {
            if kept == 0 || items[i] != items[kept - 1] {
                items[kept] = items[i];
                kept += 1;
            }
        }
        ticker.tick((end - at) as u32)?;
        at = end;
    }
    items.truncate(kept);
    Ok(())
}

/// Working state shared by the existing recursion/SCC entrypoint and charged queries.
/// Legacy stacks retain their existing growth policy; query stacks admit their n-node
/// bound first. Data fields precede their one actual-owner token.
struct Tarjan<'b> {
    order: Vec<u32>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    calls: Vec<(u32, usize)>,
    #[cfg(test)]
    observed: Option<Arc<std::sync::atomic::AtomicUsize>>,
    _held: Option<Reservation<&'b Budget>>,
}

impl<'b> Tarjan<'b> {
    fn new<C: StopCheck>(
        n: usize,
        budget: Option<&'b Budget>,
        ticker: &mut Ticker<C>,
    ) -> Result<Self, Stop> {
        let mut held = budget.map(Reservation::new);
        let mut order = graph_buffer(&mut held, n)?;
        initialize(&mut order, n, u32::MAX, ticker)?;
        let mut low = graph_buffer(&mut held, n)?;
        initialize(&mut low, n, 0, ticker)?;
        let mut on_stack = graph_buffer(&mut held, n)?;
        initialize(&mut on_stack, n, false, ticker)?;
        let stack = graph_buffer(&mut held, if budget.is_some() { n } else { 0 })?;
        let calls = graph_buffer(&mut held, if budget.is_some() { n } else { 0 })?;
        Ok(Self {
            order,
            low,
            on_stack,
            stack,
            calls,
            #[cfg(test)]
            observed: None,
            _held: held,
        })
    }

    #[cfg(test)]
    fn note(&self) {
        if let Some(count) = &self.observed {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn run<C: StopCheck>(
        &mut self,
        offsets: &[u32],
        targets: &[u32],
        out: &mut Components,
        ticker: &mut Ticker<C>,
    ) -> Result<(), Stop> {
        let n = offsets.len() - 1;
        let mut counter = 0u32;
        let visit = |v: u32, this: &mut Self, counter: &mut u32| {
            this.order[v as usize] = *counter;
            this.low[v as usize] = *counter;
            *counter += 1;
            this.stack.push(v);
            this.on_stack[v as usize] = true;
            this.calls.push((v, 0));
        };
        for root in 0..n as u32 {
            #[cfg(test)]
            self.note();
            ticker.tick(1)?;
            if self.order[root as usize] != u32::MAX {
                continue;
            }
            visit(root, self, &mut counter);
            while let Some(&(v, next)) = self.calls.last() {
                let successors =
                    &targets[offsets[v as usize] as usize..offsets[v as usize + 1] as usize];
                if let Some(&w) = successors.get(next) {
                    #[cfg(test)]
                    self.note();
                    ticker.tick(1)?;
                    self.calls.last_mut().expect("not empty").1 += 1;
                    if self.order[w as usize] == u32::MAX {
                        visit(w, self, &mut counter);
                    } else if self.on_stack[w as usize] {
                        self.low[v as usize] = self.low[v as usize].min(self.order[w as usize]);
                    }
                    continue;
                }
                #[cfg(test)]
                self.note();
                ticker.tick(1)?;
                self.calls.pop();
                if let Some(&(u, _)) = self.calls.last() {
                    self.low[u as usize] = self.low[u as usize].min(self.low[v as usize]);
                }
                if self.low[v as usize] == self.order[v as usize] {
                    let c = out.starts.len() as u32 - 1;
                    loop {
                        #[cfg(test)]
                        self.note();
                        ticker.tick(1)?;
                        let w = self.stack.pop().expect("v is on the stack");
                        self.on_stack[w as usize] = false;
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
        Ok(())
    }
}

fn components_within<'b, C: StopCheck>(
    offsets: &[u32],
    targets: &[u32],
    budget: Option<&'b Budget>,
    ticker: &mut Ticker<C>,
) -> Result<(Components, Option<Reservation<&'b Budget>>), Stop> {
    let n = offsets.len() - 1;
    // This is the existing sentinel's actual node-id bound, not a graph-size setting.
    if n != 0 {
        crate::narrow::node_id(n - 1).map_err(Stop::TooLarge)?;
    }
    let mut held = budget.map(Reservation::new);
    let mut out = Components {
        component: graph_buffer(&mut held, n)?,
        members: graph_buffer(&mut held, n)?,
        starts: graph_buffer(&mut held, if budget.is_some() { n + 1 } else { 1 })?,
    };
    initialize(&mut out.component, n, 0, ticker)?;
    out.starts.push(0);
    let mut work = Tarjan::new(n, budget, ticker)?;
    work.run(offsets, targets, &mut out, ticker)?;
    // Drop work before returning its caller's persistent components/token pair.
    drop(work);
    Ok((out, held))
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
struct Condensation<'b> {
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
    // Last: all persistent graph buffers are freed before their actual tokens.
    held: Option<Reservation<&'b Budget>>,
}

impl<'b> Condensation<'b> {
    fn of(edges: &[(u64, u64)], stop: &(dyn Fn() -> bool + Sync)) -> Option<Self> {
        let mut ticker = Ticker::new(stop).ok()?;
        let mut g = Self::prepare(edges.len(), |i| edges[i], None, stop, &mut ticker).ok()?;
        g.reach(stop, &mut ticker).ok()?;
        ticker.finish().ok()?;
        Some(g)
    }

    fn prepare<F: Fn() -> bool + Sync + ?Sized>(
        len: usize,
        edge: impl Fn(usize) -> (u64, u64) + Sync,
        budget: Option<&'b Budget>,
        stop: &F,
        ticker: &mut Ticker<&F>,
    ) -> Result<Self, Stop> {
        use rayon::prelude::*;
        let mut held = budget.map(Reservation::new);
        let bound = len
            .checked_mul(2)
            .ok_or(Stop::TooLarge(crate::RepresentationLimit {
                what: "graph endpoints",
                limit: usize::MAX as u64 / 2,
            }))?;
        let mut nodes = graph_buffer(&mut held, bound)?;
        for i in 0..len {
            ticker.tick(1)?;
            let (a, b) = edge(i);
            nodes.extend([a, b]);
        }
        ticker.check()?;
        nodes.par_sort_unstable();
        ticker.check()?;
        compact(&mut nodes, ticker)?;
        let n = nodes.len();
        if n != 0 {
            crate::narrow::node_id(n - 1).map_err(Stop::TooLarge)?;
        }
        // One admitted dense-pair allocation. Each task fills a disjoint primitive range;
        // no per-pair shared stop flag or output-row binary search.
        let mut pair_held = budget.map(Reservation::new);
        let mut pairs: Vec<(u32, u32)> = graph_buffer(&mut pair_held, len)?;
        let slots = &mut pairs.spare_capacity_mut()[..len];
        slots
            .par_chunks_mut(crate::POLL_UNITS as usize)
            .enumerate()
            .try_for_each(|(part, slots)| {
                let mut work = Ticker::new(stop)?;
                for (i, slot) in slots.iter_mut().enumerate() {
                    let (a, b) = edge(part * crate::POLL_UNITS as usize + i);
                    let dense =
                        |x| nodes.binary_search(&x).expect("every endpoint is a node") as u32;
                    slot.write((dense(a), dense(b)));
                }
                work.tick(slots.len() as u32)?;
                Ok::<_, Stop>(())
            })?;
        // SAFETY: successful joined tasks initialized every disjoint pair slot. On an
        // error the length stays0; initialized primitive pairs need no destructor.
        unsafe { pairs.set_len(len) };
        ticker.check()?;
        pairs.par_sort_unstable();
        ticker.check()?;
        compact(&mut pairs, ticker)?;
        crate::narrow::edge_offset(pairs.len()).map_err(Stop::TooLarge)?;
        let mut offsets = graph_buffer(&mut held, n + 1)?;
        initialize(&mut offsets, n + 1, 0u32, ticker)?;
        let mut targets = graph_buffer(&mut held, pairs.len())?;
        for &(a, b) in &pairs {
            ticker.tick(1)?;
            offsets[a as usize + 1] += 1;
            targets.push(b);
        }
        for i in 0..n {
            ticker.tick(1)?;
            offsets[i + 1] += offsets[i];
        }
        // Dense-pair temporary is not kept beside Tarjan/reach or final output.
        drop(pairs);
        drop(pair_held);
        let mut made = components_within(&offsets, &targets, budget, ticker)?;
        if let (Some(held), Some(token)) = (&mut held, &mut made.1) {
            held.absorb_same_origin(token);
        }
        let Components {
            component,
            members,
            starts,
        } = made.0;
        Ok(Self {
            nodes,
            offsets,
            targets,
            component,
            members,
            starts,
            cyclic: Vec::new(),
            reach: Vec::new(),
            held,
        })
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

    /// One existing reach-union kernel, with either the legacy Vec or actual charged
    /// Scratch insertion inlined at its caller. Rejected entries count as work too.
    fn reach_component<C: StopCheck>(
        &self,
        c: usize,
        stamp: &mut [u32],
        ticker: &mut Ticker<C>,
        mut push: impl FnMut(u32) -> Result<(), Stop>,
    ) -> Result<bool, Stop> {
        let mut cyclic = self.members_of(c).len() > 1;
        for &x in self.members_of(c) {
            ticker.tick(1)?;
            for &y in self.successors(x) {
                ticker.tick(1)?;
                let d = self.component[y as usize];
                if d as usize == c {
                    cyclic = true;
                    continue;
                }
                if stamp[d as usize] != c as u32 {
                    stamp[d as usize] = c as u32;
                    push(d)?;
                    for &e in &self.reach[d as usize] {
                        #[cfg(test)]
                        REACH_WORK.with(|x| x.set(x.get() + 1));
                        ticker.tick(1)?;
                        if stamp[e as usize] != c as u32 {
                            stamp[e as usize] = c as u32;
                            push(e)?;
                        }
                    }
                }
            }
        }
        if cyclic {
            push(c as u32)?;
        }
        Ok(cyclic)
    }

    fn reach<F: Fn() -> bool + Sync + ?Sized>(
        &mut self,
        _stop: &F,
        ticker: &mut Ticker<&F>,
    ) -> Result<(), Stop> {
        let components = self.components();
        let budget = self.held.as_ref().map(|h| *h.handle());
        let mut working = budget.map(Reservation::new);
        let mut stamp = graph_buffer(&mut working, components)?;
        initialize(&mut stamp, components, u32::MAX, ticker)?;
        self.cyclic = graph_buffer(&mut self.held, components)?;
        self.reach = graph_buffer(&mut self.held, components)?;
        for c in 0..components {
            ticker.tick(1)?;
            let (set, cyclic) = if let Some(budget) = budget {
                // The first successor already has a complete reach set. Its union
                // with that successor is a guaranteed minimum of this set, not slack
                // that could take a later component's headroom. Reuse that O(1) fact
                // to skip 1/2-slot reallocations. Powers of two keep normal Scratch
                // growth/capacities; empty sinks still allocate nothing.
                let members = self.members_of(c);
                let minimum = self.successors(members[0]).first().map_or(0, |&y| {
                    let d = self.component[y as usize] as usize;
                    if d == c {
                        1
                    } else {
                        self.reach[d].len() + usize::from(!self.cyclic[d])
                    }
                });
                let capacity = if minimum >= 4 {
                    4
                } else if minimum >= 2 {
                    2
                } else {
                    minimum
                };
                let mut set = Scratch::with_capacity(budget, capacity)?;
                let cyclic = self.reach_component(c, &mut stamp, ticker, |x| {
                    set.push(x)?;
                    Ok(())
                })?;
                let mut made = set.into_parts();
                self.held
                    .as_mut()
                    .expect("charged graph")
                    .absorb_same_origin(&mut made.1);
                (made.0, cyclic)
            } else {
                let mut set = Vec::new();
                let cyclic = self.reach_component(c, &mut stamp, ticker, |x| {
                    set.push(x);
                    Ok(())
                })?;
                (set, cyclic)
            };
            self.cyclic.push(cyclic);
            self.reach.push(set);
        }
        // Working stamp is freed before its own charge (locals drop in reverse order).
        drop(stamp);
        drop(working);
        Ok(())
    }

    fn counts<C: StopCheck>(&self, ticker: &mut Ticker<C>) -> Result<(u64, u64), Stop> {
        let (mut pairs, mut diagonal) = (0u64, 0u64);
        for c in 0..self.components() {
            ticker.tick(1)?;
            let mut reached = 0u64;
            for &d in &self.reach[c] {
                ticker.tick(1)?;
                reached += self.members_of(d as usize).len() as u64;
            }
            let members = self.members_of(c).len() as u64;
            pairs += members * reached;
            if self.cyclic[c] {
                diagonal += members;
            }
        }
        Ok((pairs, diagonal))
    }
}

/// Positive-length closure and self-pair counts, without output pairs or per-node size
/// records. Consumes the charged input once CSR no longer needs it. Scratch uses the
/// supplied actual Budget; the returned scalars own no memory.
pub fn closure_counts_within<F: Fn() -> bool + Sync + ?Sized>(
    edges: IdTable,
    source: usize,
    target: usize,
    budget: &Budget,
    stop: &F,
) -> Result<(u64, u64), Stop> {
    let mut work = Ticker::new(stop)?;
    let mut g = Condensation::prepare(
        edges.len(),
        |i| (edges.get(i, source), edges.get(i, target)),
        Some(budget),
        stop,
        &mut work,
    )?;
    drop(edges);
    let counts = if g.components() == 1 {
        // Every input node belongs to the one SCC. A singleton necessarily has an
        // input self-edge; larger SCCs are cyclic. No reach set or rescan is needed.
        let n = g.nodes.len() as u64;
        (n * n, n)
    } else {
        g.reach(stop, &mut work)?;
        g.counts(&mut work)?
    };
    work.finish()?;
    Ok(counts)
}

/// Nodes with a positive-length path to themselves, in node-id order. Uses SCC members
/// and singleton self-loops only; no component reach sets or closure pairs are built.
pub fn cyclic_nodes_within<'b, F: Fn() -> bool + Sync + ?Sized>(
    edges: IdTable,
    source: usize,
    target: usize,
    budget: &'b Budget,
    stop: &F,
) -> Result<Scratch<'b, u64>, Stop> {
    let mut work = Ticker::new(stop)?;
    let g = Condensation::prepare(
        edges.len(),
        |i| (edges.get(i, source), edges.get(i, target)),
        Some(budget),
        stop,
        &mut work,
    )?;
    drop(edges);
    let mut cyclic = Scratch::with_capacity(budget, g.components())?;
    let mut rows = 0;
    for c in 0..g.components() {
        work.tick(1)?;
        let members = g.members_of(c);
        let yes = members.len() > 1 || g.successors(members[0]).binary_search(&members[0]).is_ok();
        rows += if yes { members.len() } else { 0 };
        cyclic.push(yes)?;
    }
    let mut out = Scratch::with_capacity(budget, rows)?;
    for (x, &c) in g.component.iter().enumerate() {
        work.tick(1)?;
        if cyclic[c as usize] {
            out.push(g.nodes[x])?;
        }
    }
    work.finish()?;
    Ok(out)
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
            let mut expected: Vec<(u64, u64)> = closure(
                &adjacency,
                adjacency.sources().to_vec(),
                false,
                &crate::test_budget(),
            )
            .unwrap()
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

    /// Floydâ€“Warshall-style fixpoint over a tiny graph as the reference.
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
                let mut got: Vec<(u64, u64)> = closure(
                    &adjacency,
                    nodes.iter().copied(),
                    reflexive,
                    &crate::test_budget(),
                )
                .unwrap()
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
    fn charged_edges(edges: &[(u64, u64)], budget: &Arc<Budget>) -> IdTable {
        let mut out = IdTable::charged(2, edges.len(), budget).unwrap();
        for &(a, b) in edges {
            out.push_reserved(&[a, b]);
        }
        out
    }

    #[test]
    fn scalar_counts_and_cyclic_members_match_the_pair_oracle() {
        let mut state = 71u64;
        for case in 0..120 {
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 33) % 9
            };
            let edges: Vec<_> = (0..next() * 3).map(|_| (next(), next())).collect();
            let expected = naive_closure(&edges, &(0..9).collect::<Vec<_>>(), false);
            let diagonal: Vec<_> = (0..9).filter(|&n| expected.contains(&(n, n))).collect();
            let budget = crate::test_budget();
            let counts =
                closure_counts_within(charged_edges(&edges, &budget), 0, 1, &budget, &|| false)
                    .unwrap();
            assert_eq!(
                counts,
                (expected.len() as u64, diagonal.len() as u64),
                "case {case}: {edges:?}"
            );
            let got = cyclic_nodes_within(charged_edges(&edges, &budget), 0, 1, &budget, &|| false)
                .unwrap();
            assert_eq!(&*got, &diagonal, "case {case}: {edges:?}");
            drop(got);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn tarjan_counts_roots_edges_and_scc_pops_before_output() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Each case is finite even with the periodic checks removed. The test observes
        // actual kernel work, including rejected edges and skipped, already-seen roots.
        for kind in 0..3 {
            let n = if kind == 1 { 128 } else { 8192 };
            let (offsets, targets): (Vec<u32>, Vec<u32>) = if kind == 1 {
                (
                    (0..=n).map(|i| (i * n) as u32).collect(),
                    (0..n).flat_map(|_| 0..n as u32).collect(),
                )
            } else {
                (
                    (0..=n as u32).collect(),
                    (0..n as u32).map(|i| (i + 1) % n as u32).collect(),
                )
            };
            let observed = Arc::new(AtomicUsize::new(0));
            let arm = if kind == 2 { 2 * n + 2 } else { 1 };
            let stop = || observed.load(Ordering::Relaxed) >= arm;
            let mut ticker = Ticker::new(&stop).unwrap();
            let mut work = Tarjan::new(n, None, &mut ticker).unwrap();
            if kind == 0 {
                work.order.fill(0);
            }
            work.observed = Some(Arc::clone(&observed));
            let mut out = Components {
                component: vec![0; n],
                members: Vec::new(),
                starts: vec![0],
            };
            let result = work
                .run(&offsets, &targets, &mut out, &mut ticker)
                .and_then(|()| ticker.finish());
            let after = observed.load(Ordering::Relaxed) - arm;
            assert!(
                after <= crate::POLL_UNITS as usize,
                "actual Tarjan work must reach a checkpoint: case {kind}, {after}"
            );
            assert_eq!(result, Err(Stop::Cancelled));
        }
    }

    #[test]
    fn reach_rejections_are_bounded_without_new_output() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // c -> d -> [0..8192), but c's stamp already contains every reached node.
        // Only d is newly inserted: rejected reach-union entries still do real work.
        let n = 8192usize;
        let c = n + 1;
        let d = n;
        let mut g = Condensation {
            nodes: Vec::new(),
            offsets: vec![0; c + 2],
            targets: vec![d as u32],
            component: (0..=c as u32).collect(),
            members: (0..=c as u32).collect(),
            starts: (0..=c + 1).collect(),
            cyclic: Vec::new(),
            reach: (0..=c).map(|_| Vec::new()).collect(),
            held: None,
        };
        g.offsets[c + 1] = 1;
        g.reach[d] = (0..n as u32).collect();
        let mut stamp = vec![c as u32; c + 1];
        stamp[d] = u32::MAX;
        // A cfg(test) observation in the actual loop counts scanned reach entries.
        REACH_WORK.with(|x| x.set(0));
        let polls = AtomicUsize::new(0);
        let stop = || {
            polls.fetch_add(1, Ordering::Relaxed);
            REACH_WORK.with(|x| x.get() > 0)
        };
        let mut ticker = Ticker::new(&stop).unwrap();
        let mut inserts = 0;
        let result = g
            .reach_component(c, &mut stamp, &mut ticker, |_| {
                inserts += 1;
                Ok(())
            })
            .and_then(|_| ticker.finish());
        let scanned = REACH_WORK.with(std::cell::Cell::get);
        assert!(
            scanned <= crate::POLL_UNITS as usize,
            "rejected reach work must reach its checkpoint: {scanned}"
        );
        assert_eq!(inserts, 1);
        assert_eq!(result, Err(Stop::Cancelled));
        assert!(polls.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn charged_preparation_refusal_cancellation_and_result_lifetime() {
        if !crate::measure::alone() {
            return;
        }
        static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
        let pool = POOL.get_or_init(|| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap();
            pool.broadcast(|_| ());
            pool
        });
        pool.install(|| {
            for limit in [0, 48, 128, 1024, 4096, usize::MAX] {
                let budget = Arc::new(Budget::new(limit));
                let ((), excess) = crate::measure::measure_within(&budget, || {
                    // Inputs and every token belong to this measured operation.
                    let mut edges = match IdTable::charged(2, 128, &budget) {
                        Ok(x) => x,
                        Err(_) => return,
                    };
                    for i in 0..128 {
                        edges.push_reserved(&[i, (i + 1) % 128]);
                    }
                    let out = cyclic_nodes_within(edges, 0, 1, &budget, &|| false);
                    match out {
                        Ok(out) => {
                            assert_eq!(out.len(), 128);
                            assert_eq!(budget.used(), out.capacity() * size_of::<u64>());
                            drop(out);
                        }
                        Err(e) => assert!(matches!(e, Stop::Memory(_))),
                    }
                });
                assert_eq!(excess, 0, "reserve first, including drop: budget {limit}");
                assert_eq!(budget.used(), 0);
            }
            // Strict reach growth and exact headroom, including an unrelated owner
            // that must survive both success and refusal. No old-token measurement credit.
            let chain = |limit, success| {
                let budget = Arc::new(Budget::new(limit));
                let ((), excess) = crate::measure::measure_within(&budget, || {
                    let unrelated = Scratch::<u64>::with_capacity(&*budget, 3).unwrap();
                    let mut edges = IdTable::charged(2, 128, &budget).unwrap();
                    for i in 0..128 {
                        edges.push_reserved(&[i, i + 1]);
                    }
                    let result = closure_counts_within(edges, 0, 1, &budget, &|| false);
                    if success {
                        assert_eq!(result, Ok((128 * 129 / 2, 0)));
                    } else {
                        assert!(matches!(result, Err(Stop::Memory(_))));
                    }
                    assert_eq!(budget.used(), unrelated.capacity() * size_of::<u64>());
                    drop(unrelated);
                });
                assert_eq!(excess, 0, "reach admission and exact headroom");
                assert_eq!(budget.used(), 0);
                budget.peak()
            };
            let peak = chain(usize::MAX, true);
            assert_eq!(chain(peak, true), peak);
            chain(peak - 1, false);
            let budget = crate::test_budget();
            let ((), excess) = crate::measure::measure_within(&budget, || {
                let mut edges = IdTable::charged(2, 8192, &budget).unwrap();
                for i in 0..8192 {
                    edges.push_reserved(&[i, (i + 1) % 8192]);
                }
                let polls = std::sync::atomic::AtomicUsize::new(0);
                let stop = || polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 3;
                assert!(matches!(
                    closure_counts_within(edges, 0, 1, &budget, &stop),
                    Err(Stop::Cancelled)
                ));
            });
            assert_eq!(excess, 0, "cancellation cleanup");
            assert_eq!(budget.used(), 0);
        });
    }
}
