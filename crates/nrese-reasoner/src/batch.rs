//! The batch executor (reasoner-v2 design §4.1): full materialisation by semi-naive
//! evaluation over a vertically partitioned working set.
//!
//! - **Working set.** One `Relation` per predicate: its pairs sorted by subject and by
//!   object, plus the last round's delta in both orders.
//! - **Schema grounding** (design §3.2, [`super::eval::ground`]). Atoms over the TBox
//!   vocabulary are evaluated against the current facts and substituted into the rest of
//!   the rule. So `cax-sco` becomes one `(?x type C1) -> (?x type C2)` rule per subclass
//!   edge, and almost every atom gets a constant predicate. When a round derives schema
//!   facts, the rules are grounded again through those facts only, and the new instances
//!   are evaluated once over all facts. That keeps specialisation exact when instance
//!   rules feed the schema (punning, class `sameAs`).
//! - **Semi-naive evaluation.** A rule with n atoms runs n variants per round: atom i over
//!   the delta, atoms before it over the old facts and atoms after it over all facts.
//!   Each variant is driven by its delta atom's matches, split into morsels that run in
//!   parallel; the other atoms are index lookups, ordered by how bound they are.
//! - **Transitive module** (design §4.4): transitivity rules are replaced by a closure by
//!   SCC condensation, which is O(k²) on a k-clique where the rule's joins are O(k³).
//! - **Deduplication** is a sort per round, then a merge into each relation. There's no
//!   shared hash set, and the result doesn't depend on the thread count.
//!
//! The naive evaluator (`naive`, tests and the `oracle` feature only) is the oracle: both compute the same closure.

use std::sync::Arc;

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

use super::delta::Interrupted;
pub use super::eval::Schema;
use super::eval::{
    AllFacts, GroundProgram, Job, NEVER, Probes, Seg, Source, guards_hold, run_jobs_counted,
};
use super::ir::{Head, Rule};
use super::ir::{Triple, Violation};
use super::lists::{ListVocabulary, instantiate};
use nrese_exec::heap;

type Pair = (u64, u64);

/// The result of [`materialise`].
#[derive(Debug, Default)]
pub struct Materialisation {
    /// Facts derived beyond the input, sorted, without duplicates.
    pub derived: Vec<Triple>,
    /// Consistency violations on the closure, sorted.
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<super::lists::ListDiagnostic>,
    pub rounds: usize,
    /// Rules after grounding, at the end.
    pub ground_rules: usize,
    /// Predicates closed by the transitive module.
    pub transitive: usize,
    /// Time per phase: grounding, rule joins, modules, merging, consistency.
    pub phases: Phases,
    /// What the rounds held and did, in counts.
    pub counters: Counters,
}

/// Deterministic counts of a materialisation's rounds, for the guards of
/// `docs/design/performance.md` §0: they don't depend on the machine or the thread count.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counters {
    /// Per round: the working set's bytes after it.
    pub store_bytes: Vec<StoreBytes>,
    /// Bytes of driver matches the rule jobs copied out of the working set before they
    /// ran (P1-F1: none, they read the runs in place).
    pub driver_bytes_copied: usize,
    /// Per round: how often the rule jobs' candidates were checked against the working
    /// set (P1-F8: once per distinct candidate of a morsel).
    pub probes: Vec<u64>,
    /// Checks that came out of (predicate, subject, object) order within their morsel
    /// (P1-F8: none).
    pub unordered_probes: u64,
}

/// The bytes the working set's runs hold (both orders, spare capacity included; a run
/// shared by two roles counted once).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoreBytes {
    /// The store's input, apart.
    pub input: usize,
    /// What was added since: the base and recent runs.
    pub added: usize,
    /// The delta where it doesn't share another run.
    pub delta: usize,
}

impl StoreBytes {
    pub fn total(&self) -> usize {
        self.input + self.added + self.delta
    }
}

/// Time spent per phase of [`materialise`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Phases {
    pub load: std::time::Duration,
    pub grounding: std::time::Duration,
    pub joins: std::time::Duration,
    pub modules: std::time::Duration,
    pub merge: std::time::Duration,
    pub consistency: std::time::Duration,
}

/// A sorted run of pairs, by subject and by object. Cloning shares the pairs, so the
/// recent run can be the delta itself instead of a copy.
#[derive(Default, Clone)]
struct Run {
    so: Arc<Vec<Pair>>,
    /// `(object, subject)` pairs.
    os: Arc<Vec<Pair>>,
}

