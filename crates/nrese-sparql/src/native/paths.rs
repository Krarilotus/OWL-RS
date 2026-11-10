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
//! Every buffer a path's evaluation makes (its bags, an alternative's union, a sequence
//! step's ends and their tally, the pairs from bound values, the answer's columns) is a
//! [`Buf`] or a [`Map`]: its memory is charged to the query's budget, through the evaluator's
//! [`Pool`], before it is allocated, the charge moves with it, and it is freed before its
//! charge is released. No code here counts bytes: the charge is the buffer's (the executor
//! memory stack's second lead, 8 Nov: a charge kept beside its vector by hand disagreed with
//! it at one more site in each of six review rounds). What the path's text multiplies is
//! held down by the algorithm: an alternative takes its arms one at a time, under a
//! set-valued path only their support (EMS-R1), and a sequence step counts its ends once
//! they would outnumber its entries (EMS-16). A refused charge stops the evaluator as a
//! cancellation does, and its entry points report it ([`PathEvaluator::ends`] and the others).

use std::cell::Cell;
use std::sync::Arc;

use nrese_engine::quad::Permutation;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::graph::{
    Adjacency, Probes, closure_sizes_within, closure_within, reachable_within,
    transitive_closure_within,
};
use nrese_exec::{Budget, BudgetExceeded, Charged, ChargedMap, Pool, Share, Stop};
use nrese_sparql_syntax::algebra::PropertyPathExpression;

/// A buffer of a path's evaluation: charged to the query's budget through the evaluator's
/// pool while it lives ([`Charged`]).
pub(crate) type Buf<'e, T> = Charged<T, Share<'e>>;

/// A hash map of a path's evaluation, charged as a [`Buf`] is ([`ChargedMap`]).
type Map<'e, K, V> = ChargedMap<K, V, Share<'e>>;

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

/// A path's solutions from a node, or all of them: a bag (an alternative is a UNION, a
/// sequence a join). A sequence of alternatives multiplies the ways through a node: over
/// one statement `a p a`, `(p|p)/(p|p)/…` reaches `a` 2^n ways in n steps, so a sequence's
/// bag holds each value once with how often it occurs, never the copies, whose number grows
/// with the path's text. Only the caller that charges the copies ([`Self::into_copies`])
/// makes them.
pub(crate) enum Bag<'e, T> {
    /// One entry per copy.
    Copies(Buf<'e, T>),
    /// Each entry with how many copies it stands for (saturating).
    Counted(Buf<'e, (T, usize)>),
}

impl<'e, T: Copy> Bag<'e, T> {
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
        let copies = copies
            .into_iter()
            .flat_map(|v| v.iter())
            .map(|&value| (value, 1));
        copies.chain(counted.into_iter().flat_map(|c| c.iter()).copied())
    }

    /// Every copy, as many values as [`Self::len`]: a bag of copies moves out whole, with its
    /// charge; a counted one's copies are charged to `pool` before they are made, beside it.
    pub(crate) fn into_copies(self, pool: &'e Pool) -> Result<Buf<'e, T>, BudgetExceeded> {
        let total = self.len();
        let counted = match self {
            Self::Copies(values) => return Ok(values),
            Self::Counted(counted) => counted,
        };
        let mut values = Buf::with_capacity_in(pool, total)?;
        values.fill(total, |room| {
            for &(value, copies) in counted.iter() {
                room.extend(std::iter::repeat_n(value, copies));
            }
        })?;
        Ok(values)
    }

    /// How many entries the bag holds: its copies, or its counted values.
    fn entries(&self) -> usize {
        match self {
            Self::Copies(values) => values.len(),
            Self::Counted(counted) => counted.len(),
        }
    }
}

/// Bags added up, each value once with its copies, in the order each value first comes.
struct Tally<'e, T> {
    counted: Buf<'e, (T, usize)>,
    at: Map<'e, T, usize>,
}

impl<'e, T: Copy + Eq + std::hash::Hash> Tally<'e, T> {
    fn new(pool: &'e Pool) -> Self {
        Self {
            counted: Buf::in_pool(pool),
            at: Map::in_pool(pool),
        }
    }

    /// `value` added `copies` times; its room charged first. Refused, nothing changes.
    fn add(&mut self, value: T, copies: usize) -> Result<(), BudgetExceeded> {
        let next = self.counted.len();
        // The entry's room before its index's: an index never points past the entries.
        self.counted.reserve(1)?;
        let (&mut at, new) = self.at.get_or_insert_with(value, || next)?;
        match new {
            true => self.counted.push((value, copies)),
            false => {
                let held = &mut self.counted[at].1;
                *held = held.saturating_add(copies);
                Ok(())
            }
        }
    }

    /// The values `values` holds, a copy each.
    fn of(pool: &'e Pool, values: &[T]) -> Result<Self, BudgetExceeded> {
        let mut tally = Self::new(pool);
        for &value in values {
            tally.add(value, 1)?;
        }
        Ok(tally)
    }

    fn finish(self) -> Bag<'e, T> {
        Bag::Counted(self.counted)
    }
}

