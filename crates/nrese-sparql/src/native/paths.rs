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
/// edges). Equal paths have equal bags ([`PathEvaluator::either`]).
#[derive(PartialEq, Eq)]
pub(crate) enum Path {
    Link(Option<u64>),
    Reverse(Box<Path>),
    /// The steps in order (ADR-0012: a list, walked in a loop however long).
    Sequence(Vec<Path>),
    Alternative(Vec<Path>),
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
            // `^^p` is `p`, pair for pair: one path, so that arms differing only by it are
            // the same arm ([`PathEvaluator::either`]).
            PropertyPathExpression::Reverse(p) => match &**p {
                PropertyPathExpression::Reverse(p) => Self::resolve(p, snapshot),
                _ => Self::Reverse(boxed(p)),
            },
            PropertyPathExpression::Sequence(steps) => {
                Self::Sequence(steps.iter().map(|p| Self::resolve(p, snapshot)).collect())
            }
            PropertyPathExpression::Alternative(paths) => {
                Self::Alternative(paths.iter().map(|p| Self::resolve(p, snapshot)).collect())
            }
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

/// A path's solutions from a node, or all of them: a bag (an alternative is a UNION, a
/// sequence a join). A sequence of alternatives multiplies the ways through a node: over
/// one statement `a p a`, `(p|p)/(p|p)/…` reaches `a` 2^n ways in n steps, so a sequence's
/// bag holds each value once with how often it occurs, never the copies, whose number grows
/// with the path's text. Only the caller that charges the copies ([`Self::len`]) makes them.
pub(crate) enum Bag<T> {
    /// One entry per copy.
    Copies(Vec<T>),
    /// Each entry with how many copies it stands for (saturating).
    Counted(Vec<(T, usize)>),
}

impl<T: Copy> Bag<T> {
    /// How many copies the bag holds (saturating).
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Copies(values) => values.len(),
            Self::Counted(counted) => counted
                .iter()
                .fold(0usize, |n, &(_, copies)| n.saturating_add(copies)),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Copies(values) => values.is_empty(),
            Self::Counted(counted) => counted.iter().all(|&(_, copies)| copies == 0),
        }
    }

    /// Each entry with how many copies it stands for.
    pub(crate) fn counted(&self) -> impl Iterator<Item = (T, usize)> + '_ {
        let (copies, counted) = match self {
            Self::Copies(values) => (Some(values), None),
            Self::Counted(counted) => (None, Some(counted)),
        };
        let copies = copies.into_iter().flatten().map(|&value| (value, 1));
        copies.chain(counted.into_iter().flatten().copied())
    }

    /// The values, without their copies (a value may still come more than once).
    fn values(self) -> Vec<T> {
        match self {
            Self::Copies(values) => values,
            Self::Counted(counted) => counted.into_iter().map(|(value, _)| value).collect(),
        }
    }

    /// Every copy: as many values as [`Self::len`], which the caller charged.
    pub(crate) fn into_copies(self) -> Vec<T> {
        let total = self.len();
        let counted = match self {
            Self::Copies(values) => return values,
            Self::Counted(counted) => counted,
        };
        let mut values = Vec::with_capacity(total);
        for (value, copies) in counted {
            values.extend(std::iter::repeat_n(value, copies));
        }
        values
    }
}

/// Bags added up, each value once with its copies, in the order each value first comes.
struct Tally<T> {
    counted: Vec<(T, usize)>,
    at: std::collections::HashMap<T, usize>,
}

impl<T: Copy + Eq + std::hash::Hash> Tally<T> {
    fn new() -> Self {
        Self {
            counted: Vec::new(),
            at: std::collections::HashMap::new(),
        }
    }

    fn add(&mut self, value: T, copies: usize) {
        match self.at.entry(value) {
            std::collections::hash_map::Entry::Occupied(at) => {
                let held = &mut self.counted[*at.get()].1;
                *held = held.saturating_add(copies);
            }
            std::collections::hash_map::Entry::Vacant(at) => {
                at.insert(self.counted.len());
                self.counted.push((value, copies));
            }
        }
    }

