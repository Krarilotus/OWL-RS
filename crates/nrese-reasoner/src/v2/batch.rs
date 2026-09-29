//! The batch executor (reasoner-v2 design §4.1): full materialisation by semi-naive
//! evaluation over a vertically partitioned working set.
//!
//! - **Working set.** One [`Relation`] per predicate: its pairs sorted by subject and by
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
//! The naive evaluator ([`super::naive`]) is the oracle: both compute the same closure.

use std::sync::Arc;

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

pub use super::eval::Schema;
use super::eval::{AllFacts, GroundProgram, Job, Seg, Source, guards_hold, run_jobs};
use super::ir::{Head, Rule};
use super::lists::{ListVocabulary, instantiate};
use super::naive::{Triple, Violation};

type Pair = (u64, u64);

/// The result of [`materialise`].
#[derive(Debug, Default)]
pub struct Materialisation {
    /// Facts derived beyond the input, sorted, without duplicates.
    pub derived: Vec<Triple>,
    /// Consistency violations on the closure, sorted.
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<String>,
    pub rounds: usize,
    /// Rules after grounding, at the end.
    pub ground_rules: usize,
    /// Predicates closed by the transitive module.
    pub transitive: usize,
    /// Time per phase: grounding, rule joins, modules, merging, consistency.
    pub phases: Phases,
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

/// The pairs of one predicate, in two sorted runs: a large base and a small recent run
/// that takes each round's delta. The recent run is folded into the base once it reaches
/// a quarter of its size, so a round costs its delta plus the recent run, not the whole
/// relation, and the total merge work stays O(n log n).
#[derive(Default)]
pub(crate) struct Relation {
    base: Run,
    recent: Run,
    /// The last round's pairs (also in `recent`).
    delta: Run,
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
        self.base.contains(s, o) || self.recent.contains(s, o)
    }

    /// Every pair, in no particular order.
    pub(crate) fn pairs(&self) -> Vec<Pair> {
        let mut out = Vec::with_capacity(self.base.len() + self.recent.len());
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
                self.base.scan(s, o, &all, f);
                self.recent.scan(s, o, &all, f);
            }
            // The delta is in `recent` only, so the base needs no filter.
            Seg::Old => {
                self.base.scan(s, o, &all, f);
                let old = |s: u64, o: u64| !self.delta.contains(s, o);
                self.recent.scan(s, o, &old, f);
            }
        }
    }

    /// An upper bound on the matches of the bound positions in `seg`.
    fn estimate(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> usize {
        match seg {
            Seg::Delta => self.delta.estimate(s, o),
            Seg::Old | Seg::All => self.base.estimate(s, o) + self.recent.estimate(s, o),
        }
    }

    /// Makes `new` (sorted, deduplicated, disjoint from the relation) the delta.
    fn advance(&mut self, new: Vec<Pair>) {
        // Fold the recent run into the base first, so the new delta stays in `recent`.
        if self.recent.len() * 4 > self.base.len() {
            let recent = std::mem::take(&mut self.recent);
            self.base = self.base.merge(&recent);
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
                    base: Run::default(),
                    recent: delta.clone(),
                    delta,
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

    pub(crate) fn relation(&self, p: u64) -> Option<&Relation> {
        self.index.get(&p).map(|&i| &self.relations[i])
    }

    /// Adds `candidates` and makes the new ones the delta; returns the new ones.
    pub(crate) fn advance(&mut self, candidates: Vec<Triple>) -> Vec<Triple> {
        self.advance_checked(candidates, true)
    }

    /// [`Self::advance`] for candidates known to be absent from the store (filtered
    /// against it when they were derived): skips the membership check.
    pub(crate) fn advance_new(&mut self, candidates: Vec<Triple>) -> Vec<Triple> {
        self.advance_checked(candidates, false)
    }

    fn advance_checked(&mut self, mut candidates: Vec<Triple>, check: bool) -> Vec<Triple> {
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
        let mut delta = Vec::with_capacity(news.iter().map(Vec::len).sum());
        for (pairs, &p) in news.iter().zip(&self.predicates) {
            delta.extend(pairs.iter().map(|&(s, o)| [s, p, o]));
        }
        self.relations
            .par_iter_mut()
            .zip(news.into_par_iter())
            .for_each(|(relation, pairs)| relation.advance(pairs));
        delta
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

    /// The closure facts of every predicate that needs it, not yet in `store`.
    fn run(&mut self, store: &Store) -> Vec<Triple> {
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
                nrese_exec::graph::transitive_closure(&relation.pairs())
                    .into_iter()
                    .filter(|&(s, o)| !relation.contains(s, o))
                    .map(|(s, o)| [s, p, o]),
            );
            self.produced.insert(p, out.len() - before);
        }
        out
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
    diagnostics: &mut Vec<String>,
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
    run(store, clock.elapsed(), rules, lists, schema)
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
    let clock = std::time::Instant::now();
    let store = Store::from_groups(input);
    run(store, clock.elapsed(), rules, lists, schema)
}

/// Semi-naive evaluation to the fixpoint, from a store holding the input as its delta.
fn run(
    mut store: Store,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    let mut phases = Phases {
        load,
        ..Phases::default()
    };
    let mut result = Materialisation::default();
    let mut derived: Vec<Triple> = Vec::new();
    let mut program = GroundProgram::default();
    let mut transitive = Transitive::default();
    let mut equality = Equality::for_rules(rules);
    let mut regrounding = true;
    loop {
        result.rounds += 1;
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
        candidates.extend(run_jobs(&store, &jobs, &|fact| !store.contains(fact)));
        drop(jobs);
        phases.joins += clock.elapsed();
        let clock = std::time::Instant::now();
        candidates.extend(transitive.run(&store));
        if let Some(equality) = &mut equality {
            candidates.extend(equality.run(&store));
        }
        phases.modules += clock.elapsed();
        let clock = std::time::Instant::now();
        // Every candidate was checked against the store, which a round doesn't change.
        let delta = store.advance_new(candidates);
        phases.merge += clock.elapsed();
        if delta.is_empty() {
            break;
        }
        transitive.observe(&store);
        regrounding = delta.iter().any(|&t| schema.is_schema_fact(t));
        derived.extend(delta);
    }
    result.ground_rules = program.rules.len();
    result.transitive = transitive.predicates.len();
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
    derived.par_sort_unstable();
    result.derived = derived;
    result
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
        let bindings = (0..rule.variables())
            .map(|v| grounded.substitution[v].or(bindings[v]).unwrap_or(0))
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
