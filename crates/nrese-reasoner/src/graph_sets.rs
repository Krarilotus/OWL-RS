//! Support graph sets (docs/design/reasoner-provenance.md, level `acl`; research designs
//! §4): per fact of a closure, the minimal sets of graphs whose statements derive it. A
//! reader who may read the graphs `R` sees an inferred fact when one of its sets lies in
//! `R`: exactly the facts of the closure of the statements in `R` (with every set kept).
//!
//! Computed over a closure by annotated semi-naive evaluation (the why-provenance of
//! Green, Karvounarakis and Tannen, PODS 2007, over graphs instead of facts):
//! - an asserted fact starts with one singleton set per graph that holds it, a fact
//!   without premises (an axiom of the program) with the empty set;
//! - a rule instance gives its head the minimal unions of one set per premise, its
//!   schema premises (the facts its ground rule was grounded on) included;
//! - a fact whose sets change is propagated again, until nothing changes.
//!
//! Each fact keeps at most `cap` sets, the smallest first. A set dropped by the cap can
//! only hide a fact from a reader who could have seen it, never show one to a reader who
//! couldn't.
//!
//! After a change, [`update_support_sets`] recomputes only the facts it can affect (the
//! forward closure of the facts it touched), from what the others keep.

use std::borrow::Cow;

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

use super::batch::Store;
use super::eval::{
    GroundProgram, Grounding, Job, Seg, Source, derivations, instantiate_head, transitivity,
};
use super::ir::{Head, Rule, Triple};

/// Driver facts per parallel task.
const MORSEL: usize = 4096;

/// A set of graphs (indices into the caller's graph list) as a bitset: inline up to 128
/// graphs, so a union is a word-wise OR and a subset test an AND-NOT, without allocation.
/// Trailing zero words are trimmed, so equal sets are equal values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GraphSet(smallvec::SmallVec<[u64; 2]>);

impl GraphSet {
    /// The set of one graph.
    pub fn of(graph: u32) -> Self {
        let word = (graph / 64) as usize;
        let mut words = smallvec::SmallVec::from_elem(0, word + 1);
        words[word] = 1 << (graph % 64);
        Self(words)
    }

    pub fn union(&self, other: &Self) -> Self {
        let (long, short) = if self.0.len() >= other.0.len() {
            (self, other)
        } else {
            (other, self)
        };
        let mut words = long.0.clone();
        for (w, o) in words.iter_mut().zip(&short.0) {
            *w |= o;
        }
        Self(words)
    }

    /// Whether every graph of `self` is in `other`.
    pub fn is_subset(&self, other: &Self) -> bool {
        self.0
            .iter()
            .enumerate()
            .all(|(i, w)| w & !other.0.get(i).copied().unwrap_or(0) == 0)
    }

    /// The number of graphs.
    pub fn len(&self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The graphs, ascending.
    pub fn graphs(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.iter().enumerate().flat_map(|(i, &w)| {
            (0..64u32)
                .filter(move |b| w & (1 << b) != 0)
                .map(move |b| i as u32 * 64 + b)
        })
    }
}

/// The facts of `all`, read whole or, as the delta, only the facts whose sets changed.
struct Changed<'a, S: ?Sized> {
    all: &'a S,
    changed: &'a Store,
}

impl<S: Source + ?Sized> Source for Changed<'_, S> {
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        match seg.plain() {
            Seg::Old | Seg::All => self.all.scan(pattern, Seg::All, f),
            _ => self.changed.scan(pattern, Seg::All, f),
        }
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        match seg.plain() {
            Seg::Old | Seg::All => self.all.estimate(pattern, Seg::All),
            _ => self.changed.estimate(pattern, Seg::All),
        }
    }

    fn contains(&self, fact: Triple) -> bool {
        self.all.contains(fact)
    }
}

/// The sets of facts as an evaluation reads them: `None` for a fact without sets yet.
trait Lookup: Sync {
    fn sets_of(&self, fact: &Triple) -> Option<Cow<'_, [GraphSet]>>;
}

impl Lookup for HashMap<Triple, Vec<GraphSet>> {
    fn sets_of(&self, fact: &Triple) -> Option<Cow<'_, [GraphSet]>> {
        self.get(fact).map(|sets| Cow::Borrowed(sets.as_slice()))
    }
}

