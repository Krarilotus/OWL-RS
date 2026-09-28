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

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

pub use super::eval::Schema;
use super::eval::{
    AllFacts, Job, RuleKey, Seg, Source, ground, ground_delta, guards_hold, instantiate_head,
    rule_key, run_jobs, transitive_predicate,
};
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

/// The pairs of one predicate.
#[derive(Default)]
pub(crate) struct Relation {
    pub(crate) so: Vec<Pair>,
    /// `(object, subject)` pairs.
    os: Vec<Pair>,
    pub(crate) delta_so: Vec<Pair>,
    delta_os: Vec<Pair>,
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
    pub(crate) fn contains(&self, s: u64, o: u64) -> bool {
        self.so.binary_search(&(s, o)).is_ok()
    }

    fn segment(&self, seg: Seg) -> (&[Pair], &[Pair]) {
        match seg {
            Seg::Delta => (&self.delta_so, &self.delta_os),
            Seg::Old | Seg::All => (&self.so, &self.os),
        }
    }

    /// Calls `f` with every `(s, o)` matching the bound positions in `seg`.
    fn scan(&self, s: Option<u64>, o: Option<u64>, seg: Seg, f: &mut dyn FnMut(u64, u64)) {
        let (so, os) = self.segment(seg);
        let old = seg == Seg::Old && !self.delta_so.is_empty();
        let keep = |s: u64, o: u64| !old || self.delta_so.binary_search(&(s, o)).is_err();
        match (s, o) {
            (Some(s), Some(o)) => {
                if so.binary_search(&(s, o)).is_ok() && keep(s, o) {
                    f(s, o);
                }
            }
            (Some(s), None) => {
                for &(_, o) in range(so, s) {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
            (None, Some(o)) => {
                for &(_, s) in range(os, o) {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
            (None, None) => {
                for &(s, o) in so {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
        }
    }

    /// An upper bound on the matches of the bound positions in `seg`.
    fn estimate(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> usize {
        let (so, os) = self.segment(seg);
        match (s, o) {
            (Some(s), Some(o)) => usize::from(so.binary_search(&(s, o)).is_ok()),
            (Some(s), None) => range(so, s).len(),
            (None, Some(o)) => range(os, o).len(),
            (None, None) => so.len(),
        }
    }

    /// Makes `new` (sorted, deduplicated, disjoint from the relation) the delta.
    fn advance(&mut self, new: Vec<Pair>) {
        let mut new_os: Vec<Pair> = new.iter().map(|&(s, o)| (o, s)).collect();
        new_os.sort_unstable();
        if !new.is_empty() {
            self.so = merge(&self.so, &new);
            self.os = merge(&self.os, &new_os);
        }
        self.delta_so = new;
        self.delta_os = new_os;
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

    pub(crate) fn relation(&self, p: u64) -> Option<&Relation> {
        self.index.get(&p).map(|&i| &self.relations[i])
    }

    /// Adds `candidates` and makes the new ones the delta; returns the new ones.
    pub(crate) fn advance(&mut self, mut candidates: Vec<Triple>) -> Vec<Triple> {
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
                    .filter(|&(s, o)| !relation.contains(s, o))
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
                nrese_exec::graph::transitive_closure(&relation.so)
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
            let added = store.relation(*p).map_or(0, |r| r.delta_so.len());
            *dirty |= added > self.produced.get(p).copied().unwrap_or(0);
        }
    }
}

/// The fact rules of `rules`, plus the list rules instantiated over `source`.
fn fact_rules<S: Source + ?Sized>(
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

/// The ground program: evaluated rules, new rules, facts from bodiless instances and the
/// transitive predicates.
#[derive(Default)]
struct Program {
    rules: Vec<Rule>,
    known: HashSet<RuleKey>,
    fresh: Vec<Rule>,
    facts: Vec<Triple>,
    transitive: Transitive,
}

impl Program {
    /// Files one grounded rule.
    fn add(&mut self, rule: Rule) {
        if let Some(p) = transitive_predicate(&rule) {
            self.transitive.register(p);
        } else if rule.body.is_empty() {
            if let Head::Facts(heads) = &rule.head {
                self.facts
                    .extend(heads.iter().map(|h| instantiate_head(h, &[])));
            }
        } else if self.known.insert(rule_key(&rule)) {
            self.fresh.push(rule);
        }
    }

    /// Grounds `rules` over all of `source` (`full`) or through its delta only.
    fn ground<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rules: &[Rule],
        full: bool,
    ) {
        for rule in rules {
            if let Some(p) = transitive_predicate(rule) {
                self.transitive.register(p);
                continue;
            }
            let mut grounded = Vec::new();
            // Rules without schema atoms (list rules among them) have no delta grounding.
            let has_schema_atoms = rule.body.iter().any(|a| schema.is_schema_atom(a));
            if full || !has_schema_atoms {
                ground(source, schema, rule, &mut |g| grounded.push(g.rule));
            } else {
                ground_delta(source, schema, rule, &mut |g| grounded.push(g.rule));
            }
            for rule in grounded {
                self.add(rule);
            }
        }
    }
}

/// The closure of `input` under `rules` (plus the list rules when `lists` is given).
pub fn materialise(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    let clock = std::time::Instant::now();
    let mut phases = Phases::default();
    let mut store = Store::new(input.to_vec());
    phases.load = clock.elapsed();
    let mut result = Materialisation::default();
    let mut derived: Vec<Triple> = Vec::new();
    let mut program = Program::default();
    let mut regrounding = true;
    loop {
        result.rounds += 1;
        let clock = std::time::Instant::now();
        if regrounding {
            let source_rules = fact_rules(&store, rules, lists, &mut result.diagnostics);
            // The first round grounds everything; later ones through new schema facts
            // only. List rules are re-instantiated in full (deduplicated by `known`).
            program.ground(&store, schema, &source_rules, result.rounds == 1);
        }
        phases.grounding += clock.elapsed();
        let clock = std::time::Instant::now();
        // Semi-naive variants of the rules already evaluated, full evaluation of new ones.
        let mut jobs = Vec::new();
        for rule in &program.rules {
            for i in 0..rule.body.len() {
                jobs.extend(Job::variant(&store, rule, i));
            }
        }
        for rule in &program.fresh {
            jobs.extend(Job::full(&store, rule));
        }
        let mut candidates = std::mem::take(&mut program.facts);
        candidates.extend(run_jobs(&store, &jobs, &|fact| !store.contains(fact)));
        drop(jobs);
        phases.joins += clock.elapsed();
        let clock = std::time::Instant::now();
        candidates.extend(program.transitive.run(&store));
        phases.modules += clock.elapsed();
        let clock = std::time::Instant::now();
        let fresh = std::mem::take(&mut program.fresh);
        program.rules.extend(fresh);
        let delta = store.advance(candidates);
        phases.merge += clock.elapsed();
        if delta.is_empty() {
            break;
        }
        program.transitive.observe(&store);
        regrounding = delta.iter().any(|&t| schema.is_schema_fact(t));
        derived.extend(delta);
    }
    result.ground_rules = program.rules.len();
    result.transitive = program.transitive.predicates.len();
    let clock = std::time::Instant::now();
    result.violations = violations(&store, rules, lists, schema);
    phases.consistency = clock.elapsed();
    result.phases = phases;
    derived.par_sort_unstable();
    result.derived = derived;
    result
}

/// The consistency rules (plus list ones) of `rules`.
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

/// Adds the violations of consistency rule `rule` to `found`. With `delta_only`, only
/// those that use a fact of the delta: the semi-naive variants of the existing instances,
/// and new instances (grounded through delta schema facts) in full.
pub(crate) fn violations_of<S: Source + ?Sized>(
    source: &S,
    schema: &Schema,
    rule: &Rule,
    delta_only: bool,
    found: &mut HashSet<Violation>,
) {
    let variables = rule.variables();
    let record =
        |substitution: &[Option<u64>], bindings: &[Option<u64>], found: &mut HashSet<Violation>| {
            let bindings = (0..variables)
                .map(|v| substitution[v].or(bindings[v]).unwrap_or(0))
                .collect();
            found.insert(Violation {
                rule: rule.name.clone(),
                bindings,
            });
        };
    let evaluate =
        |grounded: super::eval::Grounded, variants: bool, found: &mut HashSet<Violation>| {
            let ground_rule = &grounded.rule;
            if ground_rule.body.is_empty() {
                if guards_hold(&ground_rule.guards, &grounded.substitution) {
                    record(&grounded.substitution, &vec![None; variables], found);
                }
                return;
            }
            let jobs: Vec<Job<'_>> = if variants {
                (0..ground_rule.body.len())
                    .filter_map(|i| Job::variant(source, ground_rule, i))
                    .collect()
            } else {
                Job::full(source, ground_rule).into_iter().collect()
            };
            for job in jobs {
                job.run(source, 0..job.drivers(), &mut |bindings| {
                    record(&grounded.substitution, bindings, found)
                });
            }
        };
    let mut all = Vec::new();
    ground(source, schema, rule, &mut |g| all.push(g));
    if !delta_only {
        for grounded in all {
            evaluate(grounded, false, found);
        }
        return;
    }
    // Existing instances through the delta, new instances (through delta schema facts)
    // in full.
    for grounded in all {
        evaluate(grounded, true, found);
    }
    let mut new = Vec::new();
    ground_delta(source, schema, rule, &mut |g| new.push(g));
    for grounded in new {
        evaluate(grounded, false, found);
    }
}

/// The consistency rules' violations on the closure in `store`.
fn violations(
    store: &Store,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Vec<Violation> {
    let mut found = HashSet::new();
    for rule in consistency_rules(store, rules, lists) {
        violations_of(store, schema, &rule, false, &mut found);
    }
    let mut violations: Vec<Violation> = found.into_iter().collect();
    violations.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
    violations
}