impl Run {
    /// A run of `so` (sorted, deduplicated).
    fn new(so: Vec<Pair>) -> Self {
        let mut os: Vec<Pair> = so.iter().map(|&(s, o)| (o, s)).collect();
        os.par_sort_unstable();
        Self {
            so: Arc::new(so),
            os: Arc::new(os),
        }
    }

    fn len(&self) -> usize {
        self.so.len()
    }

    /// The bytes of both orders, spare capacity included.
    fn bytes(&self) -> usize {
        (self.so.capacity() + self.os.capacity()) * std::mem::size_of::<Pair>()
    }

    /// Whether `other` holds the same pairs (shared, not a copy).
    fn shares(&self, other: &Run) -> bool {
        Arc::ptr_eq(&self.so, &other.so) && Arc::ptr_eq(&self.os, &other.os)
    }

    fn contains(&self, s: u64, o: u64) -> bool {
        self.so.binary_search(&(s, o)).is_ok()
    }

    /// Calls `f` with every `(s, o)` matching the bound positions that `keep` accepts.
    fn scan(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        keep: &dyn Fn(u64, u64) -> bool,
        f: &mut dyn FnMut(u64, u64),
    ) {
        match (s, o) {
            (Some(s), Some(o)) => {
                if self.contains(s, o) && keep(s, o) {
                    f(s, o);
                }
            }
            (Some(s), None) => {
                for &(_, o) in range(&self.so, s) {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
            (None, Some(o)) => {
                for &(_, s) in range(&self.os, o) {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
            (None, None) => {
                for &(s, o) in self.so.iter() {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
        }
    }

    /// The pairs matching the bound positions: one contiguous slice of one order, and
    /// whether that order is `(object, subject)`.
    fn matching(&self, s: Option<u64>, o: Option<u64>) -> (&[Pair], bool) {
        match (s, o) {
            (Some(s), Some(o)) => match self.so.binary_search(&(s, o)) {
                Ok(at) => (&self.so[at..=at], false),
                Err(_) => (&[], false),
            },
            (Some(s), None) => (range(&self.so, s), false),
            (None, Some(o)) => (range(&self.os, o), true),
            (None, None) => (&self.so, false),
        }
    }

    fn estimate(&self, s: Option<u64>, o: Option<u64>) -> usize {
        match (s, o) {
            (Some(s), Some(o)) => usize::from(self.contains(s, o)),
            (Some(s), None) => range(&self.so, s).len(),
            (None, Some(o)) => range(&self.os, o).len(),
            (None, None) => self.so.len(),
        }
    }

    /// A run of `os` (`(object, subject)` pairs, sorted, deduplicated).
    fn from_os(os: Vec<Pair>) -> Self {
        let mut so: Vec<Pair> = os.iter().map(|&(o, s)| (s, o)).collect();
        so.par_sort_unstable();
        Self {
            so: Arc::new(so),
            os: Arc::new(os),
        }
    }

    /// The union with a disjoint run.
    fn merge(&self, other: &Run) -> Run {
        if self.len() == 0 {
            return other.clone();
        }
        Run {
            so: Arc::new(merge(&self.so, &other.so)),
            os: Arc::new(merge(&self.os, &other.os)),
        }
    }
}

/// The pairs of one predicate, in sorted runs: the store's input, apart, and what was
/// added since, in a large base and a small recent run that takes each round's delta.
/// The recent run is folded into the base once it reaches a quarter of its size, so a
/// round costs its delta plus the recent run, not the whole relation, and the total
/// merge work stays O(n log n). The input, kept apart, makes the added pairs the base
/// and the recent run: what a materialisation derived, without a list of its own.
#[derive(Default)]
pub(crate) struct Relation {
    /// The pairs the store started with, once the first round has read them as its
    /// delta.
    input: Run,
    base: Run,
    recent: Run,
    /// The last round's pairs (also in `recent`).
    delta: Run,
    /// Whether the delta is still the input (the store's first round is to come).
    fresh: bool,
}

/// The pairs whose first component is `key`.
fn range(pairs: &[Pair], key: u64) -> &[Pair] {
    let start = pairs.partition_point(|p| p.0 < key);
    let len = pairs[start..].partition_point(|p| p.0 == key);
    &pairs[start..start + len]
}

/// Merges two sorted, disjoint pair lists.
fn merge(a: &[Pair], b: &[Pair]) -> Vec<Pair> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

impl Relation {
    pub(crate) fn delta_len(&self) -> usize {
        self.delta.len()
    }

    fn bytes(&self) -> StoreBytes {
        let (input, base, recent) = (&self.input, &self.base, &self.recent);
        let shared = |run: &Run| [input, base, recent].iter().any(|other| run.shares(other));
        StoreBytes {
            input: input.bytes(),
            added: base.bytes()
                + if recent.shares(base) {
                    0
                } else {
                    recent.bytes()
                },
            delta: if shared(&self.delta) {
                0
            } else {
                self.delta.bytes()
            },
        }
    }

    /// Whether the delta has a pair with object `o`.
    pub(crate) fn delta_has_object(&self, o: u64) -> bool {
        !range(&self.delta.os, o).is_empty()
    }

    /// Calls `f` with each distinct object of the delta.
    pub(crate) fn delta_objects(&self, f: &mut dyn FnMut(u64)) {
        for chunk in self.delta.os.chunk_by(|a, b| a.0 == b.0) {
            f(chunk[0].0);
        }
    }

    pub(crate) fn contains(&self, s: u64, o: u64) -> bool {
        self.base.contains(s, o) || self.recent.contains(s, o) || self.input.contains(s, o)
    }

    /// Every pair, in no particular order.
    pub(crate) fn pairs(&self) -> Vec<Pair> {
        let mut out = Vec::with_capacity(self.input.len() + self.base.len() + self.recent.len());
        out.extend_from_slice(&self.input.so);
        out.extend_from_slice(&self.base.so);
        out.extend_from_slice(&self.recent.so);
        out
    }

    /// Calls `f` with every `(s, o)` matching the bound positions in `seg`.
    fn scan(&self, s: Option<u64>, o: Option<u64>, seg: Seg, f: &mut dyn FnMut(u64, u64)) {
        let all = |_: u64, _: u64| true;
        match seg {
            Seg::Delta => self.delta.scan(s, o, &all, f),
            Seg::All => {
                self.input.scan(s, o, &all, f);
                self.base.scan(s, o, &all, f);
                self.recent.scan(s, o, &all, f);
            }
            // The delta is in `recent` only, so the input and the base need no filter.
            Seg::Old => {
                self.input.scan(s, o, &all, f);
                self.base.scan(s, o, &all, f);
                let old = |s: u64, o: u64| !self.delta.contains(s, o);
                self.recent.scan(s, o, &old, f);
            }
        }
    }

    /// The matches of the bound positions in `seg`, as the slices of the runs that
    /// hold them, in [`Relation::scan`]'s order: each with whether its order is
    /// `(object, subject)`, and whether its pairs are read as old (the recent run, whose
    /// delta pairs are skipped).
    fn matching(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> [(&[Pair], bool, bool); 3] {
        fn slice(run: &Run, s: Option<u64>, o: Option<u64>, old: bool) -> (&[Pair], bool, bool) {
            let (pairs, swapped) = run.matching(s, o);
            (pairs, swapped, old)
        }
        let none = (&[][..], false, false);
        match seg {
            Seg::Delta => [slice(&self.delta, s, o, false), none, none],
            Seg::All => [
                slice(&self.input, s, o, false),
                slice(&self.base, s, o, false),
                slice(&self.recent, s, o, false),
            ],
            Seg::Old => [
                slice(&self.input, s, o, false),
                slice(&self.base, s, o, false),
                slice(&self.recent, s, o, true),
            ],
        }
    }

    /// An upper bound on the matches of the bound positions in `seg`.
    fn estimate(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> usize {
        match seg {
            Seg::Delta => self.delta.estimate(s, o),
            Seg::Old | Seg::All => {
                self.input.estimate(s, o) + self.base.estimate(s, o) + self.recent.estimate(s, o)
            }
        }
    }

    /// Makes `new` (sorted, deduplicated, disjoint from the relation) the delta.
    fn advance(&mut self, new: Vec<Pair>) {
        if self.fresh {
            // The first round read the input as its delta (and recent run, shared): it
            // goes apart, unmoved.
            self.input = std::mem::take(&mut self.recent);
            self.fresh = false;
        } else if self.recent.len() * 4 > self.base.len() {
            // Fold the recent run into the base first, so the new delta stays in `recent`.
            let recent = std::mem::take(&mut self.recent);
            if self.base.len() == 0 {
                self.base = recent;
            } else {
                // One order at a time, so the old and new base never coexist in both.
                self.base.so = Arc::new(merge(&self.base.so, &recent.so));
                self.base.os = Arc::new(merge(&self.base.os, &recent.os));
            }
        }
        self.delta = Run::new(new);
        if self.delta.len() > 0 {
            // An empty recent run becomes the delta itself (shared, not copied).
            self.recent = self.recent.merge(&self.delta);
        }
    }
}

/// The working set: one relation per predicate.
#[derive(Default)]
pub(crate) struct Store {
    relations: Vec<Relation>,
    /// The predicate of each relation.
    predicates: Vec<u64>,
    index: HashMap<u64, usize>,
}

impl Store {
    /// A store holding `input`, all of it as the delta.
    pub(crate) fn new(input: Vec<Triple>) -> Self {
        let mut store = Self::default();
        store.advance(input);
        for relation in &mut store.relations {
            relation.fresh = true;
        }
        store
    }

    /// A store holding `groups` (see [`materialise_grouped`]), all of it as the delta.
    pub(crate) fn from_groups(groups: Vec<(u64, Vec<Pair>)>) -> Self {
        let mut store = Self::default();
        for (p, _) in &groups {
            store.index.insert(*p, store.predicates.len());
            store.predicates.push(*p);
        }
        store.relations = groups
            .into_par_iter()
            .map(|(_, os)| {
                let delta = Run::from_os(os);
                Relation {
                    input: Run::default(),
                    base: Run::default(),
                    recent: delta.clone(),
                    delta,
                    fresh: true,
                }
            })
            .collect();
        store
    }

    /// The relations with a non-empty delta, with their predicates.
    pub(crate) fn delta_relations(&self) -> impl Iterator<Item = (u64, &Relation)> {
        self.predicates
            .iter()
            .zip(&self.relations)
            .filter(|(_, r)| r.delta_len() > 0)
            .map(|(&p, r)| (p, r))
    }

    /// The bytes the runs hold.
    pub(crate) fn bytes(&self) -> StoreBytes {
        self.relations
            .iter()
            .map(Relation::bytes)
            .fold(StoreBytes::default(), |sum, bytes| StoreBytes {
                input: sum.input + bytes.input,
                added: sum.added + bytes.added,
                delta: sum.delta + bytes.delta,
            })
    }

    pub(crate) fn relation(&self, p: u64) -> Option<&Relation> {
        self.index.get(&p).map(|&i| &self.relations[i])
    }

    /// The relation of `p`, or every relation where `p` is open, with their predicates.
    fn relations_of(&self, p: Option<u64>) -> Box<dyn Iterator<Item = (u64, &Relation)> + '_> {
        match p {
            Some(p) => Box::new(self.relation(p).map(|r| (p, r)).into_iter()),
            None => Box::new(self.predicates.iter().copied().zip(&self.relations)),
        }
    }

    /// Adds `candidates` and makes the new ones the delta; returns the new ones.
    pub(crate) fn advance(&mut self, candidates: Vec<Triple>) -> Vec<Triple> {
        let news = self.news(candidates, true);
        let mut delta = Vec::with_capacity(news.iter().map(Vec::len).sum());
        for (pairs, &p) in news.iter().zip(&self.predicates) {
            delta.extend(pairs.iter().map(|&(s, o)| [s, p, o]));
        }
        self.install(news);
        delta
    }

    /// [`Self::advance`] for candidates known to be absent from the store (filtered
    /// against it when they were derived): skips the membership check, and returns only
    /// how many were new and whether one is a schema fact, not the facts.
    pub(crate) fn advance_new(
        &mut self,
        candidates: Vec<Triple>,
        schema: &Schema,
    ) -> (usize, bool) {
        let news = self.news(candidates, false);
        let count = news.iter().map(Vec::len).sum();
        let schema_facts = news
            .par_iter()
            .zip(self.predicates.par_iter())
            .any(|(pairs, &p)| pairs.iter().any(|&(s, o)| schema.is_schema_fact([s, p, o])));
        heap::phase("reasoner: install");
        self.install(news);
        (count, schema_facts)
    }

    /// Every pair added since the input, as facts, in no particular order: what a
    /// materialisation from the input derived.
    pub(crate) fn derived(&self) -> Vec<Triple> {
        let mut out = Vec::with_capacity(
            self.relations
                .iter()
                .map(|r| r.base.len() + r.recent.len())
                .sum(),
        );
        for (relation, &p) in self.relations.iter().zip(&self.predicates) {
            for run in [&relation.base, &relation.recent] {
                out.extend(run.so.iter().map(|&(s, o)| [s, p, o]));
            }
        }
        out
    }

    /// Makes `news` (per relation) the relations' deltas.
    fn install(&mut self, news: Vec<Vec<Pair>>) {
        self.relations
            .par_iter_mut()
            .zip(news.into_par_iter())
            .for_each(|(relation, pairs)| relation.advance(pairs));
    }

    /// The new pairs of `candidates` per relation (relations added for new predicates).
    fn news(&mut self, mut candidates: Vec<Triple>, check: bool) -> Vec<Vec<Pair>> {
        candidates.par_sort_unstable_by_key(|&[s, p, o]| (p, s, o));
        candidates.dedup();
        let chunks: Vec<&[Triple]> = candidates.chunk_by(|a, b| a[1] == b[1]).collect();
        for chunk in &chunks {
            let p = chunk[0][1];
            if !self.index.contains_key(&p) {
                self.index.insert(p, self.relations.len());
                self.relations.push(Relation::default());
                self.predicates.push(p);
            }
        }
        // Keep only facts not already known, per predicate and in parallel.
        let groups: Vec<(usize, Vec<Pair>)> = chunks
            .par_iter()
            .map(|chunk| {
                let index = self.index[&chunk[0][1]];
                let relation = &self.relations[index];
                let pairs = chunk
                    .iter()
                    .map(|&[s, _, o]| (s, o))
                    .filter(|&(s, o)| !check || !relation.contains(s, o))
                    .collect();
                (index, pairs)
            })
            .collect();
        let mut news: Vec<Vec<Pair>> = self.relations.iter().map(|_| Vec::new()).collect();
        for (index, pairs) in groups {
            news[index] = pairs;
        }
        news
    }
}

impl Source for Store {
    fn scan(&self, [s, p, o]: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        match p {
            Some(p) => {
                if let Some(relation) = self.relation(p) {
                    relation.scan(s, o, seg, &mut |s, o| f([s, p, o]));
                }
            }
            None => {
                for (relation, &p) in self.relations.iter().zip(&self.predicates) {
                    relation.scan(s, o, seg, &mut |s, o| f([s, p, o]));
                }
            }
        }
    }

    fn estimate(&self, [s, p, o]: [Option<u64>; 3], seg: Seg) -> usize {
        match p {
            Some(p) => self.relation(p).map_or(0, |r| r.estimate(s, o, seg)),
            None => self.relations.iter().map(|r| r.estimate(s, o, seg)).sum(),
        }
    }

    fn contains(&self, [s, p, o]: Triple) -> bool {
        self.relation(p).is_some_and(|r| r.contains(s, o))
    }

    fn matches_len(&self, [s, p, o]: [Option<u64>; 3], seg: Seg) -> Option<usize> {
        Some(
            self.relations_of(p)
                .map(|(_, relation)| {
                    let slices = relation.matching(s, o, seg);
                    slices.iter().map(|(pairs, ..)| pairs.len()).sum::<usize>()
                })
                .sum(),
        )
    }

    fn scan_range(
        &self,
        [s, p, o]: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        f: &mut dyn FnMut(Triple),
    ) {
        let mut at = 0;
        for (p, relation) in self.relations_of(p) {
            for (pairs, swapped, old) in relation.matching(s, o, seg) {
                let (start, end) = (at, at + pairs.len());
                at = end;
                if end <= range.start {
                    continue;
                }
                if start >= range.end {
                    return;
                }
                let part = &pairs[range.start.max(start) - start..range.end.min(end) - start];
                for &(a, b) in part {
                    let (s, o) = if swapped { (b, a) } else { (a, b) };
                    if !(old && relation.delta.contains(s, o)) {
                        f([s, p, o]);
                    }
                }
            }
        }
    }
}

/// The equality module, batch form (design §4.4): replaces `eq-rep-s/p/o`. The generic
/// rules copy a fact to each `sameAs` partner of its subject, predicate or object, and
/// every copy is copied again in the next round: O(k²) derivations per fact for a class
/// of k members. Here each new fact is expanded once to every combination of its terms'
/// classes (a term's class is the term plus its `sameAs` partners; the relation is kept
/// closed by `eq-sym` and the transitive module). A new `sameAs` pair re-expands the facts
/// that mention either side, so classes that grow later are covered.
pub(crate) struct Equality {
    same_as: u64,
    /// The module's output of the last round: already expanded, so skipped once.
    produced: HashSet<Triple>,
}

/// The `owl:sameAs` id of `rules`, if they reason with equality (`eq-rep-s`).
pub(crate) fn same_as_of(rules: &[Rule]) -> Option<u64> {
    Equality::for_rules(rules).map(|equality| equality.same_as)
}

/// The rules the equality module replaces.
pub(crate) fn is_replacement_rule(rule: &Rule) -> bool {
    matches!(rule.name.as_str(), "eq-rep-s" | "eq-rep-p" | "eq-rep-o")
}

impl Equality {
    /// The module for `rules`, if they include `eq-rep-s` (whose first atom names
    /// `owl:sameAs`).
    pub(crate) fn for_rules(rules: &[Rule]) -> Option<Self> {
        let rule = rules.iter().find(|r| r.name == "eq-rep-s")?;
        match rule.body.first()?.0[1] {
            super::ir::Term::Const(same_as) => Some(Self {
                same_as,
                produced: HashSet::new(),
            }),
            super::ir::Term::Var(_) => None,
        }
    }

    /// The class of `x`: itself and its `sameAs` partners, sorted.
    fn members<S: Source + ?Sized>(&self, store: &S, x: u64) -> Vec<u64> {
        let mut members = vec![x];
        store.scan([Some(x), Some(self.same_as), None], Seg::All, &mut |t| {
            members.push(t[2]);
        });
        members.sort_unstable();
        members.dedup();
        members
    }

    /// The expansions of the delta of `store` (see the type's docs), not yet in it.
    pub(crate) fn run<S: Source + ?Sized>(&mut self, store: &S) -> Vec<Triple> {
        if store.estimate([None, Some(self.same_as), None], Seg::All) == 0 {
            self.produced.clear();
            return Vec::new();
        }
        let same_as = self.same_as;
        let mut seeds: Vec<Triple> = Vec::new();
        let mut merged: Vec<u64> = Vec::new();
        store.scan([None, None, None], Seg::Delta, &mut |t| {
            if t[1] == same_as {
                if t[0] != t[2] {
                    merged.push(t[0]);
                }
            } else if !self.produced.contains(&t) {
                seeds.push(t);
            }
        });
        // Facts mentioning a term whose class grew.
        merged.sort_unstable();
        merged.dedup();
        for &a in &merged {
            let mut push = |t: Triple| {
                if t[1] != same_as {
                    seeds.push(t);
                }
            };
            store.scan([Some(a), None, None], Seg::All, &mut push);
            store.scan([None, None, Some(a)], Seg::All, &mut push);
            store.scan([None, Some(a), None], Seg::All, &mut push);
        }
        seeds.par_sort_unstable();
        seeds.dedup();
        let out: Vec<Triple> = seeds
            .par_iter()
            .flat_map_iter(|&[s, p, o]| {
                let (ms, mp, mo) = (
                    self.members(store, s),
                    self.members(store, p),
                    self.members(store, o),
                );
                let mut facts = Vec::new();
                for &s in &ms {
                    for &p in &mp {
                        for &o in &mo {
                            let fact = [s, p, o];
                            if !store.contains(fact) {
                                facts.push(fact);
                            }
                        }
                    }
                }
                facts
            })
            .collect();
        self.produced = out.iter().copied().collect();
        out
    }
}

/// The transitive module, batch form: for each transitive predicate, its closure by SCC
/// condensation, recomputed in any round after other rules added facts over it.
#[derive(Default)]
struct Transitive {
    /// Each predicate with whether it needs a recomputation.
    predicates: std::collections::BTreeMap<u64, bool>,
    /// New facts each predicate's last recomputation produced.
    produced: HashMap<u64, usize>,
}

impl Transitive {
    fn register(&mut self, p: u64) {
        self.predicates.entry(p).or_insert(true);
    }

    /// The closure facts of every predicate that needs it, not yet in `store`; `None` if
    /// `stop` fired.
    fn run(&mut self, store: &Store, stop: super::eval::Stop<'_>) -> Option<Vec<Triple>> {
        self.produced.clear();
        let mut out = Vec::new();
        for (&p, dirty) in &mut self.predicates {
            if !std::mem::take(dirty) {
                continue;
            }
            let Some(relation) = store.relation(p) else {
                continue;
            };
            let before = out.len();
            out.extend(
                nrese_exec::graph::transitive_closure_until(&relation.pairs(), stop)?
                    .into_iter()
                    .filter(|&(s, o)| !relation.contains(s, o))
                    .map(|(s, o)| [s, p, o]),
            );
            self.produced.insert(p, out.len() - before);
        }
        Some(out)
    }

    /// After a round: a predicate needs recomputing if other rules added facts over it.
    fn observe(&mut self, store: &Store) {
        for (p, dirty) in &mut self.predicates {
            let added = store.relation(*p).map_or(0, Relation::delta_len);
            *dirty |= added > self.produced.get(p).copied().unwrap_or(0);
        }
    }
}

/// The fact rules of `rules`, plus the list rules instantiated over `source`.
pub(crate) fn fact_rules<S: Source + ?Sized>(
    source: &S,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    diagnostics: &mut Vec<super::lists::ListDiagnostic>,
) -> Vec<Rule> {
    let mut out: Vec<Rule> = rules
        .iter()
        .filter(|r| r.head != Head::Inconsistent)
        .cloned()
        .collect();
    if let Some(vocabulary) = lists {
        let (list_rules, found) = instantiate(vocabulary, &AllFacts(source));
        out.extend(
            list_rules
                .into_iter()
                .filter(|r| r.head != Head::Inconsistent),
        );
        *diagnostics = found;
    }
    out
}

/// The consistency rules of `rules`, plus the list ones instantiated over `source`.
pub(crate) fn consistency_rules<S: Source + ?Sized>(
    source: &S,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
) -> Vec<Rule> {
    let mut out: Vec<Rule> = rules
        .iter()
        .filter(|r| r.head == Head::Inconsistent)
        .cloned()
        .collect();
    if let Some(vocabulary) = lists {
        let (list_rules, _) = instantiate(vocabulary, &AllFacts(source));
        out.extend(
            list_rules
                .into_iter()
                .filter(|r| r.head == Head::Inconsistent),
        );
    }
    out
}

/// The closure of `input` under `rules` (plus the list rules when `lists` is given).
pub fn materialise(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    materialise_owned(input.to_vec(), rules, lists, schema)
}

/// [`materialise`] taking the input by value, so the working set is its only copy.
pub fn materialise_owned(
    input: Vec<Triple>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    let clock = std::time::Instant::now();
    let store = Store::new(input);
    run(store, clock.elapsed(), rules, lists, schema, NEVER).expect("never stopped")
}

/// [`materialise_owned`], polling `stop` as [`materialise_grouped_until`] does.
pub fn materialise_owned_until(
    input: Vec<Triple>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let clock = std::time::Instant::now();
    let store = Store::new(input);
    run(store, clock.elapsed(), rules, lists, schema, stop)
}

/// [`materialise`] over input already grouped the way the working set stores it: per
/// predicate (each once), its `(object, subject)` pairs, sorted and distinct. A store can
/// stream this from a predicate-object-subject index without an intermediate triple list or
/// sort, holding about 32 bytes per input fact instead of about 72.
pub fn materialise_grouped(
    input: Vec<(u64, Vec<(u64, u64)>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    materialise_grouped_until(input, rules, lists, schema, NEVER).expect("never stopped")
}

/// [`materialise_grouped`], polling `stop` in every round, rule job and module; a fired
/// `stop` ends it with [`Interrupted`] and nothing of the partial closure.
pub fn materialise_grouped_until(
    input: Vec<(u64, Vec<(u64, u64)>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let clock = std::time::Instant::now();
    let store = Store::from_groups(input);
    run(store, clock.elapsed(), rules, lists, schema, stop)
}

/// Semi-naive evaluation to the fixpoint, from a store holding the input as its delta.
fn run(
    mut store: Store,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let check_stop = || if stop() { Err(Interrupted) } else { Ok(()) };
    let mut phases = Phases {
        load,
        ..Phases::default()
    };
    let mut result = Materialisation::default();
    let mut program = GroundProgram::default();
    let mut transitive = Transitive::default();
    let mut equality = Equality::for_rules(rules);
    let mut regrounding = true;
    loop {
        check_stop()?;
        result.rounds += 1;
        heap::phase("reasoner: grounding");
        let clock = std::time::Instant::now();
        // Rules from `evaluated` on were added this round: evaluated once in full.
        let evaluated = program.rules.len();
        if regrounding {
            let mut source = fact_rules(&store, rules, lists, &mut result.diagnostics);
            if equality.is_some() {
                source.retain(|r| !is_replacement_rule(r));
            }
            // The first round grounds everything; later ones through new schema facts
            // only (list rules in full, deduplicated).
            if result.rounds == 1 {
                program.ground(&store, schema, &source);
            } else {
                program.ground_delta(&store, schema, &source);
            }
            for &p in &program.transitive {
                transitive.register(p);
            }
        }
        phases.grounding += clock.elapsed();
        heap::phase("reasoner: joins");
        let clock = std::time::Instant::now();
        // Semi-naive variants of the evaluated rules that can match the delta, full
        // evaluation of the new ones.
        let mut candidates = program.take_facts();
        candidates.retain(|&f| !store.contains(f));
        let mut jobs = Vec::new();
        for (r, i) in program.variants(&store) {
            if r < evaluated {
                jobs.extend(Job::variant(&store, &program.rules[r], i));
            }
        }
        for rule in &program.rules[evaluated..] {
            jobs.extend(Job::full(&store, rule));
        }
        let probes = Probes::default();
        let keep = |fact| !store.contains(fact);
        candidates.extend(run_jobs_counted(&store, &jobs, &keep, stop, &probes));
        let counters = &mut result.counters;
        counters.driver_bytes_copied += jobs.iter().map(Job::copied_bytes).sum::<usize>();
        counters.probes.push(probes.probes.into_inner());
        counters.unordered_probes += probes.unordered.into_inner();
        drop(jobs);
        check_stop()?;
        phases.joins += clock.elapsed();
        heap::phase("reasoner: modules");
        let clock = std::time::Instant::now();
        candidates.extend(transitive.run(&store, stop).ok_or(Interrupted)?);
        if let Some(equality) = &mut equality {
            candidates.extend(equality.run(&store));
        }
        check_stop()?;
        phases.modules += clock.elapsed();
        heap::phase("reasoner: merge");
        let clock = std::time::Instant::now();
        // Every candidate was checked against the store, which a round doesn't change.
        let (new, schema_facts) = store.advance_new(candidates, schema);
        phases.merge += clock.elapsed();
        result.counters.store_bytes.push(store.bytes());
        if new == 0 {
            break;
        }
        transitive.observe(&store);
        regrounding = schema_facts;
    }
    result.ground_rules = program.rules.len();
    result.transitive = transitive.predicates.len();
    heap::phase("reasoner: consistency");
    let clock = std::time::Instant::now();
    program.ground(&store, schema, &consistency_rules(&store, rules, lists));
    let mut found = HashSet::new();
    check(
        &store,
        &program,
        0..program.consistency.len(),
        &[],
        &mut found,
    );
    result.violations = sorted(found);
    phases.consistency = clock.elapsed();
    result.phases = phases;
    heap::phase("reasoner: derived");
    let mut derived = store.derived();
    drop(store);
    derived.par_sort_unstable();
    result.derived = derived;
    heap::phase("reasoner: done");
    Ok(result)
}

/// Violations sorted by rule and bindings.
pub(crate) fn sorted(found: HashSet<Violation>) -> Vec<Violation> {
    let mut violations: Vec<Violation> = found.into_iter().collect();
    violations.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
    violations
}

/// Evaluates consistency instances into `found`: those in `full` over all facts, and the
/// `(instance, atom)` semi-naive variants in `variants` through the delta.
pub(crate) fn check<S: Source + ?Sized>(
    source: &S,
    program: &GroundProgram,
    full: impl IntoIterator<Item = usize>,
    variants: &[(usize, usize)],
    found: &mut HashSet<Violation>,
) {
    let record = |c: usize, bindings: &[Option<u64>], found: &mut HashSet<Violation>| {
        let (rule, grounded) = &program.consistency[c];
        // The job's bindings are sized for the grounded instance, which lacks the schema
        // variables when one of them has the highest index.
        let bindings = (0..rule.variables())
            .map(|v| {
                grounded.substitution[v]
                    .or_else(|| bindings.get(v).copied().flatten())
                    .unwrap_or(0)
            })
            .collect();
        found.insert(Violation {
            rule: rule.name.clone(),
            bindings,
        });
    };
    let mut jobs: Vec<(usize, Job<'_>)> = Vec::new();
    for c in full {
        let ground_rule = &program.consistency[c].1.rule;
        if ground_rule.body.is_empty() {
            if guards_hold(&ground_rule.guards, &program.consistency[c].1.substitution) {
                record(c, &vec![None; program.consistency[c].0.variables()], found);
            }
        } else {
            jobs.extend(Job::full(source, ground_rule).map(|job| (c, job)));
        }
    }
    for &(c, i) in variants {
        let ground_rule = &program.consistency[c].1.rule;
        jobs.extend(Job::variant(source, ground_rule, i).map(|job| (c, job)));
    }
    for (c, job) in jobs {
        job.run(source, 0..job.drivers(), &mut |bindings| {
            record(c, bindings, found)
        });
    }
}