/// During an update: the facts it recomputes as they are now, the others as they were.
struct Recomputing<'a> {
    now: &'a HashMap<Triple, Vec<GraphSet>>,
    affected: &'a HashSet<Triple>,
    previous: &'a (dyn Fn(Triple) -> Vec<GraphSet> + Sync),
}

impl Lookup for Recomputing<'_> {
    fn sets_of(&self, fact: &Triple) -> Option<Cow<'_, [GraphSet]>> {
        if self.affected.contains(fact) {
            return self.now.sets_of(fact);
        }
        let sets = (self.previous)(*fact);
        (!sets.is_empty()).then_some(Cow::Owned(sets))
    }
}

/// The sets a fact starts with: the empty set for an axiom, else one per graph that
/// holds it (none for a fact only inferred). At most `cap`.
pub fn initial_sets(graphs: Vec<u32>, axiom: bool, cap: usize) -> Vec<GraphSet> {
    match axiom {
        true => vec![GraphSet::default()],
        false => minimal(graphs.into_iter().map(GraphSet::of).collect(), cap),
    }
}

/// `sets` without those that contain another, the smallest first, at most `cap`.
fn minimal(mut sets: Vec<GraphSet>, cap: usize) -> Vec<GraphSet> {
    sets.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    sets.dedup();
    let mut out: Vec<GraphSet> = Vec::new();
    for set in sets {
        if out.len() == cap {
            break;
        }
        if !out.iter().any(|kept| kept.is_subset(&set)) {
            out.push(set);
        }
    }
    out
}

