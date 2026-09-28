//! The delta executor (reasoner-v2 design §4.2 and §5): keeps a materialisation current
//! under changes to the asserted facts, at a cost that follows the change, not the
//! dataset. It reads the state through [`Base`] (the store's indexes) and returns the
//! changes to the inferred facts.
//!
//! One [`update`] runs three phases over the same compiled program as the batch executor
//! ([`super::eval`]):
//!
//! 1. **Overdelete** (DRed; Gupta, Mumick and Subrahmanian, SIGMOD 1993). Every inferred
//!    fact with a derivation that uses a deleted fact is removed, transitively: semi-naive
//!    evaluation over the old state with the deleted facts as the delta. Rule instances
//!    that exist because of a deleted schema fact are found by grounding through it
//!    ([`super::eval::ground_delta`]) and evaluated in full.
//! 2. **Rederive.** Each overdeleted fact (and each deleted asserted fact) that still has a
//!    one-step derivation from the remaining facts is put back
//!    ([`super::eval::derivations`]).
//! 3. **Insert.** Semi-naive evaluation seeded with the rederived and the inserted facts.
//!    Rule instances created by new schema facts are evaluated once over all facts.
//!    Transitive properties are closed per new edge `(a, b)` as predecessors(a) ×
//!    successors(b) over the already closed relation, edge by edge, so a batch of edges
//!    costs the pairs it adds rather than a recomputation.
//!
//! Consistency rules are then evaluated through the facts the change added: deletes can't
//! create violations, and the state before the change passed the same check.
//!
//! List axioms are instantiated from the facts ([`super::lists`]), so a list rule appears
//! or vanishes with its list: when overdeletion reaches list structure, every list rule of
//! the old state that no longer exists is evaluated in full and its consequences are
//! overdeleted; new list rules in phase 3 are evaluated in full.
//!
//! DRed overdeletes whole transitive components on deletes inside them; B/F (R5) replaces
//! it.

use hashbrown::HashSet;
use rayon::prelude::*;

use super::batch::{Store, consistency_rules, violations_of};
use super::eval::{
    AllFacts, Job, RuleKey, Schema, Seg, Source, derivations, ground, ground_delta,
    instantiate_head, rule_key, run_jobs, transitive_predicate,
};
use super::ir::{Atom, Head, Rule, Term};
use super::lists::{ListVocabulary, instantiate};
use super::naive::{Triple, Violation};

/// The materialised state: asserted facts after the change, and inferred facts before it.
pub trait Base: Sync {
    /// Calls `f` with every fact matching `pattern` (`None` = any term). A fact may be
    /// reported more than once (asserted in several graphs).
    fn scan(&self, pattern: [Option<u64>; 3], f: &mut dyn FnMut(Triple));
    /// An upper bound on the matches of `pattern`; zero only if nothing matches.
    fn estimate(&self, pattern: [Option<u64>; 3]) -> usize;
    fn contains(&self, fact: Triple) -> bool;
    /// Whether `fact` is asserted (after the change).
    fn is_asserted(&self, fact: Triple) -> bool;
}

/// What [`update`] computed.
#[derive(Debug, Default)]
pub struct Update {
    /// Facts to add to the inferred stack.
    pub insert: Vec<Triple>,
    /// Facts to remove from the inferred stack.
    pub remove: Vec<Triple>,
    /// Consistency violations that involve a fact the change added.
    pub violations: Vec<Violation>,
    pub rounds: usize,
}

/// The state seen through a [`Base`], minus `hidden`, plus `extra` (whose delta is the
/// [`Seg::Delta`] segment).
struct Overlay<'a, B: Base + ?Sized> {
    base: &'a B,
    hidden: &'a HashSet<Triple>,
    extra: &'a Store,
}

impl<B: Base + ?Sized> Overlay<'_, B> {
    fn in_base(&self, fact: Triple) -> bool {
        !self.hidden.contains(&fact) && !self.extra.contains(fact)
    }
}

