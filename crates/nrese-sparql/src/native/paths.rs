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
//!
//! What the path's text multiplies draws from the query's budget where it grows
//! ([`PathEvaluator::either`]): an alternative takes its arms one at a time, its union
//! charged before it grows, and under a set-valued path only the arms' support, each pair
//! once as they come (the executor memory stack's review r1, EMS-R1: 64 distinct arms over
//! the same statements once held 64 bags at once). A refused charge stops the evaluator as a
//! cancellation does, and its entry points report it ([`PathEvaluator::ends`] and the others).

use std::cell::{Cell, RefCell};

use nrese_engine::quad::Permutation;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::graph::{
    Adjacency, closure, closure_sizes_until, reachable, transitive_closure_until,
};
use nrese_exec::{Budget, BudgetExceeded, Grant};
use nrese_sparql_syntax::algebra::PropertyPathExpression;

/// A path with its IRIs resolved to ids; `None` marks an IRI the store doesn't know (no
/// edges). Equal paths have equal bags ([`PathEvaluator::either`]).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum Path {
    Link(Option<u64>),
    Reverse(Box<Path>),
    /// The steps in order (ADR-0012: a list, walked in a loop however long).
    Sequence(Vec<Path>),
    /// The arms, each once with how often the text names it: an arm named again is the same
    /// bag again, found once here rather than at every node a traversal reaches (the
    /// executor memory stack's path counts, EMS-15: per-node arm matching cost the closure
    /// over an alternative +6 % instructions against engine-v2).
    Alternative(Vec<(Path, usize)>),
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
            // Each arm once, in the order it first comes, found by hash: matching pairwise
            // made 65,536 distinct arms 2.1 billion comparisons (sol-loop r3 S1 of EMS-R1).
            PropertyPathExpression::Alternative(paths) => {
                let mut arms: Vec<(Self, usize)> = Vec::with_capacity(paths.len());
                let mut at: std::collections::HashMap<Self, usize> =
                    std::collections::HashMap::with_capacity(paths.len());
                for path in paths {
                    let path = Self::resolve(path, snapshot);
                    match at.get(&path) {
                        Some(&i) => arms[i].1 += 1,
                        None => {
                            at.insert(path.clone(), arms.len());
                            arms.push((path, 1));
                        }
                    }
                }
                Self::Alternative(arms)
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

    /// Room for `additional` more values.
    fn reserve(&mut self, additional: usize) {
        self.counted.reserve_exact(additional);
        self.at.reserve(additional);
    }

    /// The bytes it holds.
    fn bytes(&self) -> usize {
        Self::bytes_for(self.counted.capacity(), self.at.capacity())
    }

    /// The bytes it holds with room for `additional` more values.
    fn bytes_with(&self, additional: usize) -> usize {
        let wanted = self.counted.len().saturating_add(additional);
        let counted = self.counted.capacity().max(wanted);
        Self::bytes_for(counted, self.at.capacity().max(wanted))
    }

    /// The bytes of room for `counted` values and an index of `at`: the index's an estimate
    /// of a hash table's (a bucket and a control byte per entry, buckets a power of two at
    /// least 8/7 of the room).
    fn bytes_for(counted: usize, at: usize) -> usize {
        let entry = std::mem::size_of::<(T, usize)>();
        let buckets = match at {
            0 => 0,
            at => (at.saturating_mul(8) / 7).max(1).next_power_of_two(),
        };
        counted
            .saturating_mul(entry)
            .saturating_add(buckets.saturating_mul(entry + 1))
    }
}

/// The bytes of `capacity` values of a vector.
fn bytes_of<T>(capacity: usize) -> usize {
    capacity.saturating_mul(std::mem::size_of::<T>())
}

/// The most a vector of `capacity` values holds while it makes room for `wanted`: its old
/// room and the new together while it moves.
fn moved<T>(capacity: usize, wanted: usize) -> usize {
    match wanted > capacity {
        true => bytes_of::<T>(capacity).saturating_add(bytes_of::<T>(wanted)),
        false => bytes_of::<T>(capacity),
    }
}

/// An alternative's bags put together as its arms arrive, one at a time
/// ([`PathEvaluator::either`]).
enum Union<T> {
    /// Every arm so far copies, and named once: their copies, concatenated.
    Copies(Vec<T>),
    /// Each value once with its copies.
    Counted(Tally<T>),
    /// The arms' values, for a set-valued path, deduplicated whenever an arm that isn't the
    /// last leaves more than twice `distinct` (the first arm's, or what the last
    /// deduplication left): at most twice the support and one arm.
    Support { values: Vec<T>, distinct: usize },
}