/// The support graph sets of every fact of the closure `facts` under `program`.
/// `graphs_of` gives an asserted fact's graphs (none for an inferred one); `axioms`
/// (sorted) hold in no graph, for every reader (a ruleset's axiomatic triples); `cap`
/// bounds the sets kept per fact.
pub fn support_sets(
    facts: &[Triple],
    program: &GroundProgram,
    axioms: &[Triple],
    graphs_of: &(dyn Fn(Triple) -> Vec<u32> + Sync),
    cap: usize,
) -> HashMap<Triple, Vec<GraphSet>> {
    let all = Store::new(facts.to_vec());
    let initial: Vec<(Triple, Vec<GraphSet>)> = facts
        .par_iter()
        .filter_map(|&fact| {
            let axiom = axioms.binary_search(&fact).is_ok();
            let initial =
                initial_sets(if axiom { Vec::new() } else { graphs_of(fact) }, axiom, cap);
            (!initial.is_empty()).then_some((fact, initial))
        })
        .collect();
    let mut sets: HashMap<Triple, Vec<GraphSet>> = HashMap::with_capacity(facts.len());
    sets.extend(initial);
    let empty = GraphSet::default();
    // Facts without a body (ground from schema facts alone, `C subClassOf C` from
    // `C a owl:Class`): evaluated again whenever one of their premises changes.
    let mut bodiless_by_premise: HashMap<Triple, Vec<Triple>> = HashMap::new();
    for &fact in &program.bodiless {
        let grounding = Grounding::Bodiless(fact);
        for premises in program.premise_alternatives(grounding) {
            for &premise in premises {
                bodiless_by_premise.entry(premise).or_default().push(fact);
            }
        }
        let from = grounding_sets(program, grounding, &sets, cap, &empty);
        merge(&mut sets, fact, from, cap);
    }
    // The rules with their schema premises; transitivity as a rule here (the module's
    // closure has no instances to annotate).
    let transitive: Vec<Rule> = program
        .transitive
        .iter()
        .map(|&p| transitivity(p))
        .collect();
    let rules: Vec<(&Rule, Grounding)> = program
        .rules
        .iter()
        .enumerate()
        .map(|(r, rule)| (rule, Grounding::Rule(r)))
        .chain(
            transitive
                .iter()
                .zip(program.transitive.iter())
                .map(|(rule, &p)| (rule, Grounding::Transitive(p))),
        )
        .filter(|(rule, _)| !rule.body.is_empty() && matches!(rule.head, Head::Facts(_)))
        .collect();
    // The rules each schema fact grounds: run over all their instances in a round where
    // its sets changed.
    let mut rules_by_premise: HashMap<Triple, Vec<usize>> = HashMap::new();
    for (k, &(_, grounding)) in rules.iter().enumerate() {
        for premises in program.premise_alternatives(grounding) {
            for &premise in premises {
                rules_by_premise.entry(premise).or_default().push(k);
            }
        }
    }
    let mut changed: Vec<Triple> = sets.keys().copied().collect();
    let mut first = true;
    while !changed.is_empty() {
        changed.sort_unstable();
        let mut whole: Vec<bool> = vec![first; rules.len()];
        first = false;
        for fact in &changed {
            for &k in rules_by_premise.get(fact).into_iter().flatten() {
                whole[k] = true;
            }
        }
        let delta = Store::new(std::mem::take(&mut changed));
        let source = Changed {
            all: &all,
            changed: &delta,
        };
        // Every instance with a premise among the changed facts, with every premise's
        // current sets (an instance found twice adds the same sets twice, harmlessly):
        // jobs in parallel morsels, each emitting (head, set) pairs.
        let mut found: Vec<(Triple, GraphSet)> = Vec::new();
        for fact in delta_facts(&delta) {
            for &bodiless in bodiless_by_premise.get(&fact).into_iter().flatten() {
                let grounding = Grounding::Bodiless(bodiless);
                for set in grounding_sets(program, grounding, &sets, cap, &empty) {
                    found.push((bodiless, set));
                }
            }
        }
        let mut jobs: Vec<(Job<'_>, &[super::ir::Atom], Vec<GraphSet>)> = Vec::new();
        for (k, &(rule, grounding)) in rules.iter().enumerate() {
            // The schema facts' part, once per rule and round: the minimal sets over the
            // alternative groundings.
            let schema = grounding_sets(program, grounding, &sets, cap, &empty);
            let Head::Facts(heads) = &rule.head else {
                continue;
            };
            if schema.is_empty() {
                continue;
            }
            // Instances with a changed premise; all of them when a schema fact changed.
            if whole[k] {
                jobs.extend(Job::full(&source, rule).map(|job| (job, heads.as_slice(), schema)));
            } else {
                for i in 0..rule.body.len() {
                    let job =
                        Job::new(
                            &source,
                            rule,
                            i,
                            |j| {
                                if j == i { Seg::Delta } else { Seg::All }
                            },
                        );
                    jobs.extend(job.map(|job| (job, heads.as_slice(), schema.clone())));
                }
            }
        }
        let tasks: Vec<(usize, std::ops::Range<usize>)> = jobs
            .iter()
            .enumerate()
            .flat_map(|(j, (job, ..))| {
                (0..job.drivers())
                    .step_by(MORSEL)
                    .map(move |start| (j, start..(start + MORSEL).min(job.drivers())))
            })
            .collect();
        let sets_now = &sets;
        found.par_extend(tasks.par_iter().flat_map_iter(|(j, range)| {
            let (job, heads, schema) = &jobs[*j];
            let mut out = Vec::new();
            let mut body = Vec::with_capacity(job.rule.body.len());
            job.run(&source, range.clone(), &mut |bindings| {
                body.clear();
                body.extend(
                    job.rule
                        .body
                        .iter()
                        .map(|atom| super::eval::instantiate_head(atom, bindings)),
                );
                let from = premise_sets_from(&body, schema, sets_now, cap);
                for head in heads.iter() {
                    let fact = super::eval::instantiate_head(head, bindings);
                    out.extend(from.iter().map(|set| (fact, set.clone())));
                }
            });
            out
        }));
        // Per head: its new sets merged into its old ones.
        found.par_sort_unstable();
        found.dedup();
        let mut starts: Vec<usize> = (0..found.len())
            .filter(|&i| i == 0 || found[i - 1].0 != found[i].0)
            .collect();
        starts.push(found.len());
        // In parallel against the sets as they are; then the changes written back.
        let updates: Vec<(Triple, Vec<GraphSet>)> = starts
            .par_windows(2)
            .filter_map(|w| {
                let group = &found[w[0]..w[1]];
                let fact = group[0].0;
                let current = sets.get(&fact).map_or(&[][..], Vec::as_slice);
                // Nothing new: every found set is covered by a kept one.
                if group
                    .iter()
                    .all(|(_, set)| current.iter().any(|kept| kept.is_subset(set)))
                {
                    return None;
                }
                let mut all = current.to_vec();
                all.extend(group.iter().map(|(_, set)| set.clone()));
                let next = minimal(all, cap);
                (next.as_slice() != current).then_some((fact, next))
            })
            .collect();
        for (fact, next) in updates {
            sets.insert(fact, next);
            changed.push(fact);
        }
    }
    sets
}

/// A change of the closure, for [`update_support_sets`].
pub struct Change<'a, S: ?Sized> {
    /// The facts of the closure after the change, and those the change removed (joins
    /// must reach what was derived from them).
    pub source: &'a S,
    /// Whether a fact is in the closure after the change.
    pub in_closure: &'a (dyn Fn(Triple) -> bool + Sync),
    /// Every fact whose own graphs changed, or that the change added to the closure or
    /// removed from it.
    pub touched: &'a [Triple],
    /// A fact's sets before the change.
    pub previous: &'a (dyn Fn(Triple) -> Vec<GraphSet> + Sync),
    /// Past this many affected facts the update gives up (a recomputation is cheaper).
    pub limit: usize,
}

