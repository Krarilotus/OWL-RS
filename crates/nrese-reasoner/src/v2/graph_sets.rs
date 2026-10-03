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

use hashbrown::HashMap;
use rayon::prelude::*;

use super::batch::Store;
use super::eval::{GroundProgram, Grounding, Job, Seg, Source, transitivity};
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
struct Changed<'a> {
    all: &'a Store,
    changed: &'a Store,
}

impl Source for Changed<'_> {
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        match seg {
            Seg::Delta => self.changed.scan(pattern, Seg::All, f),
            Seg::Old | Seg::All => self.all.scan(pattern, Seg::All, f),
        }
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        match seg {
            Seg::Delta => self.changed.estimate(pattern, Seg::All),
            Seg::Old | Seg::All => self.all.estimate(pattern, Seg::All),
        }
    }

    fn contains(&self, fact: Triple) -> bool {
        self.all.contains(fact)
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
/// `graphs_of` gives an asserted fact's graphs (none for an inferred one); `cap` bounds
/// the sets kept per fact.
pub fn support_sets(
    facts: &[Triple],
    program: &GroundProgram,
    graphs_of: &(dyn Fn(Triple) -> Vec<u32> + Sync),
    cap: usize,
) -> HashMap<Triple, Vec<GraphSet>> {
    let all = Store::new(facts.to_vec());
    let initial: Vec<(Triple, Vec<GraphSet>)> = facts
        .par_iter()
        .filter_map(|&fact| {
            let graphs = graphs_of(fact);
            (!graphs.is_empty()).then(|| {
                (
                    fact,
                    minimal(graphs.into_iter().map(GraphSet::of).collect(), cap),
                )
            })
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

/// Every fact of `store`.
fn delta_facts(store: &Store) -> Vec<Triple> {
    let mut out = Vec::new();
    store.scan([None, None, None], Seg::All, &mut |f| out.push(f));
    out
}

/// The sets of the schema facts `grounding` was grounded on: over its alternatives, the
/// minimal unions of one set per premise; none if no alternative has sets yet.
fn grounding_sets(
    program: &GroundProgram,
    grounding: Grounding,
    sets: &HashMap<Triple, Vec<GraphSet>>,
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
fn premise_sets_from(
    body: &[Triple],
    start: &[GraphSet],
    sets: &HashMap<Triple, Vec<GraphSet>>,
    cap: usize,
) -> Vec<GraphSet> {
    // The usual case, one set everywhere: a single union, nothing to minimise.
    if let [only] = start {
        let mut one = only.clone();
        let mut single = true;
        for premise in body {
            match sets.get(premise).map(Vec::as_slice) {
                None => return Vec::new(),
                Some([set]) => one = one.union(set),
                Some(_) => {
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
        let Some(own) = sets.get(premise) else {
            return Vec::new();
        };
        let mut next = Vec::with_capacity(product.len() * own.len());
        for a in &product {
            for b in own {
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
}