/// An alternative's bags put together as its arms arrive, one at a time
/// ([`PathEvaluator::either`]).
enum Union<'e, T> {
    /// Every arm so far copies, and named once: their copies, concatenated.
    Copies(Buf<'e, T>),
    /// Each value once with its copies.
    Counted(Tally<'e, T>),
    /// The arms' values, for a set-valued path, deduplicated whenever an arm that isn't the
    /// last leaves more than twice `distinct` (the first arm's, or what the last
    /// deduplication left): at most twice the support and one arm.
    Support { values: Buf<'e, T>, distinct: usize },
}

impl<'e, T: Copy + Eq + std::hash::Hash + Ord> Union<'e, T> {
    fn new(pool: &'e Pool, support: bool) -> Self {
        match support {
            true => Self::Support {
                values: Buf::in_pool(pool),
                distinct: 0,
            },
            false => Self::Copies(Buf::in_pool(pool)),
        }
    }

    /// `arm`, named `times`, added: the union grows beside the arm, which is freed once it is
    /// in. A first arm of copies becomes the union, not a copy of it. Refused, the union stops
    /// growing. A support is settled after it ([`Self::settle`]).
    fn add(&mut self, pool: &'e Pool, arm: Bag<'e, T>, times: usize) -> Result<(), BudgetExceeded> {
        if let Self::Copies(values) = self
            && (times > 1 || matches!(arm, Bag::Counted(_)))
        {
            // The copies beside the tally they go into, then freed.
            let tally = Tally::of(pool, values)?;
            *self = Self::Counted(tally);
        }
        match (self, arm) {
            (Self::Copies(values) | Self::Support { values, .. }, Bag::Copies(arm))
                if values.capacity() == 0 =>
            {
                *values = arm;
            }
            (Self::Copies(values) | Self::Support { values, .. }, Bag::Copies(arm)) => {
                values.extend_from_slice(&arm)?;
            }
            (Self::Support { values, .. }, Bag::Counted(arm)) => {
                values.extend_bounded(arm.len(), arm.iter().map(|&(value, _)| value))?;
            }
            (Self::Counted(tally), arm) => {
                for (value, copies) in arm.counted() {
                    tally.add(value, copies.saturating_mul(times))?;
                }
            }
            (Self::Copies(_), Bag::Counted(_)) => unreachable!("counted from here on, above"),
        }
        Ok(())
    }

    /// The vector an arm named `times` can be streamed into, value by value, without a
    /// vector of its own ([`PathEvaluator::stream`]): the copies' (named once) or the
    /// support's.
    fn streamed(&mut self, times: usize) -> Option<&mut Buf<'e, T>> {
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
                dedup_sorted(values);
                *distinct = values.len();
            }
        }
    }

    fn finish(self) -> Bag<'e, T> {
        match self {
            Self::Copies(values) | Self::Support { values, .. } => Bag::Copies(values),
            Self::Counted(tally) => tally.finish(),
        }
    }
}

/// `values`, sorted, with each value once.
fn dedup_sorted<T: PartialEq + Copy>(values: &mut Buf<'_, T>) {
    let mut previous = None;
    values.retain(|&value| {
        let first = previous != Some(value);
        previous = Some(value);
        first
    });
}

