//! SPARQL property paths over ids (SPARQL 1.1 §18.4).
//!
//! | Path | Result |
//! |---|---|
//! | `p`, `^p`, `a/b`, `a\|b`, `!(…)` | bags (an alternative is a UNION: a pair both sides give comes twice) |
//! | `p?` | deduplicated |
//! | `p+`, `p*` | sets (each end once per start), via breadth-first reachability |
//!
//! Zero-length paths (`*`, `?`) start from the terms that occur as a subject or object in
//! the graph the path is followed in ([`PathGraph`]), and from the path pattern's constant
//! end, whether the graph has it or not ([`PathEvaluator::fixed`]); an open `?x p* ?y`
//! pairs every node with itself. With both ends bound, a path has as many solutions as
//! its bag would hold the pair ([`PathEvaluator::multiplicity`]).
//!
//! The default graph is the store's, or the merge of all graphs
//! ([`PathEvaluator::merged`]): then every read takes a graph-last index order, where the
//! copies of a statement in several graphs are adjacent, and keeps one.
//!
//! A bound end is followed by index probes, so `ex:Cat rdfs:subClassOf* ?c` touches only the
//! nodes it reaches. Open closures are computed per strongly connected component of the
//! step's pairs ([`nrese_exec::graph::transitive_closure`]); the zero-length pairs of `p*` are every node with
//! itself, without a search. `COUNT` over an open closure ([`PathEvaluator::count_open`])
//! needs only the closure's size per node and the number of nodes, not its pairs: on YAGO,
//! `?c rdfs:subClassOf* ?d` has 1.7 million pairs from 133,000 classes and one per node of
//! the graph, tens of millions.
//!
//! A path joined to a pattern that binds one of its ends is evaluated from those values
//! only ([`PathEvaluator::reached_from`], [`PathEvaluator::reaching`]): the rows an open
//! evaluation would give for them, without the rest. `?p a :Person . ?x owl:sameAs* ?p`
//! then costs what the persons' identity groups cost, not one row per node of the graph.

use nrese_engine::quad::Permutation;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::graph::{
    Adjacency, closure, closure_sizes_until, reachable, transitive_closure_until,
};
use nrese_sparql_syntax::algebra::PropertyPathExpression;

/// A path with its IRIs resolved to ids; `None` marks an IRI the store doesn't know (no
/// edges).
pub(crate) enum Path {
    Link(Option<u64>),
    Reverse(Box<Path>),
    Sequence(Box<Path>, Box<Path>),
    Alternative(Box<Path>, Box<Path>),
    ZeroOrMore(Box<Path>),
    OneOrMore(Box<Path>),
    ZeroOrOne(Box<Path>),
    /// Predicates excluded (unknown IRIs excluded nothing and are dropped).
    Negated(Vec<u64>),
}

impl Path {
    pub(crate) fn resolve(path: &PropertyPathExpression, snapshot: &Snapshot) -> Self {
        let id = |n: &nrese_rdf::NamedNode| snapshot.lookup(n.as_ref().into()).map(TermId::raw);
        let boxed = |p: &PropertyPathExpression| Box::new(Self::resolve(p, snapshot));
        match path {
            PropertyPathExpression::NamedNode(n) => Self::Link(id(n)),
            PropertyPathExpression::Reverse(p) => Self::Reverse(boxed(p)),
            PropertyPathExpression::Sequence(a, b) => Self::Sequence(boxed(a), boxed(b)),
            PropertyPathExpression::Alternative(a, b) => Self::Alternative(boxed(a), boxed(b)),
            PropertyPathExpression::ZeroOrMore(p) => Self::ZeroOrMore(boxed(p)),
            PropertyPathExpression::OneOrMore(p) => Self::OneOrMore(boxed(p)),
            PropertyPathExpression::ZeroOrOne(p) => Self::ZeroOrOne(boxed(p)),
            PropertyPathExpression::NegatedPropertySet(ps) => {
                Self::Negated(ps.iter().filter_map(id).collect())
            }
        }
    }
}

/// Up to this many bound values, a closure probes the index from each node it reaches.
/// Beyond, it scans its step once into an [`Adjacency`] and walks that from the values.
const PROBED_VALUES: usize = 4096;

fn dedup_keep_order(mut values: Vec<u64>) -> Vec<u64> {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values.retain(|v| seen.insert(*v));
    values
}

