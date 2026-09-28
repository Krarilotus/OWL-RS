//! The delta executor (reasoner-v2 design §4.2 and §5): keeps a materialisation current
//! under changes to the asserted facts, at a cost that follows the change, not the
//! dataset. It reads the state through [`Base`] (the store's indexes) and returns the
//! changes to the inferred facts.
//!
//! One [`update`] runs three phases over the ground program of the batch executor
//! ([`GroundProgram`]):
//!
//! 1. **Overdelete** (DRed; Gupta, Mumick and Subrahmanian, SIGMOD 1993). Every inferred
//!    fact with a derivation that uses a deleted fact is removed, transitively: semi-naive
//!    evaluation over the old state with the deleted facts as the delta. Rule instances
//!    that exist because of a deleted schema fact are found by grounding through it
//!    ([`super::eval::ground_delta`]) and evaluated in full, as are list rules whose list
//!    lost a fact.
//! 2. **Rederive.** Each overdeleted fact (and each deleted asserted fact) that still has a
//!    one-step derivation from the remaining facts is put back
//!    ([`GroundProgram::derivable`], which finds the candidate rules by head).
//! 3. **Insert.** Semi-naive evaluation seeded with the rederived and the inserted facts.
//!    Rule instances created by new schema or list facts are evaluated once over all
//!    facts. Transitive properties are closed per new edge `(a, b)` as predecessors(a) ×
//!    successors(b) over the already closed relation, edge by edge, so a batch of edges
//!    costs the pairs it adds rather than a recomputation.
//!
//! Consistency rules are then evaluated through the facts the change added: deletes can't
//! create violations, and the state before the change passed the same check.
//!
//! **The ground program is the expensive part to build** (grounding every rule against
//! the TBox, instantiating the list axioms). [`update`] takes the program of the state
//! before the change (see [`program`]) and returns the new one only if the change altered
//! it, so commits that don't touch the schema cost what their delta costs. The dispatch
//! index means a round only visits rule instances that can match its delta.
//!
//! DRed overdeletes whole transitive components on deletes inside them; B/F (R5) replaces
//! it.

use std::borrow::Cow;

use hashbrown::HashSet;
use rayon::prelude::*;

use super::batch::{Store, check, consistency_rules, fact_rules, sorted};
use super::eval::{
    AllFacts, GroundProgram, Job, RuleKey, Schema, Seg, Source, instantiate_head, rule_key,
    run_jobs, transitive_predicate, transitivity,
};
use super::ir::{Head, Rule};
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

/// The rules an update runs, compiled against the vocabulary.
#[derive(Clone, Copy)]
pub struct Rules<'a> {
    pub rules: &'a [Rule],
    pub lists: Option<&'a ListVocabulary>,
    pub schema: &'a Schema,
}

impl Rules<'_> {
    /// Fact and consistency rules, with the list rules instantiated over `source`.
    fn source<S: Source + ?Sized>(&self, source: &S) -> Vec<Rule> {
        let mut diagnostics = Vec::new();
        let mut rules = fact_rules(source, self.rules, self.lists, &mut diagnostics);
        rules.extend(consistency_rules(source, self.rules, self.lists));
        rules
    }

    /// The ground program over `source`; its bodiless facts count as handed out.
    fn program<S: Source + ?Sized>(&self, source: &S) -> GroundProgram {
        let mut program = GroundProgram::default();
        program.ground(source, self.schema, &self.source(source));
        program.take_facts();
        program
    }

    /// The list rules (both kinds) instantiated over `source`.
    fn list_rules<S: Source + ?Sized>(&self, source: &S) -> Vec<Rule> {
        self.lists
            .map(|vocabulary| instantiate(vocabulary, &AllFacts(source)).0)
            .unwrap_or_default()
    }

    fn is_list_fact(&self, fact: Triple) -> bool {
        self.lists.is_some_and(|l| l.is_list_fact(fact))
    }

    /// Whether `fact` feeds the ground program: a schema or list fact.
    fn is_program_fact(&self, fact: Triple) -> bool {
        self.schema.is_schema_fact(fact) || self.is_list_fact(fact)
    }

    /// The rules with schema atoms (the ones grounding through a delta can extend).
    fn schema_rules(&self) -> Vec<Rule> {
        self.rules
            .iter()
            .filter(|r| r.body.iter().any(|a| self.schema.is_schema_atom(a)))
            .cloned()
            .collect()
    }
}