impl<B: Base + ?Sized> Source for Overlay<'_, B> {
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        self.extra.scan(pattern, seg, f);
        if seg != Seg::Delta {
            self.base.scan(pattern, &mut |fact| {
                if self.in_base(fact) {
                    f(fact);
                }
            });
        }
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        let extra = self.extra.estimate(pattern, seg);
        match seg {
            Seg::Delta => extra,
            Seg::Old | Seg::All => extra + self.base.estimate(pattern),
        }
    }

    fn contains(&self, fact: Triple) -> bool {
        self.extra.contains(fact) || (!self.hidden.contains(&fact) && self.base.contains(fact))
    }
}

/// The transitivity rule over `p`, for evaluation where the module doesn't apply
/// (overdeletion, rederivation).
fn transitivity(p: u64) -> Rule {
    let (x, y, z) = (Term::Var(0), Term::Var(1), Term::Var(2));
    Rule {
        name: "prp-trp".to_owned(),
        body: vec![Atom([x, Term::Const(p), y]), Atom([y, Term::Const(p), z])],
        guards: Vec::new(),
        head: Head::Facts(vec![Atom([x, Term::Const(p), z])]),
    }
}

/// The ground program over a state: rules, transitive predicates and bodiless facts.
#[derive(Default)]
struct Program {
    rules: Vec<Rule>,
    known: HashSet<RuleKey>,
    transitive: std::collections::BTreeSet<u64>,
    facts: Vec<Triple>,
}

impl Program {
    /// Files `rule`; returns it if it is new and has a body.
    fn add(&mut self, rule: Rule) -> Option<Rule> {
        if let Some(p) = transitive_predicate(&rule) {
            self.transitive.insert(p);
            None
        } else if rule.body.is_empty() {
            if let Head::Facts(heads) = &rule.head {
                self.facts
                    .extend(heads.iter().map(|h| instantiate_head(h, &[])));
            }
            None
        } else if self.known.insert(rule_key(&rule)) {
            self.rules.push(rule.clone());
            Some(rule)
        } else {
            None
        }
    }

    /// Grounds `rules` over all of `source`.
    fn full<S: Source + ?Sized>(source: &S, schema: &Schema, rules: &[Rule]) -> Self {
        let mut program = Self::default();
        for rule in rules {
            if let Some(p) = transitive_predicate(rule) {
                program.transitive.insert(p);
                continue;
            }
            let mut grounded = Vec::new();
            ground(source, schema, rule, &mut |g| grounded.push(g.rule));
            for rule in grounded {
                program.add(rule);
            }
        }
        program
    }

    /// Grounds `rules` through the delta of `source`; returns the new rules.
    fn extend_delta<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rules: &[Rule],
    ) -> Vec<Rule> {
        let mut grounded = Vec::new();
        for rule in rules {
            if transitive_predicate(rule).is_some() {
                continue;
            }
            ground_delta(source, schema, rule, &mut |g| grounded.push(g.rule));
        }
        grounded
            .into_iter()
            .filter_map(|rule| self.add(rule))
            .collect()
    }

    /// Every rule, with the transitive predicates as rules.
    fn with_transitivity(&self) -> Vec<Rule> {
        let mut rules = self.rules.clone();
        rules.extend(self.transitive.iter().map(|&p| transitivity(p)));
        rules
    }
}

/// The source rules: the fact rules, plus the list rules instantiated over `source`.
fn source_rules<S: Source + ?Sized>(
    source: &S,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
) -> Vec<Rule> {
    let mut out: Vec<Rule> = rules
        .iter()
        .filter(|r| r.head != Head::Inconsistent)
        .cloned()
        .collect();
    if let Some(vocabulary) = lists {
        let (list_rules, _) = instantiate(vocabulary, &AllFacts(source));
        out.extend(
            list_rules
                .into_iter()
                .filter(|r| r.head != Head::Inconsistent),
        );
    }
    out
}