fn dedup_pairs(mut pairs: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    let mut seen = std::collections::HashSet::with_capacity(pairs.len());
    pairs.retain(|p| seen.insert(*p));
    pairs
}

/// The graph a path is followed in.
#[derive(Clone, Copy)]
pub(crate) enum PathGraph<'a> {
    /// The store's default graph.
    Default,
    /// One named graph (`GRAPH <g>`, or a dataset whose default graph is one graph).
    Named(TermId),
    /// The merge of all graphs, or of the listed ones (a dataset with several `FROM`):
    /// every statement once.
    Merged(Option<&'a [TermId]>),
}

pub(crate) struct PathEvaluator<'a> {
    pub(crate) snapshot: &'a Snapshot,
    pub(crate) model: ReadModel,
    pub(crate) graph: PathGraph<'a>,
    /// A constant end of the path pattern: a zero-length step from it gives it, whether
    /// the graph has it or not (§18.4: `:x p* ?y` binds `?y` to `:x`). A value bound by
    /// other patterns has the zero-length step only as a node of the graph: the path is a
    /// pattern of its own, which the join meets.
    pub(crate) fixed: Option<u64>,
    /// The query's cancellation, polled inside traversals (closures, searches, the pairs
    /// of a sequence): a stopped traversal returns what it has, and the caller's check
    /// reports the cancellation (the review of 3 October 2026, P3).
    pub(crate) cancellation: Option<&'a crate::CancellationToken>,
    /// Closures computed per strongly connected component so far (an open `p+` or `p*`,
    /// or its size for `COUNT`), for EXPLAIN.
    pub(crate) closures: std::cell::Cell<usize>,
}

