//! SPARQL property paths over ids (SPARQL 1.1 §18.4; the deviations the reference
//! evaluator's `Deviations` names aside).
//!
//! | Path | Result |
//! |---|---|
//! | `p`, `^p`, `a/b`, `!(…)` | bags |
//! | `a\|b`, `p?` | deduplicated |
//! | `p+`, `p*` | sets (each end once per start), via breadth-first reachability |
//!
//! Zero-length paths (`*`, `?`) only start from terms that occur as a subject or object in
//! the graph the path is followed in ([`PathGraph`]); an open `?x p* ?y` pairs every such node with itself. Both ends bound
//! is an existence test (one solution or none).
//!
//! The default graph is the store's, or the merge of all graphs
//! ([`PathEvaluator::merged`]): then every read takes a graph-last index order, where the
//! copies of a statement in several graphs are adjacent, and keeps one.
//!
//! A bound end is followed by index probes, so `ex:Cat rdfs:subClassOf* ?c` touches only the
//! nodes it reaches. Open closures build an [`Adjacency`] from the step's pairs and walk it
//! from every start.
//!
//! A path joined to a pattern that binds one of its ends is evaluated from those values
//! only ([`PathEvaluator::reached_from`], [`PathEvaluator::reaching`]): the rows an open
//! evaluation would give for them, without the rest. `?p a :Person . ?x owl:sameAs* ?p`
//! then costs what the persons' identity groups cost, not one row per node of the graph.

use nrese_engine::quad::Permutation;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::graph::{Adjacency, closure, reachable};
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
}

impl PathEvaluator<'_> {
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

    /// True if `node` is a subject or object in the default graph.
    fn is_node(&self, node: u64) -> bool {
        self.quads(Some(node), None, None).next().is_some()
            || self.quads(None, None, Some(node)).next().is_some()
    }

    /// Every subject and object of the graph, each once, sorted.
    fn nodes(&self) -> Vec<u64> {
        let (pattern, by_subject, by_object) = match self.graph {
            PathGraph::Merged(None) => (QuadPattern::all(), Permutation::Spog, Permutation::Ospg),
            // The statistics know no set of graphs: read its statements.
            PathGraph::Merged(Some(_)) => {
                let mut nodes: Vec<u64> = self
                    .quads(None, None, None)
                    .flat_map(|(s, _, o)| [s, o])
                    .collect();
                nodes.sort_unstable();
                nodes.dedup();
                return nodes;
            }
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
        let distinct = |permutation| {
            self.snapshot
                .group_counts_in(self.model, &pattern, permutation)
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _)| id.raw())
        };
        let mut nodes: Vec<u64> = distinct(by_subject).chain(distinct(by_object)).collect();
        nodes.sort_unstable();
        nodes.dedup();
        nodes
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
                .flat_map(|m| self.from(b, m))
                .collect(),
            Path::Alternative(a, b) => {
                let mut ends = self.from(a, start);
                ends.extend(self.from(b, start));
                dedup_keep_order(ends)
            }
            Path::OneOrMore(p) => reachable(start, false, |n, out| out.extend(self.from(p, n))),
            Path::ZeroOrMore(p) => {
                if !self.is_node(start) {
                    return Vec::new();
                }
                reachable(start, true, |n, out| out.extend(self.from(p, n)))
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
                .flat_map(|m| self.to(a, m))
                .collect(),
            Path::Alternative(a, b) => {
                let mut starts = self.to(a, end);
                starts.extend(self.to(b, end));
                dedup_keep_order(starts)
            }
            Path::OneOrMore(p) => reachable(end, false, |n, out| out.extend(self.to(p, n))),
            Path::ZeroOrMore(p) => {
                if !self.is_node(end) {
                    return Vec::new();
                }
                reachable(end, true, |n, out| out.extend(self.to(p, n)))
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
                    reachable(start, false, |n, out| out.extend(self.from(p, n))).contains(&end)
                }
            }
            Path::OneOrMore(p) => {
                reachable(start, false, |n, out| out.extend(self.from(p, n))).contains(&end)
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
                    .filter(|&node| !reflexive || self.is_node(node));
                pairs_of(closure(&adjacency, starts, reflexive))
            }
            _ => starts
                .iter()
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
                    .filter(|&node| !reflexive || self.is_node(node));
                pairs_of(closure(&backwards, ends, reflexive))
                    .into_iter()
                    .map(|(end, start)| (start, end))
                    .collect()
            }
            _ => ends
                .iter()
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
                .flat_map(|(s, m)| self.from(b, m).into_iter().map(move |o| (s, o)))
                .collect(),
            Path::Alternative(a, b) => {
                let mut pairs = self.open(a);
                pairs.extend(self.open(b));
                dedup_pairs(pairs)
            }
            Path::OneOrMore(p) => {
                let adjacency = Adjacency::new(self.open(p));
                let starts = adjacency.sources().to_vec();
                pairs_of(closure(&adjacency, starts, false))
            }
            Path::ZeroOrMore(p) => {
                let adjacency = Adjacency::new(self.open(p));
                pairs_of(closure(&adjacency, self.nodes(), true))
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