    fn finish(self) -> Bag<T> {
        Bag::Counted(self.counted)
    }
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
    Merged(Option<&'a crate::dataset::GraphSet>),
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
    /// The search takes each end once, however many ways lead there.
    fn step_from(&self, path: &Path, node: u64, out: &mut Vec<u64>) {
        if !self.stopped() {
            out.extend(self.from(path, node).values());
        }
    }

    /// [`Self::step_from`] backwards.
    fn step_to(&self, path: &Path, node: u64, out: &mut Vec<u64>) {
        if !self.stopped() {
            out.extend(self.to(path, node).values());
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
        let sizes = closure_sizes_until(&self.edges(step), &stop)?;
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
    pub(crate) fn from(&self, path: &Path, start: u64) -> Bag<u64> {
        let copies = match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(Some(start), Some(*p), None)
                .map(|(_, _, o)| o)
                .collect(),
            Path::Reverse(p) => return self.to(p, start),
            Path::Sequence(steps) => return self.through(steps, start, true),
            Path::Alternative(paths) => return self.either(paths, |p| self.from(p, start)),
            Path::OneOrMore(p) => reachable(start, false, |n, out| self.step_from(p, n, out)),
            Path::ZeroOrMore(p) => {
                if !self.is_node(start) {
                    return Bag::Copies(Vec::new());
                }
                reachable(start, true, |n, out| self.step_from(p, n, out))
            }
            Path::ZeroOrOne(p) => {
                if !self.is_node(start) {
                    return Bag::Copies(Vec::new());
                }
                let mut ends = vec![start];
                ends.extend(self.from(p, start).values());
                dedup_keep_order(ends)
            }
            Path::Negated(excluded) => self
                .quads(Some(start), None, None)
                .filter(|(_, p, _)| !excluded.contains(p))
                .map(|(_, _, o)| o)
                .collect(),
        };
        Bag::Copies(copies)
    }

    /// The ends reached from `node` through `steps` in sequence (`forward`), or the starts
    /// reaching it (the last step first): one per way through, each end once with how
    /// many, step by step.
    fn through(&self, steps: &[Path], node: u64, forward: bool) -> Bag<u64> {
        let mut nodes = Bag::Copies(vec![node]);
        let ordered: Box<dyn Iterator<Item = &Path>> = match forward {
            true => Box::new(steps.iter()),
            false => Box::new(steps.iter().rev()),
        };
        for step in ordered {
            if nodes.is_empty() {
                break;
            }
            let mut next = Tally::new();
            for (m, ways) in nodes.counted() {
                if self.stopped() {
                    break;
                }
                let ends = match forward {
                    true => self.from(step, m),
                    false => self.to(step, m),
                };
                for (end, more) in ends.counted() {
                    next.add(end, ways.saturating_mul(more));
                }
            }
            nodes = next.finish();
        }
        nodes
    }

    /// The bags of `paths` together (a UNION): copies stay copies, and counted ones are
    /// added up with them.
    /// An arm the alternative names again is the same bag again: it is followed once and
    /// its values counted as often, never copied, so `p|p|…` costs and holds what `p` does
    /// however often the path's text repeats it.
    fn either<T: Copy + Eq + std::hash::Hash>(
        &self,
        paths: &[Path],
        bag: impl Fn(&Path) -> Bag<T>,
    ) -> Bag<T> {
        let mut arms: Vec<(&Path, usize)> = Vec::with_capacity(paths.len());
        for path in paths {
            match arms.iter_mut().find(|(arm, _)| *arm == path) {
                Some((_, times)) => *times += 1,
                None => arms.push((path, 1)),
            }
        }
        let bags: Vec<(Bag<T>, usize)> =
            arms.iter().map(|&(arm, times)| (bag(arm), times)).collect();
        if bags
            .iter()
            .all(|(bag, times)| *times == 1 && matches!(bag, Bag::Copies(_)))
        {
            return Bag::Copies(bags.into_iter().flat_map(|(bag, _)| bag.values()).collect());
        }
        let mut all = Tally::new();
        for (bag, times) in &bags {
            for (value, copies) in bag.counted() {
                all.add(value, copies.saturating_mul(*times));
            }
        }
        all.finish()
    }

    /// Starts that reach `end` (a bag).
    pub(crate) fn to(&self, path: &Path, end: u64) -> Bag<u64> {
        let copies = match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(None, Some(*p), Some(end))
                .map(|(s, _, _)| s)
                .collect(),
            Path::Reverse(p) => return self.from(p, end),
            Path::Sequence(steps) => return self.through(steps, end, false),
            Path::Alternative(paths) => return self.either(paths, |p| self.to(p, end)),
            Path::OneOrMore(p) => reachable(end, false, |n, out| self.step_to(p, n, out)),
            Path::ZeroOrMore(p) => {
                if !self.is_node(end) {
                    return Bag::Copies(Vec::new());
                }
                reachable(end, true, |n, out| self.step_to(p, n, out))
            }
            Path::ZeroOrOne(p) => {
                if !self.is_node(end) {
                    return Bag::Copies(Vec::new());
                }
                let mut starts = vec![end];
                starts.extend(self.to(p, end).values());
                dedup_keep_order(starts)
            }
            Path::Negated(excluded) => self
                .quads(None, None, Some(end))
                .filter(|(_, p, _)| !excluded.contains(p))
                .map(|(s, _, _)| s)
                .collect(),
        };
        Bag::Copies(copies)
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
            Path::Sequence(steps) => match steps.split_last() {
                None => usize::from(start == end),
                Some((last, before)) => self
                    .through(before, start, true)
                    .counted()
                    .map(|(m, ways)| ways.saturating_mul(self.multiplicity(last, m, end)))
                    .fold(0, usize::saturating_add),
            },
            Path::Alternative(paths) => paths
                .iter()
                .map(|p| self.multiplicity(p, start, end))
                .fold(0, usize::saturating_add),
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
            Path::Sequence(steps) => match steps.split_last() {
                None => start == end,
                Some((last, before)) => {
                    let mut middle = self.through(before, start, true).values();
                    middle.sort_unstable();
                    middle.dedup();
                    middle.into_iter().any(|m| self.connects(last, m, end))
                }
            },
            Path::Alternative(paths) => paths.iter().any(|p| self.connects(p, start, end)),
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
    pub(crate) fn reached_from(&self, path: &Path, starts: &[u64]) -> Bag<(u64, u64)> {
        match path {
            Path::OneOrMore(step) | Path::ZeroOrMore(step) if starts.len() > PROBED_VALUES => {
                let reflexive = matches!(path, Path::ZeroOrMore(_));
                let adjacency = Adjacency::new(self.edges(step));
                let starts = starts
                    .iter()
                    .copied()
                    .take_while(|_| !self.stopped())
                    .filter(|&node| !reflexive || self.is_node(node));
                Bag::Copies(pairs_of(closure(&adjacency, starts, reflexive)))
            }
            _ => self.pairs_at(
                starts,
                |start| self.from(path, start),
                |start, end| (start, end),
            ),
        }
    }

    /// The `(start, end)` pairs of [`Self::open`] whose end is one of `ends` (distinct
    /// values).
    pub(crate) fn reaching(&self, path: &Path, ends: &[u64]) -> Bag<(u64, u64)> {
        match path {
            Path::OneOrMore(step) | Path::ZeroOrMore(step) if ends.len() > PROBED_VALUES => {
                let reflexive = matches!(path, Path::ZeroOrMore(_));
                let backwards =
                    Adjacency::new(self.edges(step).into_iter().map(|(s, o)| (o, s)).collect());
                let ends = ends
                    .iter()
                    .copied()
                    .take_while(|_| !self.stopped())
                    .filter(|&node| !reflexive || self.is_node(node));
                let pairs = pairs_of(closure(&backwards, ends, reflexive));
                Bag::Copies(pairs.into_iter().map(|(end, start)| (start, end)).collect())
            }
            _ => self.pairs_at(ends, |end| self.to(path, end), |end, start| (start, end)),
        }
    }

    /// The pairs of each of `nodes` (distinct) with each value of its bag, `pair` making a
    /// `(start, end)` pair of the node and the value: copies while every bag is copies.
    fn pairs_at(
        &self,
        nodes: &[u64],
        bag: impl Fn(u64) -> Bag<u64>,
        pair: impl Fn(u64, u64) -> (u64, u64),
    ) -> Bag<(u64, u64)> {
        let (mut copies, mut counted) = (Vec::new(), Vec::new());
        for &node in nodes.iter().take_while(|_| !self.stopped()) {
            match bag(node) {
                Bag::Copies(values) => copies.extend(values.into_iter().map(|v| pair(node, v))),
                Bag::Counted(values) => {
                    counted.extend(values.into_iter().map(|(v, n)| (pair(node, v), n)));
                }
            }
        }
        if counted.is_empty() {
            return Bag::Copies(copies);
        }
        counted.extend(copies.into_iter().map(|pair| (pair, 1)));
        Bag::Counted(counted)
    }

    /// The pairs of `path` as edges for a closure: each once where [`Self::open`] counts
    /// them, which a closure's reachability doesn't need.
    fn edges(&self, path: &Path) -> Vec<(u64, u64)> {
        self.open(path).values()
    }

    /// Every `(start, end)` pair.
    pub(crate) fn open(&self, path: &Path) -> Bag<(u64, u64)> {
        let copies = match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(None, Some(*p), None)
                .map(|(s, _, o)| (s, o))
                .collect(),
            Path::Reverse(p) => {
                let flip = |(s, o): (u64, u64)| (o, s);
                return match self.open(p) {
                    Bag::Copies(pairs) => Bag::Copies(pairs.into_iter().map(flip).collect()),
                    Bag::Counted(pairs) => {
                        Bag::Counted(pairs.into_iter().map(|(p, n)| (flip(p), n)).collect())
                    }
                };
            }
            Path::Sequence(steps) => {
                let Some((first, rest)) = steps.split_first() else {
                    return Bag::Copies(self.nodes().into_iter().map(|n| (n, n)).collect());
                };
                // Each pair once with how many ways through, step by step (`through`).
                let mut pairs = self.open(first);
                for step in rest {
                    let mut next = Tally::new();
                    for ((s, m), ways) in pairs.counted() {
                        if self.stopped() {
                            break;
                        }
                        for (o, more) in self.from(step, m).counted() {
                            next.add((s, o), ways.saturating_mul(more));
                        }
                    }
                    pairs = next.finish();
                }
                return pairs;
            }
            Path::Alternative(paths) => return self.either(paths, |p| self.open(p)),
            Path::OneOrMore(p) => self.closure_of(&self.edges(p)),
            Path::ZeroOrMore(p) => {
                let mut pairs = self.closure_of(&self.edges(p));
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
                pairs.extend(self.open(p).values());
                dedup_pairs(pairs)
            }
            Path::Negated(excluded) => self
                .quads(None, None, None)
                .filter(|(_, p, _)| !excluded.contains(p))
                .map(|(s, _, o)| (s, o))
                .collect(),
        };
        Bag::Copies(copies)
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
            &PropertyPathExpression::Sequence(vec![step.clone(), step]),
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

    /// Spec: SPARQL 1.1 §18.4 (ArbitraryLengthPath aside, paths are bags): an alternative
    /// is a UNION and a sequence a join, so an end comes once per way through; `p+` is a
    /// set. Over `a p b`, `a q b`, `b p a`, `b p c`, the path `(p|q)/p` goes from `a` to
    /// `b` two ways and on to `a` and `c`: each twice; from `b` once to `b`.
    #[test]
    fn sequences_of_alternatives_keep_one_end_per_way_through() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let [a, b, c, p, q] = ["a", "b", "c", "p", "q"]
            .map(|name| NamedNode::new_unchecked(format!("http://e/{name}")));
        let mut tx = engine.transaction();
        for (s, pr, o) in [(&a, &p, &b), (&a, &q, &b), (&b, &p, &a), (&b, &p, &c)] {
            let quad = Quad::new(s.clone(), pr.clone(), o.clone(), GraphName::DefaultGraph);
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let link = |n: &NamedNode| PropertyPathExpression::NamedNode(n.clone());
        let sequence = PropertyPathExpression::Sequence(vec![
            PropertyPathExpression::Alternative(vec![link(&p), link(&q)]),
            link(&p),
        ]);
        let plus = Path::resolve(
            &PropertyPathExpression::OneOrMore(Box::new(sequence.clone())),
            &snapshot,
        );
        let sequence = Path::resolve(&sequence, &snapshot);
        let evaluator = PathEvaluator {
            snapshot: &snapshot,
            model: ReadModel::default(),
            graph: PathGraph::Default,
            fixed: None,
            cancellation: None,
            closures: Default::default(),
        };
        let [a, b, c] = [&a, &b, &c].map(|n| snapshot.lookup(n.as_ref().into()).unwrap().raw());
        let sorted = |bag: Bag<u64>| {
            let mut values = bag.into_copies();
            values.sort_unstable();
            values
        };
        let pairs = |bag: Bag<(u64, u64)>| {
            let mut values = bag.into_copies();
            values.sort_unstable();
            values
        };
        assert_eq!(
            sorted(evaluator.from(&sequence, a)),
            sorted(Bag::Copies(vec![a, a, c, c]))
        );
        assert_eq!(sorted(evaluator.to(&sequence, c)), vec![a, a]);
        assert_eq!(sorted(evaluator.to(&sequence, b)), vec![b]);
        assert_eq!(evaluator.multiplicity(&sequence, a, c), 2);
        assert_eq!(evaluator.multiplicity(&sequence, b, b), 1);
        assert_eq!(evaluator.multiplicity(&sequence, b, c), 0);
        assert!(evaluator.connects(&sequence, a, a));
        assert!(!evaluator.connects(&sequence, c, a));
        assert_eq!(
            pairs(evaluator.open(&sequence)),
            pairs(Bag::Copies(vec![(a, a), (a, a), (a, c), (a, c), (b, b)]))
        );
        assert_eq!(pairs(evaluator.reached_from(&sequence, &[b])), vec![(b, b)]);
        assert_eq!(
            pairs(evaluator.reaching(&sequence, &[c])),
            vec![(a, c), (a, c)]
        );
        assert_eq!(
            sorted(evaluator.from(&plus, a)),
            sorted(Bag::Copies(vec![a, c]))
        );
        assert_eq!(evaluator.from(&plus, b).into_copies(), vec![b]);
        assert_eq!(evaluator.multiplicity(&plus, a, c), 1);
        assert_eq!(evaluator.multiplicity(&plus, b, c), 0);
        assert_eq!(
            pairs(evaluator.open(&plus)),
            pairs(Bag::Copies(vec![(a, a), (a, c), (b, b)]))
        );
    }
}