impl PathEvaluator<'_> {
    fn stopped(&self) -> bool {
        self.cancellation
            .is_some_and(crate::CancellationToken::is_cancelled)
    }

    /// [`transitive_closure_until`] stopped with the query: then empty.
    fn closure_of(&self, edges: &[(u64, u64)]) -> Vec<(u64, u64)> {
        self.closures.set(self.closures.get() + 1);
        let cancellation = self.cancellation;
        let stop = move || cancellation.is_some_and(crate::CancellationToken::is_cancelled);
        transitive_closure_until(edges, &stop).unwrap_or_default()
    }

    /// One step of a search forwards over `path` from `node`: none once stopped, which
    /// ends the search.
    fn step_from(&self, path: &Path, node: u64, out: &mut Vec<u64>) {
        if !self.stopped() {
            out.extend(self.from(path, node));
        }
    }

    /// [`Self::step_from`] backwards.
    fn step_to(&self, path: &Path, node: u64, out: &mut Vec<u64>) {
        if !self.stopped() {
            out.extend(self.to(path, node));
        }
    }

    fn quads(
        &self,
        subject: Option<u64>,
        predicate: Option<u64>,
        object: Option<u64>,
    ) -> impl Iterator<Item = (u64, u64, u64)> + '_ {
        let (selector, merged) = match self.graph {
            PathGraph::Default => (GraphSelector::Exact(TermId::DEFAULT_GRAPH), None),
            PathGraph::Named(graph) => (GraphSelector::Exact(graph), None),
            PathGraph::Merged(graphs) => (GraphSelector::Any, Some(graphs)),
        };
        let pattern = QuadPattern {
            subject: subject.map(TermId::from_raw),
            predicate: predicate.map(TermId::from_raw),
            object: object.map(TermId::from_raw),
            graph: selector,
        };
        let own = merged
            .is_none()
            .then(|| self.snapshot.quads_for_pattern_in(self.model, &pattern));
        // Merged: the graph-last order whose prefix the bound positions are.
        let merged = merged.map(|graphs| {
            let permutation = match (subject.is_some(), predicate.is_some(), object.is_some()) {
                (true, _, false) | (false, false, false) | (true, true, true) => Permutation::Spog,
                (false, true, _) => Permutation::Posg,
                (_, false, true) => Permutation::Ospg,
            };
            let mut previous = None;
            self.snapshot
                .scan_sorted_in(self.model, &pattern, permutation)
                .expect("the bound positions are a prefix of the order")
                .filter(move |quad| graphs.is_none_or(|graphs| graphs.contains(&quad.graph)))
                .filter(move |quad| {
                    let statement = (quad.subject, quad.predicate, quad.object);
                    let first = previous != Some(statement);
                    previous = Some(statement);
                    first
                })
        });
        own.into_iter()
            .flatten()
            .chain(merged.into_iter().flatten())
            .map(|q| (q.subject.raw(), q.predicate.raw(), q.object.raw()))
    }

    /// True if `node` is a subject or object in the default graph (or the path pattern's
    /// constant end, [`Self::fixed`]).
    fn is_node(&self, node: u64) -> bool {
        self.fixed == Some(node)
            || self.quads(Some(node), None, None).next().is_some()
            || self.quads(None, None, Some(node)).next().is_some()
    }

    /// Every subject and object of the graph, each once, sorted.
    fn nodes(&self) -> Vec<u64> {
        let (subjects, objects) = self.subjects_and_objects();
        let mut nodes = Vec::with_capacity(subjects.len().max(objects.len()));
        merge_union(&subjects, &objects, |node| nodes.push(node));
        nodes
    }

    /// The number of [`Self::nodes`], without collecting them (from the engine's
    /// statistics where the graph is one graph or all of them).
    fn node_count(&self) -> u64 {
        let graphs = match self.graph {
            PathGraph::Default => Some(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
            PathGraph::Named(graph) => Some(GraphSelector::Exact(graph)),
            PathGraph::Merged(None) => Some(GraphSelector::Any),
            PathGraph::Merged(Some(_)) => None,
        };
        if let Some(count) =
            graphs.and_then(|graphs| self.snapshot.node_count_in(self.model, graphs))
        {
            return count;
        }
        let (subjects, objects) = self.subjects_and_objects();
        let mut count = 0;
        merge_union(&subjects, &objects, |_| count += 1);
        count
    }

    /// The distinct subjects and the distinct objects of the graph, each sorted.
    fn subjects_and_objects(&self) -> (Vec<u64>, Vec<u64>) {
        // Reads the statements: for a set of graphs, which the walks don't know, and where
        // the walks can't answer.
        let read = || {
            let (mut subjects, mut objects): (Vec<u64>, Vec<u64>) =
                self.quads(None, None, None).map(|(s, _, o)| (s, o)).unzip();
            for values in [&mut subjects, &mut objects] {
                values.sort_unstable();
                values.dedup();
            }
            (subjects, objects)
        };
        let (pattern, by_subject, by_object) = match self.graph {
            PathGraph::Merged(None) => (QuadPattern::all(), Permutation::Spog, Permutation::Ospg),
            PathGraph::Merged(Some(_)) => return read(),
            PathGraph::Default => (
                QuadPattern::in_graph(TermId::DEFAULT_GRAPH),
                Permutation::Gspo,
                Permutation::Gosp,
            ),
            PathGraph::Named(graph) => (
                QuadPattern::in_graph(graph),
                Permutation::Gspo,
                Permutation::Gosp,
            ),
        };
        let distinct = |permutation| -> Option<Vec<u64>> {
            let counts = self
                .snapshot
                .group_counts_in(self.model, &pattern, permutation)?;
            Some(counts.into_iter().map(|(id, _)| id.raw()).collect())
        };
        match (distinct(by_subject), distinct(by_object)) {
            (Some(subjects), Some(objects)) => (subjects, objects),
            _ => read(),
        }
    }

    /// The number of pairs [`Self::open`] gives for a closure (`p+`, `p*`), from the
    /// closure's size per node and, for `p*`, the number of nodes: no pair is built.
    /// `None` for other paths.
    pub(crate) fn count_open(&self, path: &Path) -> Option<u64> {
        let (Path::OneOrMore(step) | Path::ZeroOrMore(step)) = path else {
            return None;
        };
        self.closures.set(self.closures.get() + 1);
        let cancellation = self.cancellation;
        let stop = move || cancellation.is_some_and(crate::CancellationToken::is_cancelled);
        let sizes = closure_sizes_until(&self.open(step), &stop)?;
        if matches!(path, Path::OneOrMore(_)) {
            return Some(sizes.iter().map(|&(_, reached, _)| reached).sum());
        }
        // Every node with itself, plus what each start reaches besides itself. The starts
        // are nodes: their pairs come from the graph's statements.
        let beyond: u64 = sizes
            .iter()
            .map(|&(_, reached, on_cycle)| reached - u64::from(on_cycle))
            .sum();
        Some(self.node_count() + beyond)
    }

    /// How many nodes the closure `path` (`p+` or `p*`) reaches from `node` (`forward`) or
    /// how many reach it, if that search expands at most `budget` nodes: the planner's
    /// estimate of a closure from a constant, exact where it is small. `None` past the
    /// budget and for other paths.
    pub(crate) fn reach_within(
        &self,
        path: &Path,
        node: u64,
        forward: bool,
        budget: usize,
    ) -> Option<usize> {
        let (Path::OneOrMore(step) | Path::ZeroOrMore(step)) = path else {
            return None;
        };
        let expanded = std::cell::Cell::new(0usize);
        let reached = reachable(node, matches!(path, Path::ZeroOrMore(_)), |n, out| {
            if expanded.get() < budget {
                expanded.set(expanded.get() + 1);
                match forward {
                    true => self.step_from(step, n, out),
                    false => self.step_to(step, n, out),
                }
            } else {
                expanded.set(budget + 1);
            }
        });
        (expanded.get() <= budget).then_some(reached.len())
    }

    /// Ends reachable from `start` (a bag).
    pub(crate) fn from(&self, path: &Path, start: u64) -> Vec<u64> {
        match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(Some(start), Some(*p), None)
                .map(|(_, _, o)| o)
                .collect(),
            Path::Reverse(p) => self.to(p, start),
            Path::Sequence(a, b) => self
                .from(a, start)
                .into_iter()
                .take_while(|_| !self.stopped())
                .flat_map(|m| self.from(b, m))
                .collect(),
            Path::Alternative(a, b) => {
                let mut ends = self.from(a, start);
                ends.extend(self.from(b, start));
                ends
            }
            Path::OneOrMore(p) => reachable(start, false, |n, out| self.step_from(p, n, out)),
            Path::ZeroOrMore(p) => {
                if !self.is_node(start) {
                    return Vec::new();
                }
                reachable(start, true, |n, out| self.step_from(p, n, out))
            }
            Path::ZeroOrOne(p) => {
                if !self.is_node(start) {
                    return Vec::new();
                }
                let mut ends = vec![start];
                ends.extend(self.from(p, start));
                dedup_keep_order(ends)
            }
            Path::Negated(excluded) => self
                .quads(Some(start), None, None)
                .filter(|(_, p, _)| !excluded.contains(p))
                .map(|(_, _, o)| o)
                .collect(),
        }
    }

    /// Starts that reach `end` (a bag).
    pub(crate) fn to(&self, path: &Path, end: u64) -> Vec<u64> {
        match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(None, Some(*p), Some(end))
                .map(|(s, _, _)| s)
                .collect(),
            Path::Reverse(p) => self.from(p, end),
            Path::Sequence(a, b) => self
                .to(b, end)
                .into_iter()
                .take_while(|_| !self.stopped())
                .flat_map(|m| self.to(a, m))
                .collect(),
            Path::Alternative(a, b) => {
                let mut starts = self.to(a, end);
                starts.extend(self.to(b, end));
                starts
            }
            Path::OneOrMore(p) => reachable(end, false, |n, out| self.step_to(p, n, out)),
            Path::ZeroOrMore(p) => {
                if !self.is_node(end) {
                    return Vec::new();
                }
                reachable(end, true, |n, out| self.step_to(p, n, out))
            }
            Path::ZeroOrOne(p) => {
                if !self.is_node(end) {
                    return Vec::new();
                }
                let mut starts = vec![end];
                starts.extend(self.to(p, end));
                dedup_keep_order(starts)
            }
            Path::Negated(excluded) => self
                .quads(None, None, Some(end))
                .filter(|(_, p, _)| !excluded.contains(p))
                .map(|(s, _, _)| s)
                .collect(),
        }
    }

    /// How many solutions `start path end` has: as many as `end` occurs in
    /// [`Self::from`]`(path, start)`, without building that bag. An alternative is a UNION
    /// (a pair both sides give counts twice) and a sequence a join (one per middle node);
    /// closures and `p?` are sets (at most one).
    pub(crate) fn multiplicity(&self, path: &Path, start: u64, end: u64) -> usize {
        match path {
            Path::Link(None) => 0,
            Path::Link(Some(p)) => self.quads(Some(start), Some(*p), Some(end)).count(),
            Path::Reverse(p) => self.multiplicity(p, end, start),
            Path::Sequence(a, b) => self
                .from(a, start)
                .into_iter()
                .map(|m| self.multiplicity(b, m, end))
                .sum(),
            Path::Alternative(a, b) => {
                self.multiplicity(a, start, end) + self.multiplicity(b, start, end)
            }
            Path::ZeroOrMore(_) | Path::OneOrMore(_) | Path::ZeroOrOne(_) => {
                usize::from(self.connects(path, start, end))
            }
            Path::Negated(excluded) => self
                .quads(Some(start), None, Some(end))
                .filter(|(_, p, _)| !excluded.contains(p))
                .count(),
        }
    }

    /// True if `path` connects `start` to `end`.
    pub(crate) fn connects(&self, path: &Path, start: u64, end: u64) -> bool {
        match path {
            Path::Link(None) => false,
            Path::Link(Some(p)) => self
                .quads(Some(start), Some(*p), Some(end))
                .next()
                .is_some(),
            Path::Reverse(p) => self.connects(p, end, start),
            Path::Sequence(a, b) => self
                .from(a, start)
                .into_iter()
                .any(|m| self.connects(b, m, end)),
            Path::Alternative(a, b) => self.connects(a, start, end) || self.connects(b, start, end),
            Path::ZeroOrMore(p) => {
                if start == end {
                    self.is_node(start)
                } else {
                    reachable(start, false, |n, out| self.step_from(p, n, out)).contains(&end)
                }
            }
            Path::OneOrMore(p) => {
                reachable(start, false, |n, out| self.step_from(p, n, out)).contains(&end)
            }
            Path::ZeroOrOne(p) => {
                if start == end {
                    self.is_node(start)
                } else {
                    self.connects(p, start, end)
                }
            }
            Path::Negated(excluded) => self
                .quads(Some(start), None, Some(end))
                .any(|(_, p, _)| !excluded.contains(&p)),
        }
    }

    /// The `(start, end)` pairs of [`Self::open`] whose start is one of `starts` (distinct
    /// values).
    pub(crate) fn reached_from(&self, path: &Path, starts: &[u64]) -> Vec<(u64, u64)> {
        match path {
            Path::OneOrMore(step) | Path::ZeroOrMore(step) if starts.len() > PROBED_VALUES => {
                let reflexive = matches!(path, Path::ZeroOrMore(_));
                let adjacency = Adjacency::new(self.open(step));
                let starts = starts
                    .iter()
                    .copied()
                    .take_while(|_| !self.stopped())
                    .filter(|&node| !reflexive || self.is_node(node));
                pairs_of(closure(&adjacency, starts, reflexive))
            }
            _ => starts
                .iter()
                .take_while(|_| !self.stopped())
                .flat_map(|&start| {
                    self.from(path, start)
                        .into_iter()
                        .map(move |end| (start, end))
                })
                .collect(),
        }
    }

    /// The `(start, end)` pairs of [`Self::open`] whose end is one of `ends` (distinct
    /// values).
    pub(crate) fn reaching(&self, path: &Path, ends: &[u64]) -> Vec<(u64, u64)> {
        match path {
            Path::OneOrMore(step) | Path::ZeroOrMore(step) if ends.len() > PROBED_VALUES => {
                let reflexive = matches!(path, Path::ZeroOrMore(_));
                let backwards =
                    Adjacency::new(self.open(step).into_iter().map(|(s, o)| (o, s)).collect());
                let ends = ends
                    .iter()
                    .copied()
                    .take_while(|_| !self.stopped())
                    .filter(|&node| !reflexive || self.is_node(node));
                pairs_of(closure(&backwards, ends, reflexive))
                    .into_iter()
                    .map(|(end, start)| (start, end))
                    .collect()
            }
            _ => ends
                .iter()
                .take_while(|_| !self.stopped())
                .flat_map(|&end| {
                    self.to(path, end)
                        .into_iter()
                        .map(move |start| (start, end))
                })
                .collect(),
        }
    }

    /// Every `(start, end)` pair.
    pub(crate) fn open(&self, path: &Path) -> Vec<(u64, u64)> {
        match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(None, Some(*p), None)
                .map(|(s, _, o)| (s, o))
                .collect(),
            Path::Reverse(p) => self.open(p).into_iter().map(|(s, o)| (o, s)).collect(),
            Path::Sequence(a, b) => self
                .open(a)
                .into_iter()
                .take_while(|_| !self.stopped())
                .flat_map(|(s, m)| self.from(b, m).into_iter().map(move |o| (s, o)))
                .collect(),
            Path::Alternative(a, b) => {
                let mut pairs = self.open(a);
                pairs.extend(self.open(b));
                pairs
            }
            Path::OneOrMore(p) => self.closure_of(&self.open(p)),
            Path::ZeroOrMore(p) => {
                let mut pairs = self.closure_of(&self.open(p));
                // Every node with itself, unless the closure has it already (on a cycle).
                let mut on_cycle: Vec<u64> = pairs
                    .iter()
                    .filter(|(a, b)| a == b)
                    .map(|&(a, _)| a)
                    .collect();
                on_cycle.sort_unstable();
                pairs.extend(
                    self.nodes()
                        .into_iter()
                        .filter(|node| on_cycle.binary_search(node).is_err())
                        .map(|node| (node, node)),
                );
                pairs
            }
            Path::ZeroOrOne(p) => {
                let mut pairs: Vec<(u64, u64)> = self.nodes().into_iter().map(|n| (n, n)).collect();
                pairs.extend(self.open(p));
                dedup_pairs(pairs)
            }
            Path::Negated(excluded) => self
                .quads(None, None, None)
                .filter(|(_, p, _)| !excluded.contains(p))
                .map(|(s, _, o)| (s, o))
                .collect(),
        }
    }
}