impl<T: Copy + Eq + std::hash::Hash + Ord> Union<T> {
    fn new(support: bool) -> Self {
        match support {
            true => Self::Support {
                values: Vec::new(),
                distinct: 0,
            },
            false => Self::Copies(Vec::new()),
        }
    }

    /// The most it holds, beside `arm`, while [`Self::add`] makes room for `arm`, named
    /// `times`: a vector that grows moves, its old room beside the new until the move ends,
    /// as the budget owner's `ChargedVec` charges it (sol-loop r1 S3 of EMS-R1); a first arm
    /// of copies becomes the union, nothing beside it.
    fn peak_with(&self, arm: &Bag<T>, times: usize) -> usize {
        let room = |values: &Vec<T>| match (values.capacity(), arm) {
            (0, Bag::Copies(_)) => 0,
            (capacity, _) => moved::<T>(capacity, values.len().saturating_add(arm.entries())),
        };
        match self {
            Self::Copies(values) if times == 1 && matches!(arm, Bag::Copies(_)) => room(values),
            // Counted from here on: the copies beside the tally they go into.
            Self::Copies(values) => {
                let wanted = values.len().saturating_add(arm.entries());
                let tally = Tally::<T>::bytes_for(wanted, wanted);
                bytes_of::<T>(values.capacity()).saturating_add(tally)
            }
            Self::Counted(tally) => match (tally.bytes(), tally.bytes_with(arm.entries())) {
                (now, then) if then > now => now.saturating_add(then),
                (now, _) => now,
            },
            Self::Support { values, .. } => room(values),
        }
    }

    /// `arm`, named `times`, added (`last`: no arm after it).
    fn add(&mut self, arm: Bag<T>, times: usize, last: bool) {
        if let Self::Copies(values) = self
            && (times > 1 || matches!(arm, Bag::Counted(_)))
        {
            let mut tally = Tally::new();
            tally.reserve(values.len().saturating_add(arm.entries()));
            for &value in values.iter() {
                tally.add(value, 1);
            }
            *self = Self::Counted(tally);
        }
        match self {
            // The first arm's vector becomes the union, not a copy of it.
            Self::Copies(values) if values.capacity() == 0 => *values = arm.values(),
            Self::Copies(values) => {
                values.reserve_exact(arm.entries());
                values.extend(arm.values());
            }
            Self::Counted(tally) => {
                tally.reserve(arm.entries());
                for (value, copies) in arm.counted() {
                    tally.add(value, copies.saturating_mul(times));
                }
            }
            Self::Support { values, .. } => {
                if values.capacity() == 0 {
                    *values = arm.values();
                } else {
                    values.reserve_exact(arm.entries());
                    values.extend(arm.values());
                }
                self.settle(last);
            }
        }
    }

    /// The vector an arm named `times` can be streamed into, value by value, without a
    /// vector of its own ([`PathEvaluator::stream`]): the copies' (named once) or the
    /// support's.
    fn streamed(&mut self, times: usize) -> Option<&mut Vec<T>> {
        match self {
            Self::Copies(values) if times == 1 => Some(values),
            Self::Support { values, .. } => Some(values),
            _ => None,
        }
    }

    /// A support after an arm (`last`: no arm after it): deduplicated once more than twice
    /// what the last deduplication left (the first arm's count stands for it).
    fn settle(&mut self, last: bool) {
        if let Self::Support { values, distinct } = self {
            if *distinct == 0 {
                *distinct = values.len();
            } else if !last && values.len() > distinct.saturating_mul(2) {
                values.sort_unstable();
                values.dedup();
                *distinct = values.len();
            }
        }
    }

    fn finish(self) -> Bag<T> {
        match self {
            Self::Copies(values) | Self::Support { values, .. } => Bag::Copies(values),
            Self::Counted(tally) => tally.finish(),
        }
    }
}

/// A sequence step's ends: pushed as they come, a copy each, unless what it starts from is
/// counted already; then each end once with how many ways ([`Tally`]). A step whose copies
/// outnumber the entries it was given is counted when it ends ([`Self::finish`]): ways that
/// grow, by the path's text (an alternative) or by the data (a node's several ends), are
/// the only ones that compound over the next steps (`(p?/…/p?)+` over a cycle: 2^n copies),
/// and a step that doesn't grow never gives more copies than it got. Counting hashes every
/// end: a sequence whose steps give one end an entry (`author/city`) needs none of it (the
/// executor memory stack's timing: counting every step cost path alternatives x1.56).
enum Ends<T> {
    Copies(Vec<T>),
    Counted(Tally<T>),
}

impl<T: Copy + Eq + std::hash::Hash> Ends<T> {
    fn new(counted: bool) -> Self {
        match counted {
            true => Self::Counted(Tally::new()),
            false => Self::Copies(Vec::new()),
        }
    }