/// The fact rules [`super::lists`] instantiates over `source`.
fn list_rules<S: Source + ?Sized>(source: &S, lists: Option<&ListVocabulary>) -> Vec<Rule> {
    let Some(vocabulary) = lists else {
        return Vec::new();
    };
    let (rules, _) = instantiate(vocabulary, &AllFacts(source));
    rules
        .into_iter()
        .filter(|r| r.head != Head::Inconsistent)
        .collect()
}

/// Whether `fact` is part of a list (structure or axiom), whose rules [`super::lists`]
/// instantiates from the facts.
fn is_list_fact(lists: Option<&ListVocabulary>, fact: Triple) -> bool {
    lists.is_some_and(|l| l.is_list_fact(fact))
}

/// Maintains the materialisation of `base` under `rules` for the asserted facts
/// `inserted` and `deleted`, which `base` already reflects.
pub fn update<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Update {
    let mut result = Update::default();
    let no_hidden = HashSet::new();

    // 1. Overdelete, over the old state: `base` plus the deleted facts.
    let mut overdeleted: HashSet<Triple> = HashSet::new();
    if !deleted.is_empty() {
        let mut extra = Store::new(deleted.to_vec());
        let (program, source, old_lists) = {
            let old = Overlay {
                base,
                hidden: &no_hidden,
                extra: &extra,
            };
            let source = source_rules(&old, rules, lists);
            (
                Program::full(&old, schema, &source),
                source,
                list_rules(&old, lists),
            )
        };
        let program_rules = program.with_transitivity();
        let mut vanished_done: HashSet<RuleKey> = HashSet::new();
        loop {
            result.rounds += 1;
            let old = Overlay {
                base,
                hidden: &no_hidden,
                extra: &extra,
            };
            // Instances through a deleted schema fact, known or not, are evaluated in full.
            let mut scratch = Program::default();
            let mut fresh = scratch.extend_delta(&old, schema, &source);
            fresh.extend(scratch.transitive.iter().map(|&p| transitivity(p)));
            // List rules whose list lost a fact: in full as well.
            let mut delta_facts = Vec::new();
            extra.scan([None, None, None], Seg::Delta, &mut |f| delta_facts.push(f));
            if delta_facts.iter().any(|&f| is_list_fact(lists, f)) {
                let mut gone: HashSet<Triple> = overdeleted.clone();
                gone.extend(delta_facts.iter().copied());
                gone.extend(deleted.iter().copied());
                let empty = Store::default();
                let now = Overlay {
                    base,
                    hidden: &gone,
                    extra: &empty,
                };
                let current: HashSet<RuleKey> =
                    list_rules(&now, lists).iter().map(rule_key).collect();
                for rule in &old_lists {
                    let key = rule_key(rule);
                    if current.contains(&key) || !vanished_done.insert(key) {
                        continue;
                    }
                    match transitive_predicate(rule) {
                        Some(p) => fresh.push(transitivity(p)),
                        None if rule.body.is_empty() => {
                            if let Head::Facts(heads) = &rule.head {
                                scratch
                                    .facts
                                    .extend(heads.iter().map(|h| instantiate_head(h, &[])));
                            }
                        }
                        None => fresh.push(rule.clone()),
                    }
                }
            }
            let mut jobs = Vec::new();
            for rule in &program_rules {
                for i in 0..rule.body.len() {
                    jobs.extend(Job::variant(&old, rule, i));
                }
            }
            for rule in &fresh {
                jobs.extend(Job::full(&old, rule));
            }
            // Only inferred facts are overdeleted; asserted ones stay.
            let overdeletable =
                |f: Triple| base.contains(f) && !base.is_asserted(f) && !extra.contains(f);
            let mut candidates = run_jobs(&old, &jobs, &overdeletable);
            candidates.extend(scratch.facts.iter().copied().filter(|&f| overdeletable(f)));
            drop(jobs);
            let delta = extra.advance(candidates);
            if delta.is_empty() {
                break;
            }
            overdeleted.extend(delta);
        }
    }

    // 2. Rederive: overdeleted and deleted facts with a derivation from what remains. The
    // program is grounded over the settled state, without the inserted facts: rule
    // instances that exist because of them are then new in phase 3 and evaluated in full.
    let mut hidden: HashSet<Triple> = overdeleted.clone();
    hidden.extend(deleted.iter().copied());
    let mut unsettled = hidden.clone();
    unsettled.extend(inserted.iter().copied());
    let empty = Store::default();
    let remaining = Overlay {
        base,
        hidden: &unsettled,
        extra: &empty,
    };
    let source = source_rules(&remaining, rules, lists);
    let mut program = Program::full(&remaining, schema, &source);
    let checks = program.with_transitivity();
    let bodiless: HashSet<Triple> = program.facts.drain(..).collect();
    let candidates: Vec<Triple> = overdeleted.iter().chain(deleted).copied().collect();
    let rederived: Vec<Triple> = candidates
        .par_iter()
        .copied()
        .filter(|&fact| !base.is_asserted(fact))
        .filter(|&fact| {
            bodiless.contains(&fact)
                || checks.iter().any(|rule| {
                    let mut found = false;
                    derivations(&remaining, rule, fact, Seg::All, &mut |_| {
                        found = true;
                        true
                    });
                    found
                })
        })
        .collect();

    // 3. Insert: semi-naive from the rederived and inserted facts.
    let mut seeds = rederived;
    seeds.extend_from_slice(inserted);
    let mut extra = Store::new(seeds);
    let mut module_output: HashSet<Triple> = HashSet::new();
    loop {
        result.rounds += 1;
        let state = Overlay {
            base,
            hidden: &hidden,
            extra: &extra,
        };
        // Rule instances through new schema facts, and list rules from new list facts.
        let transitive_before = program.transitive.clone();
        let mut fresh = program.extend_delta(&state, schema, &source);
        {
            let mut delta_facts = Vec::new();
            extra.scan([None, None, None], Seg::Delta, &mut |f| delta_facts.push(f));
            if delta_facts.iter().any(|&f| is_list_fact(lists, f))
                && let Some(vocabulary) = lists
            {
                let (list_rules, _) = instantiate(vocabulary, &AllFacts(&state));
                for rule in list_rules
                    .into_iter()
                    .filter(|r| r.head != Head::Inconsistent)
                {
                    fresh.extend(program.add(rule));
                }
            }
        }
        let fresh_keys: HashSet<RuleKey> = fresh.iter().map(rule_key).collect();
        let mut jobs = Vec::new();
        for rule in &program.rules {
            if fresh_keys.contains(&rule_key(rule)) {
                continue;
            }
            for i in 0..rule.body.len() {
                jobs.extend(Job::variant(&state, rule, i));
            }
        }
        for rule in &fresh {
            jobs.extend(Job::full(&state, rule));
        }
        let mut candidates = std::mem::take(&mut program.facts);
        candidates.retain(|&f| !state.contains(f));
        candidates.extend(run_jobs(&state, &jobs, &|fact| !state.contains(fact)));
        drop(jobs);
        // Transitive properties: newly declared ones closed in full (SCC condensation), the
        // others per new edge. The module's own pairs keep the relation closed, so the
        // next round skips them; edges from other rules are closed then.
        let mut produced: Vec<Triple> = Vec::new();
        for &p in &program.transitive {
            if !transitive_before.contains(&p) {
                let mut all = Vec::new();
                state.scan([None, Some(p), None], Seg::All, &mut |t| {
                    all.push((t[0], t[2]))
                });
                produced.extend(
                    nrese_exec::graph::transitive_closure(&all)
                        .into_iter()
                        .map(|(s, o)| [s, p, o])
                        .filter(|&f| !state.contains(f)),
                );
                continue;
            }
            let mut edges = Vec::new();
            state.scan([None, Some(p), None], Seg::Delta, &mut |t| {
                if !module_output.contains(&t) {
                    edges.push((t[0], t[2]));
                }
            });
            produced.extend(close_edges(&state, p, &edges));
        }
        module_output = produced.iter().copied().collect();
        candidates.extend(produced);
        let delta = extra.advance(candidates);
        if delta.is_empty() {
            break;
        }
    }

    // The changes to the inferred stack.
    let mut added: Vec<Triple> = Vec::new();
    extra.scan([None, None, None], Seg::All, &mut |f| added.push(f));
    added.sort_unstable();
    added.dedup();
    let visible_before = |f: Triple| base.contains(f) && !hidden.contains(&f);
    result.insert = added
        .iter()
        .copied()
        .filter(|&f| !base.is_asserted(f) && !(visible_before(f) || overdeleted.contains(&f)))
        .collect();
    let kept: HashSet<Triple> = added.iter().copied().collect();
    result.remove = overdeleted
        .iter()
        .copied()
        .filter(|f| !kept.contains(f))
        .collect();
    result.remove.sort_unstable();

    // Consistency through the facts the change added. The overlay holds every fact of
    // phase 3 (rederived ones are hidden in `base`), with the new ones as its delta.
    let inserted_set: HashSet<Triple> = inserted.iter().copied().collect();
    let (new_facts, restored): (Vec<Triple>, Vec<Triple>) = added.iter().copied().partition(|&f| {
        inserted_set.contains(&f) || (!visible_before(f) && !overdeleted.contains(&f))
    });
    let mut fresh = Store::new(restored);
    fresh.advance(new_facts);
    let state = Overlay {
        base,
        hidden: &hidden,
        extra: &fresh,
    };
    let mut found = HashSet::new();
    for rule in consistency_rules(&state, rules, lists) {
        violations_of(&state, schema, &rule, true, &mut found);
    }
    result.violations = found.into_iter().collect();
    result
        .violations
        .sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
    result
}