/// The support graph sets after `change`, for the facts it can affect: those derived,
/// in any number of steps, from a touched fact. The others keep theirs. Each affected
/// fact of the closure is in the result, with no sets if none is left; affected facts
/// outside it aren't.
///
/// `program` is the ground program after the change and must have been grounded on the
/// same schema facts as the one before (the caller compares
/// [`GroundProgram::schema_premises`]). `None` when the change reaches a schema fact,
/// or affects more than `change.limit` facts: then [`support_sets`] computes them all.
///
/// The affected facts start again from their own graphs: one pass over each one's
/// derivations takes in what the unaffected facts support, then semi-naive rounds among
/// the affected facts reach the least fixpoint, as [`support_sets`] does for all.
pub fn update_support_sets<S: Source + Sync + ?Sized>(
    change: &Change<'_, S>,
    program: &GroundProgram,
    axioms: &[Triple],
    graphs_of: &(dyn Fn(Triple) -> Vec<u32> + Sync),
    cap: usize,
) -> Option<HashMap<Triple, Vec<GraphSet>>> {
    let schema = program.schema_premises();
    let transitive: Vec<(Rule, Grounding)> = program
        .transitive
        .iter()
        .map(|&p| (transitivity(p), Grounding::Transitive(p)))
        .collect();
    let rules: Vec<(&Rule, Grounding)> = program
        .rules
        .iter()
        .enumerate()
        .map(|(r, rule)| (rule, Grounding::Rule(r)))
        .chain(
            transitive
                .iter()
                .map(|(rule, grounding)| (rule, *grounding)),
        )
        .filter(|(rule, _)| !rule.body.is_empty() && matches!(rule.head, Head::Facts(_)))
        .collect();
    // The affected facts: the touched ones and what follows from them.
    let mut affected: HashSet<Triple> = change.touched.iter().copied().collect();
    if affected.iter().any(|fact| schema.contains(fact)) {
        return None;
    }
    let mut frontier: Vec<Triple> = affected.iter().copied().collect();
    while !frontier.is_empty() {
        frontier.sort_unstable();
        let delta = Store::new(std::mem::take(&mut frontier));
        let source = Changed {
            all: change.source,
            changed: &delta,
        };
        let heads = instances(&source, &rules, &|k, bindings, out| {
            let rule = rules[k].0;
            if let Head::Facts(heads) = &rule.head {
                out.extend(heads.iter().map(|head| instantiate_head(head, bindings)));
            }
        });
        for head in heads {
            if affected.insert(head) {
                if schema.contains(&head) {
                    return None;
                }
                frontier.push(head);
            }
        }
        if affected.len() > change.limit {
            return None;
        }
    }
    let members: HashSet<Triple> = affected
        .iter()
        .copied()
        .filter(|&fact| (change.in_closure)(fact))
        .collect();
    let empty = GraphSet::default();
    let mut now: HashMap<Triple, Vec<GraphSet>> = members
        .iter()
        .map(|&fact| {
            let axiom = axioms.binary_search(&fact).is_ok();
            let graphs = if axiom { Vec::new() } else { graphs_of(fact) };
            (fact, initial_sets(graphs, axiom, cap))
        })
        .collect();
    // The schema facts' part per rule: they aren't affected, so it is fixed.
    let schemas: Vec<Vec<GraphSet>> = {
        let lookup = Recomputing {
            now: &now,
            affected: &affected,
            previous: change.previous,
        };
        rules
            .iter()
            .map(|&(_, grounding)| grounding_sets(program, grounding, &lookup, cap, &empty))
            .collect()
    };
    let by_grounding: HashMap<Grounding, usize> = rules
        .iter()
        .enumerate()
        .map(|(k, &(_, grounding))| (grounding, k))
        .collect();
    let premises_in_closure = |body: &[Triple]| body.iter().all(|&p| (change.in_closure)(p));
    // Every derivation of every affected fact, against the sets as they start.
    let found: Vec<(Triple, Vec<GraphSet>)> = {
        let lookup = Recomputing {
            now: &now,
            affected: &affected,
            previous: change.previous,
        };
        let list: Vec<Triple> = members.iter().copied().collect();
        list.par_iter()
            .map(|&fact| {
                let mut from = Vec::new();
                if program.bodiless.contains(&fact) {
                    from.extend(grounding_sets(
                        program,
                        Grounding::Bodiless(fact),
                        &lookup,
                        cap,
                        &empty,
                    ));
                }
                let mut groundings: Vec<Grounding> = program
                    .producers(fact)
                    .into_iter()
                    .map(Grounding::Rule)
                    .collect();
                if program.transitive.contains(&fact[1]) {
                    groundings.push(Grounding::Transitive(fact[1]));
                }
                for grounding in groundings {
                    let Some(&k) = by_grounding.get(&grounding) else {
                        continue;
                    };
                    let (rule, schema) = (rules[k].0, &schemas[k]);
                    if schema.is_empty() {
                        continue;
                    }
                    let mut body = Vec::with_capacity(rule.body.len());
                    derivations(change.source, rule, fact, Seg::All, &mut |bindings| {
                        body.clear();
                        body.extend(
                            rule.body
                                .iter()
                                .map(|atom| instantiate_head(atom, bindings)),
                        );
                        if premises_in_closure(&body) {
                            from.extend(premise_sets_from(&body, schema, &lookup, cap));
                        }
                        false
                    });
                }
                (fact, from)
            })
            .collect()
    };
    let mut changed: Vec<Triple> = Vec::new();
    for (fact, from) in found {
        if merge(&mut now, fact, from, cap) {
            changed.push(fact);
        }
    }
    // Semi-naive rounds among the affected facts.
    while !changed.is_empty() {
        changed.sort_unstable();
        let delta = Store::new(std::mem::take(&mut changed));
        let source = Changed {
            all: change.source,
            changed: &delta,
        };
        let found = {
            let lookup = Recomputing {
                now: &now,
                affected: &affected,
                previous: change.previous,
            };
            instances(&source, &rules, &|k, bindings, out| {
                let rule = rules[k].0;
                let Head::Facts(heads) = &rule.head else {
                    return;
                };
                let body: Vec<Triple> = rule
                    .body
                    .iter()
                    .map(|atom| instantiate_head(atom, bindings))
                    .collect();
                if !premises_in_closure(&body) {
                    return;
                }
                let from = premise_sets_from(&body, &schemas[k], &lookup, cap);
                for head in heads {
                    let fact = instantiate_head(head, bindings);
                    if members.contains(&fact) {
                        out.extend(from.iter().map(|set| (fact, set.clone())));
                    }
                }
            })
        };
        let mut by_fact: HashMap<Triple, Vec<GraphSet>> = HashMap::new();
        for (fact, set) in found {
            by_fact.entry(fact).or_default().push(set);
        }
        for (fact, from) in by_fact {
            if merge(&mut now, fact, from, cap) {
                changed.push(fact);
            }
        }
    }
    Some(now)
}