/// What [`update`] computed.
#[derive(Default)]
pub struct Update {
    /// Facts to add to the inferred stack.
    pub insert: Vec<Triple>,
    /// Facts to remove from the inferred stack.
    pub remove: Vec<Triple>,
    /// Consistency violations that involve a fact the change added.
    pub violations: Vec<Violation>,
    pub rounds: usize,
    /// The ground program of the state after the change, if it differs from the one given
    /// (or none was given); `None` means the given program still applies.
    pub program: Option<GroundProgram>,
}

impl std::fmt::Debug for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Update")
            .field("insert", &self.insert.len())
            .field("remove", &self.remove.len())
            .field("violations", &self.violations)
            .field("rounds", &self.rounds)
            .field("program_changed", &self.program.is_some())
            .finish()
    }
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

/// The ground program of the materialised state `base`: the cache [`update`] takes.
pub fn program<B: Base + ?Sized>(base: &B, rules: Rules<'_>) -> GroundProgram {
    let (hidden, extra) = (HashSet::new(), Store::default());
    rules.program(&Overlay {
        base,
        hidden: &hidden,
        extra: &extra,
    })
}

/// Maintains the materialisation of `base` under `rules` for a change of the asserted
/// facts, which `base` already reflects: `inserted` are facts new to the state (neither
/// asserted nor inferred before), `deleted` facts no longer asserted. `cache` is the
/// ground program of the state before the change ([`program`]); without it, it is built.
pub fn update<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
) -> Update {
    let mut result = Update::default();
    let schema = rules.schema;
    let empty = Store::default();
    let inserted_set: HashSet<Triple> = inserted.iter().copied().collect();
    // The ground program of the old state: `base` without the inserted facts, plus the
    // deleted ones.
    let computed: Option<GroundProgram> = cache.is_none().then(|| {
        let extra = Store::new(deleted.to_vec());
        rules.program(&Overlay {
            base,
            hidden: &inserted_set,
            extra: &extra,
        })
    });
    let old_program: &GroundProgram = cache.or(computed.as_ref()).expect("one of them");

    // 1. Overdelete, over the old state.
    let mut overdeleted: HashSet<Triple> = HashSet::new();
    if !deleted.is_empty() {
        let mut extra = Store::new(deleted.to_vec());
        let (fact_schema_rules, old_lists) = {
            let old = Overlay {
                base,
                hidden: &inserted_set,
                extra: &extra,
            };
            let lists: Vec<Rule> = rules
                .list_rules(&old)
                .into_iter()
                .filter(|r| r.head != Head::Inconsistent)
                .collect();
            let schema_rules: Vec<Rule> = rules
                .schema_rules()
                .into_iter()
                .filter(|r| r.head != Head::Inconsistent)
                .collect();
            (schema_rules, lists)
        };
        let transitivity_rules: Vec<Rule> = old_program
            .transitive
            .iter()
            .map(|&p| transitivity(p))
            .collect();
        let mut vanished_done: HashSet<RuleKey> = HashSet::new();
        loop {
            result.rounds += 1;
            let old = Overlay {
                base,
                hidden: &inserted_set,
                extra: &extra,
            };
            // Instances through a deleted schema fact, known or not, in full.
            let mut scratch = GroundProgram::default();
            scratch.ground_delta(&old, schema, &fact_schema_rules);
            let mut fresh: Vec<Rule> = scratch.rules.clone();
            fresh.extend(scratch.transitive.iter().map(|&p| transitivity(p)));
            let mut bodiless = scratch.take_facts();
            // List rules whose list lost a fact: in full as well.
            let mut delta_facts = Vec::new();
            extra.scan([None, None, None], Seg::Delta, &mut |f| delta_facts.push(f));
            if delta_facts.iter().any(|&f| rules.is_list_fact(f)) {
                let mut gone: HashSet<Triple> = overdeleted.clone();
                gone.extend(delta_facts.iter().copied());
                gone.extend(deleted.iter().copied());
                gone.extend(inserted.iter().copied());
                let now = Overlay {
                    base,
                    hidden: &gone,
                    extra: &empty,
                };
                let current: HashSet<RuleKey> =
                    rules.list_rules(&now).iter().map(rule_key).collect();
                for rule in &old_lists {
                    let key = rule_key(rule);
                    if current.contains(&key) || !vanished_done.insert(key) {
                        continue;
                    }
                    match transitive_predicate(rule) {
                        Some(p) => fresh.push(transitivity(p)),
                        None if rule.body.is_empty() => {
                            if let Head::Facts(heads) = &rule.head {
                                bodiless.extend(heads.iter().map(|h| instantiate_head(h, &[])));
                            }
                        }
                        None => fresh.push(rule.clone()),
                    }
                }
            }
            let mut jobs = Vec::new();
            for (r, i) in old_program.variants(&extra) {
                jobs.extend(Job::variant(&old, &old_program.rules[r], i));
            }
            for rule in &transitivity_rules {
                for i in 0..2 {
                    jobs.extend(Job::variant(&old, rule, i));
                }
            }
            for rule in &fresh {
                jobs.extend(Job::full(&old, rule));
            }
            // Only inferred facts of the old state are overdeleted; asserted ones stay.
            let overdeletable = |f: Triple| {
                base.contains(f)
                    && !base.is_asserted(f)
                    && !extra.contains(f)
                    && !inserted_set.contains(&f)
            };
            let mut candidates = run_jobs(&old, &jobs, &overdeletable);
            candidates.extend(bodiless.into_iter().filter(|&f| overdeletable(f)));
            drop(jobs);
            let delta = extra.advance(candidates);
            if delta.is_empty() {
                break;
            }
            overdeleted.extend(delta);
        }
    }

    // 2. Rederive: overdeleted and deleted facts with a derivation from what remains. The
    // program is that of the settled state, without the inserted facts: rule instances
    // that exist because of them are then new in phase 3 and evaluated in full.
    let mut hidden: HashSet<Triple> = overdeleted.clone();
    hidden.extend(deleted.iter().copied());
    let mut unsettled = hidden.clone();
    unsettled.extend(inserted.iter().copied());
    let remaining = Overlay {
        base,
        hidden: &unsettled,
        extra: &empty,
    };
    let program_changed = deleted
        .iter()
        .chain(&overdeleted)
        .any(|&f| rules.is_program_fact(f));
    let mut program: Cow<'_, GroundProgram> = if program_changed {
        Cow::Owned(rules.program(&remaining))
    } else {
        Cow::Borrowed(old_program)
    };
    let candidates: Vec<Triple> = overdeleted.iter().chain(deleted).copied().collect();
    let settled: &GroundProgram = &program;
    let rederived: Vec<Triple> = candidates
        .par_iter()
        .copied()
        .filter(|&fact| !base.is_asserted(fact) && settled.derivable(&remaining, fact))
        .collect();
    let settled_consistency = program.consistency.len();

    // 3. Insert: semi-naive from the rederived and inserted facts.
    let schema_rules = rules.schema_rules();
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
        // Instances through new schema facts, and list rules from new list facts. They
        // are appended, so the new ones are `evaluated..`.
        let evaluated = program.rules.len();
        let transitive_before = program.transitive.clone();
        let mut delta_facts = Vec::new();
        extra.scan([None, None, None], Seg::Delta, &mut |f| delta_facts.push(f));
        if delta_facts.iter().any(|&f| schema.is_schema_fact(f)) {
            program.to_mut().ground_delta(&state, schema, &schema_rules);
        }
        if delta_facts.iter().any(|&f| rules.is_list_fact(f)) {
            let list_rules = rules.list_rules(&state);
            program.to_mut().ground(&state, schema, &list_rules);
        }
        let mut candidates = if program.has_pending_facts() {
            program.to_mut().take_facts()
        } else {
            Vec::new()
        };
        candidates.retain(|&f| !state.contains(f));
        let mut jobs = Vec::new();
        for (r, i) in program.variants(&extra) {
            if r < evaluated {
                jobs.extend(Job::variant(&state, &program.rules[r], i));
            }
        }
        for rule in &program.rules[evaluated..] {
            jobs.extend(Job::full(&state, rule));
        }
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
    let visible_before =
        |f: Triple| base.contains(f) && !hidden.contains(&f) && !inserted_set.contains(&f);
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
    // phase 3 (rederived ones are hidden in `base`), with the new ones as its delta:
    // existing instances through them, new instances in full.
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
    let variants: Vec<(usize, usize)> = program
        .consistency_variants(&fresh)
        .into_iter()
        .filter(|&(c, _)| c < settled_consistency)
        .collect();
    let mut found = HashSet::new();
    check(
        &state,
        &program,
        settled_consistency..program.consistency.len(),
        &variants,
        &mut found,
    );
    result.violations = sorted(found);
    result.program = match program {
        Cow::Owned(program) => Some(program),
        Cow::Borrowed(_) => None,
    }
    .or(computed);
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