/// A sequence step's ends: pushed as they come, a copy each, unless what it starts from is
/// counted already; then each end once with how many ways ([`Tally`]). A step whose copies
/// would outnumber the entries it was given is counted from that end on: ways that grow, by
/// the path's text (an alternative) or by the data (a node's several ends), are the only
/// ones that compound over the next steps (`(p?/…/p?)+` over a cycle: 2^n copies), and a
/// step that doesn't grow never gives more copies than it got. So a step holds at most its
/// entries' copies before it counts: `p/(64 distinct arms)` over n pairs holds n copies and
/// then each end once, never 64·n (the package fix check r2 of the executor memory stack,
/// EMS-R1). Counting hashes every end: a sequence whose steps give one end an entry
/// (`author/city`) needs none of it (the executor memory stack's timing: counting every step
/// cost path alternatives x1.56).
enum Ends<'e, T> {
    Copies(Buf<'e, T>),
    Counted(Tally<'e, T>),
}

impl<'e, T: Copy + Eq + std::hash::Hash> Ends<'e, T> {
    fn new(pool: &'e Pool, counted: bool) -> Self {
        match counted {
            true => Self::Counted(Tally::new(pool)),
            false => Self::Copies(Buf::in_pool(pool)),
        }
    }

    /// `value` added `copies` times, in a step given `entries` entries: a copy while copies
    /// don't outnumber the entries, else counted from here on (the copies beside the tally
    /// they go into, then freed). Refused, it stops growing.
    fn add(
        &mut self,
        pool: &'e Pool,
        value: T,
        copies: usize,
        entries: usize,
    ) -> Result<(), BudgetExceeded> {
        if let Self::Copies(values) = self {
            if copies == 1 && values.len() < entries {
                return values.push(value);
            }
            let tally = Tally::of(pool, values)?;
            *self = Self::Counted(tally);
        }
        match self {
            Self::Counted(tally) => tally.add(value, copies),
            Self::Copies(_) => unreachable!("copies were counted above"),
        }
    }

    fn finish(self) -> Bag<'e, T> {
        match self {
            Self::Copies(values) => Bag::Copies(values),
            Self::Counted(tally) => tally.finish(),
        }
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
    pub(crate) closures: Cell<usize>,
    /// What every buffer of the evaluation draws from: a grant on the query's budget whose
    /// spare makes the small bags of a traversal's nodes cost no budget call. Its spare is
    /// the budget owner's rule ([`nrese_exec::Grant::new`]: the part's cap over the pool's
    /// workers, none under an envelope's 1 KiB floor), so evaluators in parallel workers
    /// (EXISTS) hold one cap between them.
    pool: Pool,
    /// The first charge the budget refused: it stops the evaluator as a cancellation does,
    /// and the entry points report it ([`Self::answer`]).
    refused: Cell<Option<BudgetExceeded>>,
}

impl<'a> PathEvaluator<'a> {
    /// An evaluator of paths in `graph` whose buffers draw from `budget`.
    pub(crate) fn new(
        snapshot: &'a Snapshot,
        model: ReadModel,
        graph: PathGraph<'a>,
        cancellation: Option<&'a crate::CancellationToken>,
        budget: &Arc<Budget>,
    ) -> Self {
        Self {
            snapshot,
            model,
            graph,
            fixed: None,
            cancellation,
            closures: Cell::new(0),
            pool: Pool::new(Arc::clone(budget)),
            refused: Cell::new(None),
        }
    }
}

impl PathEvaluator<'_> {
    /// What the evaluation's buffers draw from: the answer's columns are made in it
    /// ([`Bag::into_copies`]) and their charge handed to the answer's table.
    pub(crate) fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Cancelled, or refused a charge.
    fn stopped(&self) -> bool {
        self.refused.get().is_some()
            || self
                .cancellation
                .is_some_and(crate::CancellationToken::is_cancelled)
    }

    /// The first refusal recorded: the evaluator stops.
    fn refuse(&self, refused: BudgetExceeded) {
        if self.refused.get().is_none() {
            self.refused.set(Some(refused));
        }
    }

    /// Whether a buffer's growth fitted; a refusal is recorded.
    fn fits(&self, grown: Result<(), BudgetExceeded>) -> bool {
        match grown {
            Ok(()) => true,
            Err(refused) => {
                self.refuse(refused);
                false
            }
        }
    }

    /// `value`, unless a charge was refused while it was made: then what a stopped traversal
    /// returned, never an answer.
    fn answer<R>(&self, value: R) -> Result<R, BudgetExceeded> {
        match self.refused.get() {
            Some(refused) => Err(refused),
            None => Ok(value),
        }
    }

    /// An empty buffer of the evaluation.
    fn buf<T>(&self) -> Buf<'_, T> {
        Buf::in_pool(&self.pool)
    }

    /// `items` in a buffer of the evaluation, charged as it grows; refused, the items so far.
    fn collect<T>(&self, items: impl Iterator<Item = T>) -> Buf<'_, T> {
        let mut values = self.buf();
        self.push_all(&mut values, items);
        values
    }

    /// `items` pushed into `values`, its growth charged first. False if refused: then the
    /// items so far.
    fn push_all<T>(&self, values: &mut Buf<'_, T>, items: impl Iterator<Item = T>) -> bool {
        for item in items {
            if !self.fits(values.push(item)) {
                return false;
            }
        }
        true
    }

    /// The values of a bag, without their copies (a value may still come more than once).
    fn values<'e, T: Copy>(&'e self, bag: Bag<'e, T>) -> Buf<'e, T> {
        match bag {
            Bag::Copies(values) => values,
            Bag::Counted(counted) => {
                let mut values = self.buf();
                if self.fits(values.reserve_exact(counted.len())) {
                    self.push_all(&mut values, counted.iter().map(|&(value, _)| value));
                }
                values
            }
        }
    }

    /// `values` with each value once, in the order it first comes.
    fn dedup_keep_order<'e, T: Copy + Eq + std::hash::Hash>(
        &'e self,
        mut values: Buf<'e, T>,
    ) -> Buf<'e, T> {
        let mut seen: Map<'e, T, ()> = Map::in_pool(&self.pool);
        values.retain(|&value| match seen.get_or_insert_with(value, || ()) {
            Ok((_, first)) => first,
            Err(refused) => {
                self.refuse(refused);
                true
            }
        });
        values
    }

    /// A closure's result, charged to the evaluator's pool while it was made
    /// ([`nrese_exec::graph`]'s `_within` kernels): a refused charge is the evaluator's
    /// refusal, a stop its cancellation (which the caller's check reports); either way empty.
    fn closed<'e, T>(&'e self, made: Result<Buf<'e, T>, Stop>) -> Buf<'e, T> {
        match made {
            Ok(values) => values,
            Err(Stop::Memory(refused)) => {
                self.refuse(refused);
                self.buf()
            }
            Err(_) => self.buf(),
        }
    }

    /// The query's cancellation, as the graph kernels poll it.
    fn cancelled(&self) -> impl Fn() -> bool + Sync + '_ {
        let cancellation = self.cancellation;
        move || cancellation.is_some_and(crate::CancellationToken::is_cancelled)
    }

    /// [`transitive_closure_within`] of `edges` on the evaluator's pool, stopped with the
    /// query: then empty. None of edges a refused charge cut short (sol-loop r1 S2 of
    /// EMS-R1): they are no path's, and their closure would be built for nothing.
    fn closure_of(&self, edges: &[(u64, u64)]) -> Buf<'_, (u64, u64)> {
        if self.stopped() {
            return self.buf();
        }
        self.closures.set(self.closures.get() + 1);
        self.closed(transitive_closure_within(
            edges,
            &self.pool,
            &self.cancelled(),
        ))
    }

    /// The evaluator's stop as a search's: its refusal, or the query's cancellation.
    fn halted(&self) -> Result<(), Stop> {
        match self.refused.get() {
            Some(refused) => Err(Stop::Memory(refused)),
            None if self.stopped() => Err(Stop::Cancelled),
            None => Ok(()),
        }
    }

    /// One step of a search over `path` from `node`, forwards (`forward`) or backwards: its
    /// ends, in a buffer of the evaluation. Refused while they were found (a refusal cuts them
    /// short), the refusal, which ends the search; the query's cancellation the search polls
    /// itself, once per batch of work. The search takes each end once, however many ways lead
    /// there.
    fn ends_of(&self, path: &Path, node: u64, forward: bool) -> Result<Buf<'_, u64>, Stop> {
        let ends = match forward {
            true => self.support_from(path, node),
            false => self.support_to(path, node),
        };
        match self.refused.get() {
            Some(refused) => Err(Stop::Memory(refused)),
            None => Ok(ends),
        }
    }

    /// The closure of `step` from `node` (`forward`) or into it: the nodes it reaches in one
    /// or more steps, or zero or more with `include`, each once, in breadth-first order. The
    /// search's buffers are charged to the evaluator's pool while it searches
    /// ([`reachable_within`]; review r1 of the graph closure package, R1-F1): refused, the
    /// evaluator's refusal; stopped, empty.
    fn search(&self, step: &Path, node: u64, include: bool, forward: bool) -> Buf<'_, u64> {
        self.closed(self.searched(node, include, |node| self.ends_of(step, node, forward)))
    }

    /// [`Self::search`] with each node's ends found by `ends`, and its stop returned.
    fn searched<'e, S: std::ops::Deref<Target = [u64]>>(
        &'e self,
        node: u64,
        include: bool,
        ends: impl FnMut(u64) -> Result<S, Stop>,
    ) -> Result<Buf<'e, u64>, Stop> {
        self.halted()?;
        reachable_within(node, include, Probes(ends), &self.pool, &self.cancelled())
    }

    /// The ends of `path` from `node`, for a set-valued path (a search's step, `p?`): each
    /// at least once, an alternative's put together as a support ([`Self::either`]).
    fn support_from(&self, path: &Path, node: u64) -> Buf<'_, u64> {
        match path {
            Path::Alternative(paths) => self.values(self.either(
                paths,
                true,
                |p| Bag::Copies(self.support_from(p, node)),
                |p, values| self.stream(p, values, Some(node), None, |(_, _, o)| o),
            )),
            Path::Reverse(p) => self.support_to(p, node),
            _ => self.values(self.from(path, node)),
        }
    }

    /// [`Self::support_from`] backwards: the starts reaching `node`.
    fn support_to(&self, path: &Path, node: u64) -> Buf<'_, u64> {
        match path {
            Path::Alternative(paths) => self.values(self.either(
                paths,
                true,
                |p| Bag::Copies(self.support_to(p, node)),
                |p, values| self.stream(p, values, None, Some(node), |(s, _, _)| s),
            )),
            Path::Reverse(p) => self.support_from(p, node),
            _ => self.values(self.to(path, node)),
        }
    }

    /// The pairs of `path`, for a set-valued path (a closure's edges, `p?`): each at least
    /// once, an alternative's put together as a support ([`Self::either`]).
    fn support(&self, path: &Path) -> Buf<'_, (u64, u64)> {
        match path {
            Path::Alternative(paths) => self.values(self.either(
                paths,
                true,
                |p| Bag::Copies(self.support(p)),
                |p, values| self.stream(p, values, None, None, |(s, _, o)| (s, o)),
            )),
            Path::Reverse(p) => {
                let mut pairs = self.support(p);
                for pair in pairs.iter_mut() {
                    *pair = (pair.1, pair.0);
                }
                pairs
            }
            _ => self.values(self.open(path)),
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
    fn nodes(&self) -> Buf<'_, u64> {
        let (subjects, objects) = self.subjects_and_objects();
        let mut nodes = self.buf();
        if self.fits(nodes.reserve_exact(subjects.len().max(objects.len()))) {
            merge_union(&subjects, &objects, |node| {
                if !self.stopped() {
                    self.fits(nodes.push(node));
                }
            });
        }
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
        let edges = self.answer(self.edges(step))?;
        let sizes = match closure_sizes_within(&edges, &self.pool, &self.cancelled()) {
            Ok(sizes) => sizes,
            Err(Stop::Memory(refused)) => return Err(refused),
            Err(_) => return Ok(None),
        };
        drop(edges);
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
        let reached = self.searched(node, matches!(path, Path::ZeroOrMore(_)), |n| {
            if expanded.get() < budget {
                expanded.set(expanded.get() + 1);
                self.ends_of(step, n, forward)
            } else {
                // Past the budget: no more nodes expanded, and no estimate.
                expanded.set(budget + 1);
                Ok(self.buf())
            }
        });
        // Stopped or refused: no estimate (the planner's own evaluator, nothing the query keeps).
        let reached = reached.ok()?.len();
        (expanded.get() <= budget && self.refused.get().is_none()).then_some(reached)
    }

    /// Ends reachable from `start` (a bag), or the charge refused while they were found.
    pub(crate) fn ends(&self, path: &Path, start: u64) -> Result<Bag<'_, u64>, BudgetExceeded> {
        let ends = self.from(path, start);
        self.answer(ends)
    }

    /// Starts that reach `end` (a bag), or the charge refused while they were found.
    pub(crate) fn starts(&self, path: &Path, end: u64) -> Result<Bag<'_, u64>, BudgetExceeded> {
        let starts = self.to(path, end);
        self.answer(starts)
    }

    /// Every `(start, end)` pair (a bag), or the charge refused while they were found.
    pub(crate) fn pairs(&self, path: &Path) -> Result<Bag<'_, (u64, u64)>, BudgetExceeded> {
        let pairs = self.open(path);
        self.answer(pairs)
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
    fn from(&self, path: &Path, start: u64) -> Bag<'_, u64> {
        let copies = match path {
            Path::Link(None) => self.buf(),
            Path::Link(Some(p)) => {
                self.collect(self.quads(Some(start), Some(*p), None).map(|(_, _, o)| o))
            }
            Path::Reverse(p) => return self.to(p, start),
            Path::Sequence(steps) => return self.through(steps, start, true),
            Path::Alternative(paths) => {
                return self.either(
                    paths,
                    false,
                    |p| self.from(p, start),
                    |p, values| self.stream(p, values, Some(start), None, |(_, _, o)| o),
                );
            }
            Path::OneOrMore(p) => self.search(p, start, false, true),
            Path::ZeroOrMore(p) => {
                if !self.is_node(start) {
                    return Bag::Copies(self.buf());
                }
                self.search(p, start, true, true)
            }
            Path::ZeroOrOne(p) => {
                if !self.is_node(start) {
                    return Bag::Copies(self.buf());
                }
                let mut ends = self.buf();
                if self.fits(ends.push(start)) {
                    let support = self.support_from(p, start);
                    self.fits(ends.extend_from_slice(&support));
                }
                self.dedup_keep_order(ends)
            }
            Path::Negated(excluded) => self.collect(
                self.quads(Some(start), None, None)
                    .filter(|(_, p, _)| !excluded.contains(p))
                    .map(|(_, _, o)| o),
            ),
        };
        Bag::Copies(copies)
    }

    /// The ends reached from `node` through `steps` in sequence (`forward`), or the starts
    /// reaching it (the last step first): one per way through, each end once with how
    /// many, step by step ([`Self::step`]); each step's input freed once the next is made.
    fn through(&self, steps: &[Path], node: u64, forward: bool) -> Bag<'_, u64> {
        let mut start = self.buf();
        if !self.fits(start.push(node)) {
            return Bag::Copies(start);
        }
        let mut nodes = Bag::Copies(start);
        let ordered: Box<dyn Iterator<Item = &Path>> = match forward {
            true => Box::new(steps.iter()),
            false => Box::new(steps.iter().rev()),
        };
        for step in ordered {
            // Stopped, the step makes nothing: a stopped sequence ends empty.
            if nodes.is_empty() {
                break;
            }
            nodes = self
                .step(&nodes, step, forward, |m| m, |_, end| end)
                .finish();
        }
        nodes
    }

    /// One step of a sequence: each entry of `input` (its node `node`) followed through
    /// `step` forwards, or backwards to the starts reaching it, `out` making the result of an
    /// entry and an end, the ways multiplied ([`Ends::add`]: counted once its copies would
    /// outnumber the entries). A leaf step (a link, a negated set) streams its statements'
    /// ends in, with no vector of their own; another step's bag is added, then freed. Stops
    /// early once a charge was refused.
    fn step<'e, I: Copy, O: Copy + Eq + std::hash::Hash>(
        &'e self,
        input: &Bag<'e, I>,
        step: &Path,
        forward: bool,
        node: impl Fn(I) -> u64,
        out: impl Fn(I, u64) -> O,
    ) -> Ends<'e, O> {
        let entries = input.entries();
        let mut next = Ends::new(&self.pool, matches!(input, Bag::Counted(_)));
        for (item, ways) in input.counted() {
            if self.stopped() {
                break;
            }
            let m = node(item);
            let (subject, object) = match forward {
                true => (Some(m), None),
                false => (None, Some(m)),
            };
            let streamed = self.leaf_each(step, subject, object, |(s, _, o)| {
                let end = if forward { o } else { s };
                self.fits(next.add(&self.pool, out(item, end), ways, entries))
            });
            let fits = streamed.unwrap_or_else(|| {
                let ends = match forward {
                    true => self.from(step, m),
                    false => self.to(step, m),
                };
                ends.counted().all(|(end, more)| {
                    let ways = ways.saturating_mul(more);
                    self.fits(next.add(&self.pool, out(item, end), ways, entries))
                })
            });
            if !fits {
                break;
            }
        }
        next
    }

    /// A leaf path's statements `subject ? object` (bound where given), each passed to
    /// `each` until it returns false: `None` for any other path, else whether all were.
    fn leaf_each(
        &self,
        path: &Path,
        subject: Option<u64>,
        object: Option<u64>,
        mut each: impl FnMut((u64, u64, u64)) -> bool,
    ) -> Option<bool> {
        match path {
            Path::Link(None) => Some(true),
            Path::Link(Some(p)) => Some(self.quads(subject, Some(*p), object).all(&mut each)),
            Path::Negated(excluded) => Some(
                self.quads(subject, None, object)
                    .filter(|(_, p, _)| !excluded.contains(p))
                    .all(&mut each),
            ),
            _ => None,
        }
    }

    /// The bags of `paths` together (a UNION): copies stay copies, and counted ones are
    /// added up with them; for a set-valued path (`support`), the arms' values, each at
    /// least once.
    /// An arm the alternative names again is the same bag again ([`Path::Alternative`]:
    /// matched when the path is resolved): it is followed once and its values counted as
    /// often, never copied, so `p|p|…` costs and holds what `p` does however often the
    /// path's text repeats it.
    /// The arms come one at a time ([`Union`]). A leaf arm (a link, a negated set) streams
    /// its values into the union ([`Self::stream`]), with no vector of its own, as
    /// engine-v2's `flat_map` did (the executor memory stack's path counts, EMS-15: an arm's
    /// vector copied into the union cost a closure over an alternative +38 M instructions a
    /// run). Any other arm arrives as its bag, the union grown beside it, then the arm freed
    /// (the first arm of copies becomes the union, not a copy). Distinct arms with
    /// overlapping bags (`!(q)|!(q|q)|…`) so hold the union and one arm, and under a
    /// set-valued path the union at most twice their support (the package review r1 of the
    /// executor memory stack, EMS-R1). A refused charge stops here with what was put
    /// together, which the entry points never answer ([`Self::answer`]).
    fn either<'e, T: Copy + Eq + std::hash::Hash + Ord>(
        &'e self,
        arms: &[(Path, usize)],
        support: bool,
        bag: impl Fn(&Path) -> Bag<'e, T>,
        stream: impl Fn(&Path, &mut Buf<'e, T>) -> Option<bool>,
    ) -> Bag<'e, T> {
        let mut union = Union::new(&self.pool, support);
        for (i, (arm, times)) in arms.iter().enumerate() {
            let (times, last) = (*times, i + 1 == arms.len());
            if self.stopped() {
                break;
            }
            let streamed = match union.streamed(times) {
                Some(values) => stream(arm, values),
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
            if !self.fits(union.add(&self.pool, arm, times)) {
                break;
            }
            union.settle(last);
        }
        union.finish()
    }

    /// A leaf arm's values pushed into `values` (an alternative's union, [`Self::either`]):
    /// the statements `subject ? object`, bound where given, `value` making each value of
    /// a statement. `None` for any other path; false if the union's growth was refused.
    fn stream<T>(
        &self,
        path: &Path,
        values: &mut Buf<'_, T>,
        subject: Option<u64>,
        object: Option<u64>,
        value: impl Fn((u64, u64, u64)) -> T,
    ) -> Option<bool> {
        match path {
            Path::Link(None) => Some(true),
            Path::Link(Some(p)) => {
                let statements = self.quads(subject, Some(*p), object);
                Some(self.push_all(values, statements.map(value)))
            }
            Path::Negated(excluded) => {
                let statements = self.quads(subject, None, object);
                let kept = statements.filter(|(_, p, _)| !excluded.contains(p));
                Some(self.push_all(values, kept.map(value)))
            }
            _ => None,
        }
    }

    /// Starts that reach `end` (a bag).
    fn to(&self, path: &Path, end: u64) -> Bag<'_, u64> {
        let copies = match path {
            Path::Link(None) => self.buf(),
            Path::Link(Some(p)) => {
                self.collect(self.quads(None, Some(*p), Some(end)).map(|(s, _, _)| s))
            }
            Path::Reverse(p) => return self.from(p, end),
            Path::Sequence(steps) => return self.through(steps, end, false),
            Path::Alternative(paths) => {
                return self.either(
                    paths,
                    false,
                    |p| self.to(p, end),
                    |p, values| self.stream(p, values, None, Some(end), |(s, _, _)| s),
                );
            }
            Path::OneOrMore(p) => self.search(p, end, false, false),
            Path::ZeroOrMore(p) => {
                if !self.is_node(end) {
                    return Bag::Copies(self.buf());
                }
                self.search(p, end, true, false)
            }
            Path::ZeroOrOne(p) => {
                if !self.is_node(end) {
                    return Bag::Copies(self.buf());
                }
                let mut starts = self.buf();
                if self.fits(starts.push(end)) {
                    let support = self.support_to(p, end);
                    self.fits(starts.extend_from_slice(&support));
                }
                self.dedup_keep_order(starts)
            }
            Path::Negated(excluded) => self.collect(
                self.quads(None, None, Some(end))
                    .filter(|(_, p, _)| !excluded.contains(p))
                    .map(|(s, _, _)| s),
            ),
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
                    let mut middle = self.values(self.through(before, start, true));
                    middle.sort_unstable();
                    dedup_sorted(&mut middle);
                    middle.iter().any(|&m| self.connects(last, m, end))
                }
            },
            Path::Alternative(arms) => arms.iter().any(|(p, _)| self.connects(p, start, end)),
            Path::ZeroOrMore(p) => {
                if start == end {
                    self.is_node(start)
                } else {
                    self.search(p, start, false, true).contains(&end)
                }
            }
            Path::OneOrMore(p) => self.search(p, start, false, true).contains(&end),
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
    ) -> Result<Bag<'_, (u64, u64)>, BudgetExceeded> {
        let pairs = match path {
            Path::OneOrMore(step) | Path::ZeroOrMore(step) if starts.len() > PROBED_VALUES => {
                let reflexive = matches!(path, Path::ZeroOrMore(_));
                // The edges' charge stays while the adjacency made of them lives (declared
                // before it: dropped after it).
                let (edges, _held) = self.edges(step).into_parts();
                let adjacency = Adjacency::new(edges);
                let starts = starts
                    .iter()
                    .copied()
                    .take_while(|_| !self.stopped())
                    .filter(|&node| !reflexive || self.is_node(node));
                let pairs =
                    closure_within(&adjacency, starts, reflexive, &self.pool, &self.cancelled());
                Bag::Copies(self.closed(pairs))
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
    ) -> Result<Bag<'_, (u64, u64)>, BudgetExceeded> {
        let pairs = match path {
            Path::OneOrMore(step) | Path::ZeroOrMore(step) if ends.len() > PROBED_VALUES => {
                let reflexive = matches!(path, Path::ZeroOrMore(_));
                let mut backwards = self.edges(step);
                for edge in backwards.iter_mut() {
                    *edge = (edge.1, edge.0);
                }
                // The edges' charge stays while the adjacency made of them lives.
                let (backwards, _held) = backwards.into_parts();
                let adjacency = Adjacency::new(backwards);
                let ends = ends
                    .iter()
                    .copied()
                    .take_while(|_| !self.stopped())
                    .filter(|&node| !reflexive || self.is_node(node));
                let pairs =
                    closure_within(&adjacency, ends, reflexive, &self.pool, &self.cancelled());
                let mut pairs = self.closed(pairs);
                for pair in pairs.iter_mut() {
                    *pair = (pair.1, pair.0);
                }
                Bag::Copies(pairs)
            }
            _ => self.pairs_at(ends, |end| self.to(path, end), |end, start| (start, end)),
        };
        self.answer(pairs)
    }

    /// The pairs of each of `nodes` (distinct) with each value of its bag, `pair` making a
    /// `(start, end)` pair of the node and the value: copies while every bag is copies.
    /// The pairs grow beside each node's bag, which is freed once it is in (the bags of 64
    /// distinct arms from 4,096 bound values are 64 pairs a node: sol-loop r1 S1 of EMS-R1).
    fn pairs_at<'e>(
        &'e self,
        nodes: &[u64],
        bag: impl Fn(u64) -> Bag<'e, u64>,
        pair: impl Fn(u64, u64) -> (u64, u64),
    ) -> Bag<'e, (u64, u64)> {
        let (mut copies, mut counted) = (self.buf(), self.buf());
        for &node in nodes.iter().take_while(|_| !self.stopped()) {
            let added = match bag(node) {
                Bag::Copies(values) => {
                    let pairs = values.iter().map(|&value| pair(node, value));
                    copies.fill(values.len(), |room| room.extend(pairs))
                }
                Bag::Counted(values) => {
                    let pairs = values.iter().map(|&(value, n)| (pair(node, value), n));
                    counted.fill(values.len(), |room| room.extend(pairs))
                }
            };
            if !self.fits(added) {
                break;
            }
        }
        if counted.is_empty() {
            return Bag::Copies(copies);
        }
        if self.fits(counted.reserve_exact(copies.len())) {
            self.push_all(&mut counted, copies.iter().map(|&pair| (pair, 1)));
        }
        Bag::Counted(counted)
    }

    /// The pairs of `path` as edges for a closure: each at least once ([`Self::support`]),
    /// which is all a closure's reachability needs.
    fn edges(&self, path: &Path) -> Buf<'_, (u64, u64)> {
        self.support(path)
    }

    /// Every `(start, end)` pair.
    fn open(&self, path: &Path) -> Bag<'_, (u64, u64)> {
        let copies = match path {
            Path::Link(None) => self.buf(),
            Path::Link(Some(p)) => {
                self.collect(self.quads(None, Some(*p), None).map(|(s, _, o)| (s, o)))
            }
            Path::Reverse(p) => {
                let flip = |&mut (s, o): &mut (u64, u64)| (o, s);
                return match self.open(p) {
                    Bag::Copies(mut pairs) => {
                        pairs.iter_mut().for_each(|pair| *pair = flip(pair));
                        Bag::Copies(pairs)
                    }
                    Bag::Counted(mut pairs) => {
                        pairs.iter_mut().for_each(|(pair, _)| *pair = flip(pair));
                        Bag::Counted(pairs)
                    }
                };
            }
            Path::Sequence(steps) => {
                let Some((first, rest)) = steps.split_first() else {
                    return Bag::Copies(self.collect(self.nodes().iter().map(|&n| (n, n))));
                };
                // Each pair once with how many ways through, step by step (`through`), each
                // step's input freed once the next is made.
                let mut pairs = self.open(first);
                for step in rest {
                    // Stopped, the step makes nothing: a stopped sequence ends empty.
                    if pairs.is_empty() {
                        break;
                    }
                    let next = self.step(&pairs, step, true, |(_, m)| m, |(s, _), o| (s, o));
                    pairs = next.finish();
                }
                return pairs;
            }
            Path::Alternative(paths) => {
                return self.either(
                    paths,
                    false,
                    |p| self.open(p),
                    |p, values| self.stream(p, values, None, None, |(s, _, o)| (s, o)),
                );
            }
            // The closure's pairs charged before they are made, beside its edges.
            Path::OneOrMore(p) => {
                let edges = self.edges(p);
                self.closure_of(&edges)
            }
            Path::ZeroOrMore(p) => {
                let edges = self.edges(p);
                let mut pairs = self.closure_of(&edges);
                drop(edges);
                // Every node with itself, unless the closure has it already (on a cycle).
                let mut on_cycle =
                    self.collect(pairs.iter().filter(|(a, b)| a == b).map(|&(a, _)| a));
                on_cycle.sort_unstable();
                let nodes = self.nodes();
                let reflexive = nodes
                    .iter()
                    .filter(|node| on_cycle.binary_search(node).is_err())
                    .map(|&node| (node, node));
                self.push_all(&mut pairs, reflexive);
                pairs
            }
            Path::ZeroOrOne(p) => {
                let mut pairs = self.collect(self.nodes().iter().map(|&n| (n, n)));
                let support = self.support(p);
                self.fits(pairs.extend_from_slice(&support));
                drop(support);
                self.dedup_keep_order(pairs)
            }
            Path::Negated(excluded) => self.collect(
                self.quads(None, None, None)
                    .filter(|(_, p, _)| !excluded.contains(p))
                    .map(|(s, _, o)| (s, o)),
            ),
        };
        Bag::Copies(copies)
    }
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

    /// A bag's copies, sorted.
    fn sorted<T: Copy + Ord>(evaluator: &PathEvaluator<'_>, bag: Bag<'_, T>) -> Vec<T> {
        let mut values = bag.into_copies(evaluator.pool()).unwrap().to_vec();
        values.sort_unstable();
        values
    }

    /// `values`, sorted.
    fn sorted_vec<T: Ord>(mut values: Vec<T>) -> Vec<T> {
        values.sort_unstable();
        values
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
        let budget = Arc::new(Budget::unlimited());
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
        let budget = Arc::new(Budget::unlimited());
        let evaluator = PathEvaluator::new(
            &snapshot,
            ReadModel::default(),
            PathGraph::Default,
            None,
            &budget,
        );
        let [a, b, c] = [&a, &b, &c].map(|n| snapshot.lookup(n.as_ref().into()).unwrap().raw());
        let sorted = |bag| sorted(&evaluator, bag);
        let pairs = |bag| sorted_pairs(&evaluator, bag);
        assert_eq!(
            sorted(evaluator.from(&sequence, a)),
            sorted_vec(vec![a, a, c, c])
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
            sorted_vec(vec![(a, a), (a, a), (a, c), (a, c), (b, b)])
        );
        assert_eq!(
            pairs(evaluator.reached_from(&sequence, &[b]).unwrap()),
            vec![(b, b)]
        );
        assert_eq!(
            pairs(evaluator.reaching(&sequence, &[c]).unwrap()),
            vec![(a, c), (a, c)]
        );
        assert_eq!(sorted(evaluator.from(&plus, a)), sorted_vec(vec![a, c]));
        assert_eq!(sorted(evaluator.from(&plus, b)), vec![b]);
        assert_eq!(evaluator.multiplicity(&plus, a, c), 1);
        assert_eq!(evaluator.multiplicity(&plus, b, c), 0);
        assert_eq!(
            pairs(evaluator.open(&plus)),
            sorted_vec(vec![(a, a), (a, c), (b, b)])
        );
    }

    /// [`sorted`] for pairs (a closure can't be generic).
    fn sorted_pairs(evaluator: &PathEvaluator<'_>, bag: Bag<'_, (u64, u64)>) -> Vec<(u64, u64)> {
        sorted(evaluator, bag)
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
        let budget = Arc::new(Budget::unlimited());
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
        let budget = Arc::new(Budget::unlimited());
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
        assert_eq!(
            sorted(&evaluator, evaluator.from(&plus, a)),
            vec![a.min(b), a.max(b)]
        );
    }

    /// Spec (sol-loop r1 S2 of EMS-R1): a charge the budget refused stops the evaluator, and
    /// what it found so far is no answer, so no closure is built of it. Over 1,000 self-loops
    /// and `m q m`, the arms `!(q)`, `!(q|q)`, `!(q|q|q)` under `+` under a 24 KiB budget: the
    /// union's room for the arms' 2,000 and more pairs (16 KB each arm) doesn't fit, so the
    /// pairs are refused and no closure is computed.
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
        let budget = Arc::new(Budget::new(24 << 10));
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