/// What `emit` makes of every instance of `rules` (by index) with a premise among
/// `source`'s delta facts, the instances in parallel morsels.
fn instances<S, T, E>(source: &S, rules: &[(&Rule, Grounding)], emit: &E) -> Vec<T>
where
    S: Source + Sync + ?Sized,
    T: Send,
    E: Fn(usize, &[Option<u64>], &mut Vec<T>) + Sync,
{
    let jobs: Vec<(usize, Job<'_>)> = rules
        .iter()
        .enumerate()
        .flat_map(|(k, &(rule, _))| {
            (0..rule.body.len()).filter_map(move |i| {
                Job::new(
                    source,
                    rule,
                    i,
                    |j| if j == i { Seg::Delta } else { Seg::All },
                )
                .map(|job| (k, job))
            })
        })
        .collect();
    let tasks: Vec<(usize, std::ops::Range<usize>)> = jobs
        .iter()
        .enumerate()
        .flat_map(|(j, (_, job))| {
            (0..job.drivers())
                .step_by(MORSEL)
                .map(move |start| (j, start..(start + MORSEL).min(job.drivers())))
        })
        .collect();
    tasks
        .par_iter()
        .flat_map_iter(|(j, range)| {
            let (k, job) = &jobs[*j];
            let mut out = Vec::new();
            job.run(source, range.clone(), &mut |bindings| {
                emit(*k, bindings, &mut out)
            });
            out
        })
        .collect()
}

/// Every fact of `store`.
fn delta_facts(store: &Store) -> Vec<Triple> {
    let mut out = Vec::new();
    store.scan([None, None, None], Seg::All, &mut |f| out.push(f));
    out
}

/// The sets of the schema facts `grounding` was grounded on: over its alternatives, the
/// minimal unions of one set per premise; none if no alternative has sets yet.
fn grounding_sets<L: Lookup + ?Sized>(
    program: &GroundProgram,
    grounding: Grounding,
    sets: &L,
    cap: usize,
    empty: &GraphSet,
) -> Vec<GraphSet> {
    let all = program
        .premise_alternatives(grounding)
        .into_iter()
        .flat_map(|premises| premise_sets_from(premises, std::slice::from_ref(empty), sets, cap))
        .collect();
    minimal(all, cap)
}

/// The minimal unions of the sets `start` with one set per premise of `body`; none if a
/// premise has none yet.
fn premise_sets_from<L: Lookup + ?Sized>(
    body: &[Triple],
    start: &[GraphSet],
    sets: &L,
    cap: usize,
) -> Vec<GraphSet> {
    // The usual case, one set everywhere: a single union, nothing to minimise.
    if let [only] = start {
        let mut one = only.clone();
        let mut single = true;
        for premise in body {
            let Some(own) = sets.sets_of(premise) else {
                return Vec::new();
            };
            match &*own {
                [set] => one = one.union(set),
                _ => {
                    single = false;
                    break;
                }
            }
        }
        if single {
            return vec![one];
        }
    }
    let mut product = start.to_vec();
    for premise in body {
        let Some(own) = sets.sets_of(premise) else {
            return Vec::new();
        };
        let mut next = Vec::with_capacity(product.len() * own.len());
        for a in &product {
            for b in own.iter() {
                next.push(a.union(b));
            }
        }
        product = minimal(next, cap);
    }
    product
}

/// Adds `from` to `fact`'s sets; whether they changed.
fn merge(
    sets: &mut HashMap<Triple, Vec<GraphSet>>,
    fact: Triple,
    from: Vec<GraphSet>,
    cap: usize,
) -> bool {
    let current = sets.entry(fact).or_default();
    let mut all = current.clone();
    all.extend(from);
    let next = minimal(all, cap);
    if next == *current {
        return false;
    }
    *current = next;
    true
}

#[cfg(test)]
mod tests {
    /// Against the definition, over random ontologies spread over three graphs: for every
    /// set R of graphs, the facts with a support set inside R are exactly the closure of
    /// the statements in R (no cap).
    #[test]
    fn support_sets_give_the_closure_of_each_set_of_graphs() {
        super::super::tests::support_sets_against_closures();
    }

    /// The update after random changes of where statements are equals a recomputation.
    #[test]
    fn updates_equal_recomputation() {
        super::super::tests::support_set_updates_against_recomputation();
    }
}