    fn add(&mut self, value: T, copies: usize) {
        match self {
            Self::Copies(values) if copies == 1 => values.push(value),
            Self::Copies(values) => {
                let mut tally = Tally::new();
                for &value in values.iter() {
                    tally.add(value, 1);
                }
                tally.add(value, copies);
                *self = Self::Counted(tally);
            }
            Self::Counted(tally) => tally.add(value, copies),
        }
    }

    /// The step's bag, given the number of entries it started from: copies that outnumber
    /// them are counted.
    fn finish(self, entries: usize) -> Bag<T> {
        match self {
            Self::Copies(values) if values.len() > entries => {
                let mut tally = Tally::new();
                for value in values {
                    tally.add(value, 1);
                }
                tally.finish()
            }
            Self::Copies(values) => Bag::Copies(values),
            Self::Counted(tally) => tally.finish(),
        }
    }
}

impl<T> Bag<T> {
    /// How many entries the bag holds: its copies, or its counted values.
    fn entries(&self) -> usize {
        match self {
            Self::Copies(values) => values.len(),
            Self::Counted(counted) => counted.len(),
        }
    }

    /// The bytes it holds.
    fn bytes(&self) -> usize {
        match self {
            Self::Copies(values) => bytes_of::<T>(values.capacity()),
            Self::Counted(counted) => bytes_of::<(T, usize)>(counted.capacity()),
        }
    }

    /// Its copies, if it holds copies.
    fn copies_entries(&self) -> usize {
        match self {
            Self::Copies(values) => values.len(),
            Self::Counted(_) => 0,
        }
    }

    /// Its counted values, if it holds counted ones.
    fn counted_entries(&self) -> usize {
        match self {
            Self::Copies(_) => 0,
            Self::Counted(counted) => counted.len(),
        }
    }
}

/// A vector's capacity once it has room for `additional` more values, at least doubling
/// where it grows, and the most its room holds meanwhile: the old beside the new while it
/// moves.
fn grown<T>(values: &Vec<T>, additional: usize) -> (usize, usize) {
    let capacity = values.capacity();
    let wanted = values.len().saturating_add(additional);
    if wanted <= capacity {
        return (capacity, bytes_of::<T>(capacity));
    }
    let to = wanted.max(capacity.saturating_mul(2));
    let peak = bytes_of::<T>(capacity).saturating_add(bytes_of::<T>(to));
    (to, peak)
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
    pub(crate) closures: Cell<usize>,
    /// What the alternatives being put together hold, charged to the query's budget before
    /// they grow ([`Self::hold`]): every byte, from a [`Grant`]'s spare, so the small bags
    /// of a traversal's nodes cost no budget call. Its spare is the budget owner's rule
    /// ([`Grant::new`]: the part's cap over the pool's workers, none under an envelope's
    /// 1 KiB floor), so evaluators in parallel workers (EXISTS) hold one cap between them.
    scratch: RefCell<Grant<&'a Budget>>,
    /// The first charge the budget refused: it stops the evaluator as a cancellation does,
    /// and the entry points report it ([`Self::answer`]).
    refused: Cell<Option<BudgetExceeded>>,
}

impl<'a> PathEvaluator<'a> {
    /// An evaluator of paths in `graph` whose scratch draws from `budget`.
    pub(crate) fn new(
        snapshot: &'a Snapshot,
        model: ReadModel,
        graph: PathGraph<'a>,
        cancellation: Option<&'a crate::CancellationToken>,
        budget: &'a Budget,
    ) -> Self {
        Self {
            snapshot,
            model,
            graph,
            fixed: None,
            cancellation,
            closures: Cell::new(0),
            scratch: RefCell::new(Grant::new(budget)),
            refused: Cell::new(None),
        }
    }
}