fn pairs_of(table: nrese_exec::IdTable) -> Vec<(u64, u64)> {
    table
        .column(0)
        .iter()
        .copied()
        .zip(table.column(1).iter().copied())
        .collect()
}

/// Calls `f` for each value of the union of two sorted, distinct lists, in order.
fn merge_union(a: &[u64], b: &[u64], mut f: impl FnMut(u64)) {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                f(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                f(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                f(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    a[i..].iter().chain(&b[j..]).for_each(|&x| f(x));
}

#[cfg(test)]
mod tests {
    use nrese_engine::{Engine, EngineConfig};
    use nrese_rdf::{GraphName, NamedNode, Quad};

    use super::*;

    fn node(i: usize) -> NamedNode {
        NamedNode::new_unchecked(format!("http://e/n{i}"))
    }

    #[test]
    fn cancelled_traversals_stop_inside() {
        // The review of 3 October 2026 (P3): a closure, a search and the pairs of a
        // sequence once ran to their end whatever the query's cancellation said.
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let next = NamedNode::new_unchecked("http://e/next");
        let mut tx = engine.transaction();
        for i in 0..50 {
            let quad = Quad::new(node(i), next.clone(), node(i + 1), GraphName::DefaultGraph);
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let step = PropertyPathExpression::NamedNode(next);
        let plus = Path::resolve(
            &PropertyPathExpression::OneOrMore(Box::new(step.clone())),
            &snapshot,
        );
        let two = Path::resolve(
            &PropertyPathExpression::Sequence(Box::new(step.clone()), Box::new(step)),
            &snapshot,
        );
        let token = crate::CancellationToken::new();
        let evaluator = PathEvaluator {
            snapshot: &snapshot,
            model: ReadModel::default(),
            graph: PathGraph::Default,
            fixed: None,
            cancellation: Some(&token),
            closures: Default::default(),
        };
        let first = snapshot.lookup(node(0).as_ref().into()).unwrap().raw();
        let last = snapshot.lookup(node(50).as_ref().into()).unwrap().raw();
        assert_eq!(evaluator.open(&plus).len(), 51 * 50 / 2);
        assert_eq!(evaluator.from(&plus, first).len(), 50);
        assert_eq!(evaluator.to(&plus, last).len(), 50);
        assert_eq!(evaluator.open(&two).len(), 49);
        assert!(evaluator.connects(&plus, first, last));

        token.cancel();
        assert!(evaluator.open(&plus).is_empty());
        assert!(evaluator.from(&plus, first).is_empty());
        assert!(evaluator.to(&plus, last).is_empty());
        assert!(evaluator.open(&two).is_empty());
        assert!(!evaluator.connects(&plus, first, last));
        assert_eq!(evaluator.count_open(&plus), None);
    }
}