/// Closes `p` after adding `edges` to a relation that is transitively closed apart from
/// them: for each edge `(a, b)`, every predecessor of `a` (and `a`) gets every successor
/// of `b` (and `b`). Edges are processed one at a time over the relation including the
/// pairs added so far, which keeps it closed. Returns the new pairs.
fn close_edges<S: Source + ?Sized>(state: &S, p: u64, edges: &[(u64, u64)]) -> Vec<Triple> {
    use hashbrown::HashMap;
    let mut added: HashSet<(u64, u64)> = HashSet::new();
    let mut successors: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut predecessors: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut out = Vec::new();
    for &(a, b) in edges {
        let mut before = vec![a];
        state.scan([None, Some(p), Some(a)], Seg::All, &mut |t| {
            before.push(t[0])
        });
        before.extend(predecessors.get(&a).into_iter().flatten().copied());
        let mut after = vec![b];
        state.scan([Some(b), Some(p), None], Seg::All, &mut |t| {
            after.push(t[2])
        });
        after.extend(successors.get(&b).into_iter().flatten().copied());
        before.sort_unstable();
        before.dedup();
        after.sort_unstable();
        after.dedup();
        for &x in &before {
            for &y in &after {
                let fact = [x, p, y];
                if !added.contains(&(x, y)) && !state.contains(fact) {
                    added.insert((x, y));
                    successors.entry(x).or_default().push(y);
                    predecessors.entry(y).or_default().push(x);
                    out.push(fact);
                }
            }
        }
    }
    out
}

/// A [`Base`] over in-memory facts, for tests and tools.
pub struct MemoryBase {
    facts: Store,
    asserted: HashSet<Triple>,
}

impl MemoryBase {
    pub fn new(asserted: &[Triple], inferred: &[Triple]) -> Self {
        let mut all = asserted.to_vec();
        all.extend_from_slice(inferred);
        Self {
            facts: Store::new(all),
            asserted: asserted.iter().copied().collect(),
        }
    }
}

impl Base for MemoryBase {
    fn scan(&self, pattern: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        self.facts.scan(pattern, Seg::All, f);
    }

    fn estimate(&self, pattern: [Option<u64>; 3]) -> usize {
        self.facts.estimate(pattern, Seg::All)
    }

    fn contains(&self, fact: Triple) -> bool {
        self.facts.contains(fact)
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.asserted.contains(&fact)
    }
}