impl PathEvaluator<'_> {
    /// Cancelled, or refused a charge.
    fn stopped(&self) -> bool {
        self.refused.get().is_some()
            || self
                .cancellation
                .is_some_and(crate::CancellationToken::is_cancelled)
    }

    /// Holds `bytes` of scratch where `held` were held: the difference charged, or given
    /// back. False if the budget refused the charge (recorded: the evaluator stops), `held`
    /// then unchanged.
    fn hold(&self, held: &mut usize, bytes: usize) -> bool {
        if bytes == *held {
            return true;
        }
        let mut scratch = self.scratch.borrow_mut();
        if bytes > *held {
            if let Err(refused) = scratch.take(bytes - *held) {
                if self.refused.get().is_none() {
                    self.refused.set(Some(refused));
                }
                return false;
            }
        } else {
            scratch.give_back(*held - bytes);
        }
        *held = bytes;
        true
    }

    /// `value`, unless a charge was refused while it was made: then what a stopped traversal
    /// returned, never an answer.
    fn answer<R>(&self, value: R) -> Result<R, BudgetExceeded> {
        match self.refused.get() {
            Some(refused) => Err(refused),
            None => Ok(value),
        }
    }

    /// [`transitive_closure_until`] stopped with the query: then empty. None of edges a
    /// refused charge cut short (sol-loop r1 S2 of EMS-R1): they are no path's, and their
    /// closure would be built for nothing (no charge is taken while it runs).
    fn closure_of(&self, edges: &[(u64, u64)]) -> Vec<(u64, u64)> {
        if self.stopped() {
            return Vec::new();
        }
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
            out.extend(self.support_from(path, node));
        }
    }

    /// [`Self::step_from`] backwards.
    fn step_to(&self, path: &Path, node: u64, out: &mut Vec<u64>) {
        if !self.stopped() {
            out.extend(self.support_to(path, node));
        }
    }

    /// The ends of `path` from `node`, for a set-valued path (a search's step, `p?`): each
    /// at least once, an alternative's put together as a support ([`Self::either`]).
    fn support_from(&self, path: &Path, node: u64) -> Vec<u64> {
        match path {
            Path::Alternative(paths) => self
                .either(
                    paths,
                    true,
                    |p| Bag::Copies(self.support_from(p, node)),
                    |p, held, values| self.stream(p, held, values, Some(node), None, |(_, _, o)| o),
                )
                .values(),
            Path::Reverse(p) => self.support_to(p, node),
            _ => self.from(path, node).values(),
        }
    }

    /// [`Self::support_from`] backwards: the starts reaching `node`.
    fn support_to(&self, path: &Path, node: u64) -> Vec<u64> {
        match path {
            Path::Alternative(paths) => self
                .either(
                    paths,
                    true,
                    |p| Bag::Copies(self.support_to(p, node)),
                    |p, held, values| self.stream(p, held, values, None, Some(node), |(s, _, _)| s),
                )
                .values(),
            Path::Reverse(p) => self.support_from(p, node),
            _ => self.to(path, node).values(),
        }
    }

    /// The pairs of `path`, for a set-valued path (a closure's edges, `p?`): each at least
    /// once, an alternative's put together as a support ([`Self::either`]).
    fn support(&self, path: &Path) -> Vec<(u64, u64)> {
        match path {
            Path::Alternative(paths) => self
                .either(
                    paths,
                    true,
                    |p| Bag::Copies(self.support(p)),
                    |p, held, values| self.stream(p, held, values, None, None, |(s, _, o)| (s, o)),
                )
                .values(),
            Path::Reverse(p) => {
                let mut pairs = self.support(p);
                for pair in &mut pairs {
                    *pair = (pair.1, pair.0);
                }
                pairs
            }
            _ => self.open(path).values(),
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
    /// `None` for other paths, and once cancelled.
    pub(crate) fn count_open(&self, path: &Path) -> Result<Option<u64>, BudgetExceeded> {
        let (Path::OneOrMore(step) | Path::ZeroOrMore(step)) = path else {
            return Ok(None);
        };
        self.closures.set(self.closures.get() + 1);
        let cancellation = self.cancellation;
        let stop = move || cancellation.is_some_and(crate::CancellationToken::is_cancelled);
        let edges = self.answer(self.edges(step))?;
        let Some(sizes) = closure_sizes_until(&edges, &stop) else {
            return Ok(None);
        };
        if matches!(path, Path::OneOrMore(_)) {
            return Ok(Some(sizes.iter().map(|&(_, reached, _)| reached).sum()));
        }
        // Every node with itself, plus what each start reaches besides itself. The starts
        // are nodes: their pairs come from the graph's statements.
        let beyond: u64 = sizes
            .iter()
            .map(|&(_, reached, on_cycle)| reached - u64::from(on_cycle))
            .sum();
        Ok(Some(self.node_count() + beyond))
    }

    /// How many nodes the closure `path` (`p+` or `p*`) reaches from `node` (`forward`) or
    /// how many reach it, if that search expands at most `budget` nodes: the planner's
    /// estimate of a closure from a constant, exact where it is small. `None` past the
    /// budget, for other paths, and where the query's budget refused the search's scratch.
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
        (expanded.get() <= budget && self.refused.get().is_none()).then_some(reached.len())
    }

    /// Ends reachable from `start` (a bag), or the charge refused while they were found.
    pub(crate) fn ends(&self, path: &Path, start: u64) -> Result<Bag<u64>, BudgetExceeded> {
        self.answer(self.from(path, start))
    }

    /// Starts that reach `end` (a bag), or the charge refused while they were found.
    pub(crate) fn starts(&self, path: &Path, end: u64) -> Result<Bag<u64>, BudgetExceeded> {
        self.answer(self.to(path, end))
    }

    /// Every `(start, end)` pair (a bag), or the charge refused while they were found.
    pub(crate) fn pairs(&self, path: &Path) -> Result<Bag<(u64, u64)>, BudgetExceeded> {
        self.answer(self.open(path))
    }

    /// How many solutions `start path end` has ([`Self::multiplicity`]), or the charge
    /// refused while they were counted.
    pub(crate) fn solutions(
        &self,
        path: &Path,
        start: u64,
        end: u64,
    ) -> Result<usize, BudgetExceeded> {
        self.answer(self.multiplicity(path, start, end))
    }

    /// Ends reachable from `start` (a bag).
    fn from(&self, path: &Path, start: u64) -> Bag<u64> {
        let copies = match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(Some(start), Some(*p), None)
                .map(|(_, _, o)| o)
                .collect(),
            Path::Reverse(p) => return self.to(p, start),
            Path::Sequence(steps) => return self.through(steps, start, true),
            Path::Alternative(paths) => {
                return self.either(
                    paths,
                    false,
                    |p| self.from(p, start),
                    |p, held, values| {
                        self.stream(p, held, values, Some(start), None, |(_, _, o)| o)
                    },
                );
            }
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
                ends.extend(self.support_from(p, start));
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
            let entries = nodes.entries();
            let mut next = Ends::new(matches!(nodes, Bag::Counted(_)));
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
            nodes = next.finish(entries);
        }
        nodes
    }

    /// The bags of `paths` together (a UNION): copies stay copies, and counted ones are
    /// added up with them; for a set-valued path (`support`), the arms' values, each at
    /// least once.
    /// An arm the alternative names again is the same bag again ([`Path::Alternative`]:
    /// matched when the path is resolved): it is followed once and its values counted as
    /// often, never copied, so `p|p|…` costs and holds what `p` does however often the
    /// path's text repeats it.
    /// The arms come one at a time ([`Union`]). A leaf arm (a link, a negated set) streams
    /// its values into the union, charged as the union grows ([`Self::stream`]), with no
    /// vector of its own, as engine-v2's `flat_map` did (the executor memory stack's path
    /// counts, EMS-15: an arm's vector copied into the union cost a closure over an
    /// alternative +38 M instructions a run). Any other arm arrives as its bag, held with the
    /// union's room for it in one charge before the union grows (the first arm of copies
    /// becomes the union, not a copy). Distinct arms with overlapping bags (`!(q)|!(q|q)|…`)
    /// so hold the union and one arm, and under a set-valued path the union at most twice
    /// their support (the package review r1 of the executor memory stack, EMS-R1). A
    /// refused charge stops here with what was put together, which the entry points never
    /// answer ([`Self::answer`]). What it returns is no longer charged: its consumer charges
    /// it (an enclosing alternative as it arrives, the path's table before it is made).
    fn either<T: Copy + Eq + std::hash::Hash + Ord>(
        &self,
        arms: &[(Path, usize)],
        support: bool,
        bag: impl Fn(&Path) -> Bag<T>,
        stream: impl Fn(&Path, &mut usize, &mut Vec<T>) -> Option<bool>,
    ) -> Bag<T> {
        let mut union = Union::new(support);
        let mut held = 0;
        for (i, (arm, times)) in arms.iter().enumerate() {
            let (times, last) = (*times, i + 1 == arms.len());
            if self.stopped() {
                break;
            }
            let streamed = match union.streamed(times) {
                Some(values) => stream(arm, &mut held, values),
                None => None,
            };
            match streamed {
                Some(true) => {
                    union.settle(last);
                    continue;
                }
                Some(false) => break,
                None => {}
            }
            let arm = bag(arm);
            // The arm as it arrives and the union's room for it, held in one charge (what
            // the merge frees stays held until the next arm's charge or the end).
            let peak = union.peak_with(&arm, times).saturating_add(arm.bytes());
            if !self.hold(&mut held, peak) {
                break;
            }
            union.add(arm, times, last);
        }
        self.hold(&mut held, 0);
        union.finish()
    }

    /// A leaf arm's values pushed into `values` (an alternative's union, [`Self::either`]):
    /// the statements `subject ? object`, bound where given, `value` making each value of
    /// a statement. `None` for any other path; false if the union's growth was refused.
    fn stream<T>(
        &self,
        path: &Path,
        held: &mut usize,
        values: &mut Vec<T>,
        subject: Option<u64>,
        object: Option<u64>,
        value: impl Fn((u64, u64, u64)) -> T,
    ) -> Option<bool> {
        match path {
            Path::Link(None) => Some(true),
            Path::Link(Some(p)) => {
                let statements = self.quads(subject, Some(*p), object);
                Some(self.push_all(held, values, statements.map(value)))
            }
            Path::Negated(excluded) => {
                let statements = self.quads(subject, None, object);
                let kept = statements.filter(|(_, p, _)| !excluded.contains(p));
                Some(self.push_all(held, values, kept.map(value)))
            }
            _ => None,
        }
    }

    /// `items` pushed into `values`, its room held in `held` before each growth
    /// ([`Self::room`], doubling). False if refused: then the items so far.
    fn push_all<T>(
        &self,
        held: &mut usize,
        values: &mut Vec<T>,
        items: impl Iterator<Item = T>,
    ) -> bool {
        for item in items {
            if values.len() == values.capacity() && !self.room(held, values, 1, true) {
                return false;
            }
            values.push(item);
        }
        true
    }

    /// Starts that reach `end` (a bag).
    fn to(&self, path: &Path, end: u64) -> Bag<u64> {
        let copies = match path {
            Path::Link(None) => Vec::new(),
            Path::Link(Some(p)) => self
                .quads(None, Some(*p), Some(end))
                .map(|(s, _, _)| s)
                .collect(),
            Path::Reverse(p) => return self.from(p, end),
            Path::Sequence(steps) => return self.through(steps, end, false),
            Path::Alternative(paths) => {
                return self.either(
                    paths,
                    false,
                    |p| self.to(p, end),
                    |p, held, values| self.stream(p, held, values, None, Some(end), |(s, _, _)| s),
                );
            }
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
                starts.extend(self.support_to(p, end));
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
    fn multiplicity(&self, path: &Path, start: u64, end: u64) -> usize {
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
            Path::Alternative(arms) => arms
                .iter()
                .map(|(p, times)| self.multiplicity(p, start, end).saturating_mul(*times))
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
    fn connects(&self, path: &Path, start: u64, end: u64) -> bool {
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
            Path::Alternative(arms) => arms.iter().any(|(p, _)| self.connects(p, start, end)),
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
    /// values), or the charge refused while they were found.
    pub(crate) fn reached_from(
        &self,
        path: &Path,
        starts: &[u64],
    ) -> Result<Bag<(u64, u64)>, BudgetExceeded> {
        let pairs = match path {
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
        };
        self.answer(pairs)
    }

    /// The `(start, end)` pairs of [`Self::open`] whose end is one of `ends` (distinct
    /// values), or the charge refused while they were found.
    pub(crate) fn reaching(
        &self,
        path: &Path,
        ends: &[u64],
    ) -> Result<Bag<(u64, u64)>, BudgetExceeded> {
        let pairs = match path {
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
        };
        self.answer(pairs)
    }

    /// The pairs of each of `nodes` (distinct) with each value of its bag, `pair` making a
    /// `(start, end)` pair of the node and the value: copies while every bag is copies.
    /// Each node's bag and the pairs' room for it are held in one charge before the pairs
    /// grow (doubling; the old room beside the new while a vector moves), as an
    /// alternative's union is ([`Self::either`]): the bags of 64 distinct arms from 4,096
    /// bound values are 64 pairs a node (sol-loop r1 S1 of EMS-R1). What it returns is no
    /// longer charged: the path's table charges it.
    fn pairs_at(
        &self,
        nodes: &[u64],
        bag: impl Fn(u64) -> Bag<u64>,
        pair: impl Fn(u64, u64) -> (u64, u64),
    ) -> Bag<(u64, u64)> {
        let (mut copies, mut counted) = (Vec::new(), Vec::new());
        let mut held = 0;
        for &node in nodes.iter().take_while(|_| !self.stopped()) {
            let bag = bag(node);
            let (copies_to, copies_peak) = grown(&copies, bag.copies_entries());
            let (counted_to, counted_peak) = grown(&counted, bag.counted_entries());
            let peak = copies_peak
                .saturating_add(counted_peak)
                .saturating_add(bag.bytes());
            if !self.hold(&mut held, peak) {
                break;
            }
            match bag {
                Bag::Copies(values) => {
                    copies.reserve_exact(copies_to - copies.len());
                    copies.extend(values.into_iter().map(|v| pair(node, v)));
                }
                Bag::Counted(values) => {
                    counted.reserve_exact(counted_to - counted.len());
                    counted.extend(values.into_iter().map(|(v, n)| (pair(node, v), n)));
                }
            }
        }
        let pairs = match counted.is_empty() {
            true => Bag::Copies(copies),
            false => {
                let peak = moved::<((u64, u64), usize)>(
                    counted.capacity(),
                    counted.len().saturating_add(copies.len()),
                )
                .saturating_add(bytes_of::<(u64, u64)>(copies.capacity()));
                if self.hold(&mut held, peak) {
                    counted.reserve_exact(copies.len());
                    counted.extend(copies.into_iter().map(|pair| (pair, 1)));
                }
                Bag::Counted(counted)
            }
        };
        self.hold(&mut held, 0);
        pairs
    }

    /// Room for `additional` more values in `values`, its bytes held in `held` first: the
    /// old room and the new together while the vector moves, then the new. `doubling`: at
    /// least twice the old room, for a vector grown a bag at a time. False if refused.
    fn room<T>(
        &self,
        held: &mut usize,
        values: &mut Vec<T>,
        additional: usize,
        doubling: bool,
    ) -> bool {
        let wanted = values.len().saturating_add(additional);
        if wanted <= values.capacity() {
            return true;
        }
        let wanted = match doubling {
            true => wanted.max(values.capacity().saturating_mul(2)),
            false => wanted,
        };
        if !self.hold(held, moved::<T>(values.capacity(), wanted)) {
            return false;
        }
        values.reserve_exact(wanted - values.len());
        self.hold(held, bytes_of::<T>(values.capacity()))
    }

    /// The pairs of `path` as edges for a closure: each at least once ([`Self::support`]),
    /// which is all a closure's reachability needs.
    fn edges(&self, path: &Path) -> Vec<(u64, u64)> {
        self.support(path)
    }

    /// Every `(start, end)` pair.
    fn open(&self, path: &Path) -> Bag<(u64, u64)> {
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
                    let entries = pairs.entries();
                    let mut next = Ends::new(matches!(pairs, Bag::Counted(_)));
                    for ((s, m), ways) in pairs.counted() {
                        if self.stopped() {
                            break;
                        }
                        for (o, more) in self.from(step, m).counted() {
                            next.add((s, o), ways.saturating_mul(more));
                        }
                    }
                    pairs = next.finish(entries);
                }
                return pairs;
            }
            Path::Alternative(paths) => {
                return self.either(
                    paths,
                    false,
                    |p| self.open(p),
                    |p, held, values| self.stream(p, held, values, None, None, |(s, _, o)| (s, o)),
                );
            }
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
                pairs.extend(self.support(p));
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
        let budget = Budget::unlimited();
        let evaluator = PathEvaluator::new(
            &snapshot,
            ReadModel::default(),
            PathGraph::Default,
            Some(&token),
            &budget,
        );
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
        assert_eq!(evaluator.count_open(&plus), Ok(None));
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
        let budget = Budget::unlimited();
        let evaluator = PathEvaluator::new(
            &snapshot,
            ReadModel::default(),
            PathGraph::Default,
            None,
            &budget,
        );
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
        assert_eq!(
            pairs(evaluator.reached_from(&sequence, &[b]).unwrap()),
            vec![(b, b)]
        );
        assert_eq!(
            pairs(evaluator.reaching(&sequence, &[c]).unwrap()),
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

    /// Guard (the executor memory stack's timing, notes/2026-11-02-exec-memory-stack.md
    /// EMS-11: counting every sequence's ends cost path alternatives x1.56): a sequence counts
    /// the ways through a node only where they can add up: a step with an alternative, a
    /// counted input, or a step that gives more ends than it was given. A sequence whose steps
    /// give each entry one end keeps copies, from a node and for every pair, and so does an
    /// alternative of such sequences; a sequence with an alternative step counts.
    #[test]
    fn only_a_step_that_can_multiply_ways_counts_them() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let [a, b, c, p, q] = ["a", "b", "c", "p", "q"]
            .map(|name| NamedNode::new_unchecked(format!("http://e/{name}")));
        let mut tx = engine.transaction();
        // A chain: every node one `p` on, and `a q b`, `b q a` beside it.
        for (s, pr, o) in [(&a, &p, &b), (&a, &q, &b), (&b, &p, &c), (&b, &q, &a)] {
            let quad = Quad::new(s.clone(), pr.clone(), o.clone(), GraphName::DefaultGraph);
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let link = |n: &NamedNode| PropertyPathExpression::NamedNode(n.clone());
        let plain = PropertyPathExpression::Sequence(vec![link(&p), link(&p)]);
        let either = PropertyPathExpression::Alternative(vec![
            plain.clone(),
            PropertyPathExpression::Sequence(vec![link(&q), link(&p)]),
        ]);
        let stepped = PropertyPathExpression::Sequence(vec![
            link(&p),
            PropertyPathExpression::Alternative(vec![link(&p), link(&q)]),
        ]);
        let [plain, either, stepped] =
            [plain, either, stepped].map(|path| Path::resolve(&path, &snapshot));
        let budget = Budget::unlimited();
        let evaluator = PathEvaluator::new(
            &snapshot,
            ReadModel::default(),
            PathGraph::Default,
            None,
            &budget,
        );
        let a = snapshot.lookup(a.as_ref().into()).unwrap().raw();
        assert!(matches!(evaluator.from(&plain, a), Bag::Copies(_)));
        assert!(matches!(evaluator.open(&plain), Bag::Copies(_)));
        assert!(matches!(evaluator.open(&either), Bag::Copies(_)));
        // From `b` the alternative step gives two ends for one: counted.
        assert!(matches!(evaluator.from(&stepped, a), Bag::Counted(_)));
    }

    /// Spec (PR #85 N2; sol-loop r3 S1 of the executor memory stack): a sequence's bag holds
    /// each end once with how many ways lead there, whatever multiplies the ways: the path's
    /// text or the data. Over a two-node cycle (`a p b`, `b p a`), 20 `p?` steps from `a` reach
    /// `a` and `b` 2^19 ways each: two entries, never 2^20 copies; the closure of such a
    /// sequence answers its two nodes (SPARQL 1.1 §18.4: a sequence is a join, `+` a set).
    #[test]
    fn ways_the_data_multiplies_are_counted_not_copied() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let [a, b, p] =
            ["a", "b", "p"].map(|name| NamedNode::new_unchecked(format!("http://e/{name}")));
        let mut tx = engine.transaction();
        for (s, o) in [(&a, &b), (&b, &a)] {
            let quad = Quad::new(s.clone(), p.clone(), o.clone(), GraphName::DefaultGraph);
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let maybe = PropertyPathExpression::ZeroOrOne(Box::new(PropertyPathExpression::NamedNode(
            p.clone(),
        )));
        let steps = PropertyPathExpression::Sequence(vec![maybe; 20]);
        let plus = Path::resolve(
            &PropertyPathExpression::OneOrMore(Box::new(steps.clone())),
            &snapshot,
        );
        let steps = Path::resolve(&steps, &snapshot);
        let budget = Budget::unlimited();
        let evaluator = PathEvaluator::new(
            &snapshot,
            ReadModel::default(),
            PathGraph::Default,
            None,
            &budget,
        );
        let [a, b] = [&a, &b].map(|n| snapshot.lookup(n.as_ref().into()).unwrap().raw());
        let bag = evaluator.from(&steps, a);
        assert_eq!(bag.len(), 1 << 20, "the ways: the sequence's solutions");
        let entries = match &bag {
            Bag::Copies(values) => values.len(),
            Bag::Counted(counted) => counted.len(),
        };
        assert_eq!(entries, 2, "each end once, not a copy per way");
        // Every pair: (a, a), (a, b), (b, a), (b, b), 2^19 ways each.
        let pairs = evaluator.open(&steps);
        assert_eq!(pairs.len(), 1 << 21, "the ways of every pair");
        let entries = match &pairs {
            Bag::Copies(values) => values.len(),
            Bag::Counted(counted) => counted.len(),
        };
        assert_eq!(entries, 4, "each pair once, not a copy per way");
        let mut reached = evaluator.from(&plus, a).into_copies();
        reached.sort_unstable();
        assert_eq!(reached, vec![a.min(b), a.max(b)]);
    }

    /// Spec (sol-loop r1 S2 of EMS-R1): a charge the budget refused stops the evaluator, and
    /// what it found so far is no answer, so no closure is built of it. Over 1,000 self-loops
    /// and `m q m`, the arms `!(q)`, `!(q|q)`, `!(q|q|q)` under `+` under a 24 KiB budget: the
    /// first arm's 1,000 pairs (16 KB) and the union's room for them don't fit together, so
    /// the pairs are refused and no closure is computed.
    #[test]
    fn a_refused_alternative_builds_no_closure() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let [p, q, m] =
            ["p", "q", "m"].map(|name| NamedNode::new_unchecked(format!("http://e/{name}")));
        let mut tx = engine.transaction();
        for i in 0..1000 {
            let quad = Quad::new(node(i), p.clone(), node(i), GraphName::DefaultGraph);
            tx.insert(quad.as_ref());
        }
        let marker = Quad::new(m.clone(), q.clone(), m, GraphName::DefaultGraph);
        tx.insert(marker.as_ref());
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let arms = (1..=3)
            .map(|i| PropertyPathExpression::NegatedPropertySet(vec![q.clone(); i]))
            .collect();
        let either = PropertyPathExpression::Alternative(arms);
        let plus = Path::resolve(
            &PropertyPathExpression::OneOrMore(Box::new(either)),
            &snapshot,
        );
        let budget = Budget::new(24 << 10);
        let evaluator = PathEvaluator::new(
            &snapshot,
            ReadModel::default(),
            PathGraph::Default,
            None,
            &budget,
        );
        assert!(evaluator.pairs(&plus).is_err(), "the pairs are refused");
        assert_eq!(evaluator.closures.get(), 0, "no closure of refused pairs");
    }
}
