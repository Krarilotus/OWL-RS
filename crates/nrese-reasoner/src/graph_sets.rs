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
//! Each fact keeps at most `cap` sets, the smallest first ([`Annotation`]). Where the cap
//! leaves out one of a fact's minimal sets, or one of a premise's on the way, the fact is
//! marked truncated: its kept sets are each a real derivation's, but maybe not all. A
//! reader one of them fits sees the fact; a reader none fits sees it only if the fact is
//! unmarked; for a marked one, [`closure_within`] decides, exactly. So the cap bounds the
//! work and the memory, never what a reader sees. A second mark says whether a set other
//! than one graph that asserts the fact may be missing ([`Annotation::beyond_own`]): a
//! fact asserted in more graphs than the cap is marked truncated (its dependents may lack
//! sets through it), but a reader who reads none of its graphs depends on the second only.
//!
//! After a change, [`update_support_sets`] recomputes only the facts it can affect (the
//! forward closure of the facts it touched), from what the others keep: each starts
//! again from its own graphs, its mark included.
//!
//! Memory: an evaluation's working set (the closure's store, the sets, a round's
//! derivations) is not charged before it is allocated; it reports its size to a
//! [`Watch`] at round boundaries and stops when the watch refuses (the reading query's
//! envelope), or when the process passes its memory limit. A temporary exemption, shared
//! with the materialiser's working set, until the reasoner's working set is charged
//! (notes/2026-10-20-executor-admission-design.md, W1 site "reasoner fixpoint").

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

/// A fact's support graph sets as an evaluation keeps them: minimal, at most the cap, the
/// smallest first; and whether one of its minimal sets may be missing because the cap
/// left one out, here or in a premise's on the way. Unmarked, the sets are all of the
/// fact's: a reader none of them fits can't derive it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Annotation {
    pub sets: Vec<GraphSet>,
    pub truncated: bool,
    /// Whether a set other than one graph that asserts the fact may be missing, here or in
    /// a premise's: what a reader who reads none of those graphs depends on. Implies
    /// `truncated`, which also marks a fact whose own graphs alone pass the cap (and passes
    /// to what is derived from it); equal to it for a fact only inferred.
    pub beyond_own: bool,
}

impl Annotation {
    /// The empty set alone: what a fact without premises has, and where a product starts.
    fn empty_set() -> Self {
        Self {
            sets: vec![GraphSet::default()],
            truncated: false,
            beyond_own: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// The bytes it holds on the heap: its list, and the words of sets past two.
    pub fn heap_bytes(&self) -> usize {
        self.sets.capacity() * size_of::<GraphSet>()
            + self.sets.iter().map(GraphSet::heap_bytes).sum::<usize>()
    }
}

/// Why an evaluation stopped before its fixpoint; nothing it computed is to be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Its working set grew to `bytes`, more than its [`Watch`] let it hold.
    Memory { bytes: usize },
    /// The process held more than its memory limit ([`super::eval::over_memory_limit`]).
    ProcessMemory,
}

/// What an evaluation reports its working set's bytes to at round boundaries: `false`
/// stops it ([`Stopped::Memory`]).
pub type Watch<'a> = &'a (dyn Fn(usize) -> bool + Sync);

fn unwatched(_: usize) -> bool {
    true
}

/// A [`Watch`] that lets an evaluation hold anything (the process limit still applies).
pub const UNWATCHED: Watch<'static> = &unwatched;

/// `Err` if the process is past its memory limit or `watch` refuses `bytes`.
fn check(watch: Watch<'_>, bytes: usize) -> Result<(), Stopped> {
    if super::eval::over_memory_limit() {
        return Err(Stopped::ProcessMemory);
    }
    match watch(bytes) {
        true => Ok(()),
        false => Err(Stopped::Memory { bytes }),
    }
}

/// The bytes a hash map of `capacity` entries of `(K, V)` holds in its table.
fn table_bytes<K, V>(capacity: usize) -> usize {
    capacity * (size_of::<(K, V)>() + 1)
}

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

    /// The bytes it holds on the heap: none up to 128 graphs, else its words.
    pub fn heap_bytes(&self) -> usize {
        match self.0.spilled() {
            true => self.0.capacity() * size_of::<u64>(),
            false => 0,
        }
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
    fn sets_of(&self, fact: &Triple) -> Option<Cow<'_, Annotation>>;
}

impl Lookup for HashMap<Triple, Annotation> {
    fn sets_of(&self, fact: &Triple) -> Option<Cow<'_, Annotation>> {
        self.get(fact).map(Cow::Borrowed)
    }
}

/// During an update: the facts it recomputes as they are now, the others as they were.
struct Recomputing<'a> {
    now: &'a HashMap<Triple, Annotation>,
    affected: &'a HashSet<Triple>,
    previous: &'a (dyn Fn(Triple) -> Annotation + Sync),
}

impl Lookup for Recomputing<'_> {
    fn sets_of(&self, fact: &Triple) -> Option<Cow<'_, Annotation>> {
        if self.affected.contains(fact) {
            return self.now.sets_of(fact);
        }
        let previous = (self.previous)(*fact);
        (!previous.is_empty()).then_some(Cow::Owned(previous))
    }
}

/// The sets a fact starts with: the empty set for an axiom, else one per graph that
/// holds it (none for a fact only inferred). At most `cap`, marked truncated past it.
pub fn initial_sets(graphs: Vec<u32>, axiom: bool, cap: usize) -> Annotation {
    match axiom {
        true => Annotation::empty_set(),
        false => minimal(graphs.into_iter().map(GraphSet::of).collect(), cap, &|_| {
            true
        }),
    }
}

/// `sets` without those that contain another, the smallest first, at most `cap`; marked
/// truncated if the cap left out a set that no kept one is contained in, and beyond its own
/// graphs if one of those isn't `own` (one graph that asserts the fact).
fn minimal(mut sets: Vec<GraphSet>, cap: usize, own: &dyn Fn(&GraphSet) -> bool) -> Annotation {
    sets.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    sets.dedup();
    let mut out: Vec<GraphSet> = Vec::new();
    let (mut truncated, mut beyond_own) = (false, false);
    for set in sets {
        if out.iter().any(|kept| kept.is_subset(&set)) {
            continue;
        }
        if out.len() == cap {
            truncated = true;
            if !own(&set) {
                beyond_own = true;
                break;
            }
            continue;
        }
        out.push(set);
    }
    Annotation {
        sets: out,
        truncated,
        beyond_own,
    }
}

/// Whether a set is one graph that asserts `fact`, its graphs looked up once, when first
/// asked ([`minimal`]'s `own`).
fn own_graph_of(
    fact: Triple,
    graphs_of: &(dyn Fn(Triple) -> Vec<u32> + Sync),
) -> impl Fn(&GraphSet) -> bool + '_ {
    let graphs = std::cell::OnceCell::new();
    move |set: &GraphSet| {
        let mut members = set.graphs();
        matches!((members.next(), members.next()), (Some(graph), None)
            if graphs.get_or_init(|| graphs_of(fact)).contains(&graph))
    }
}

/// The support graph sets of every fact of the closure `facts` under `program`.
/// `graphs_of` gives an asserted fact's graphs (none for an inferred one); `axioms`
/// (sorted) hold in no graph, for every reader (a ruleset's axiomatic triples); `cap`
/// bounds the sets kept per fact. `watch` sees the working set's bytes after each round.
pub fn support_sets(
    facts: &[Triple],
    program: &GroundProgram,
    axioms: &[Triple],
    graphs_of: &(dyn Fn(Triple) -> Vec<u32> + Sync),
    cap: usize,
    watch: Watch<'_>,
) -> Result<HashMap<Triple, Annotation>, Stopped> {
    let all = Store::new(facts.to_vec());
    let initial: Vec<(Triple, Annotation)> = facts
        .par_iter()
        .filter_map(|&fact| {
            let axiom = axioms.binary_search(&fact).is_ok();
            let initial =
                initial_sets(if axiom { Vec::new() } else { graphs_of(fact) }, axiom, cap);
            (!initial.is_empty()).then_some((fact, initial))
        })
        .collect();
    // The heap bytes of the annotations the map holds, kept as they change.
    let mut annotations: usize = initial.iter().map(|(_, a)| a.heap_bytes()).sum();
    let mut sets: HashMap<Triple, Annotation> = HashMap::with_capacity(facts.len());
    sets.extend(initial);
    let empty = Annotation::empty_set();
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
        merge(&mut sets, fact, from, cap, graphs_of, &mut annotations);
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
    // What the round doesn't change: the closure's store and the premise maps.
    let fixed = all.bytes().total()
        + table_bytes::<Triple, Vec<Triple>>(bodiless_by_premise.capacity())
        + table_bytes::<Triple, Vec<usize>>(rules_by_premise.capacity())
        + bodiless_by_premise
            .values()
            .map(|v| v.capacity())
            .sum::<usize>()
            * size_of::<Triple>()
        + rules_by_premise
            .values()
            .map(|v| v.capacity())
            .sum::<usize>()
            * size_of::<usize>();
    let held = |sets: &HashMap<Triple, Annotation>, annotations: usize| {
        fixed + table_bytes::<Triple, Annotation>(sets.capacity()) + annotations
    };
    check(watch, held(&sets, annotations))?;
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
        // jobs in parallel morsels, each emitting (head, set, whether the instance's sets
        // are truncated) triples.
        let mut found: Vec<(Triple, GraphSet, bool)> = Vec::new();
        for fact in delta_facts(&delta) {
            for &bodiless in bodiless_by_premise.get(&fact).into_iter().flatten() {
                let grounding = Grounding::Bodiless(bodiless);
                let from = grounding_sets(program, grounding, &sets, cap, &empty);
                found.extend(
                    from.sets
                        .into_iter()
                        .map(|set| (bodiless, set, from.truncated)),
                );
            }
        }
        let mut jobs: Vec<(Job<'_>, &[super::ir::Atom], Annotation)> = Vec::new();
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
                    out.extend(
                        from.sets
                            .iter()
                            .map(|set| (fact, set.clone(), from.truncated)),
                    );
                }
            });
            out
        }));
        // The round's peak: its derivations and its delta beside the sets.
        let round = found.capacity() * size_of::<(Triple, GraphSet, bool)>()
            + found
                .iter()
                .map(|(_, set, _)| set.heap_bytes())
                .sum::<usize>()
            + delta.bytes().total()
            + jobs.capacity() * size_of::<(Job<'_>, &[super::ir::Atom], Annotation)>()
            + tasks.capacity() * size_of::<(usize, std::ops::Range<usize>)>();
        check(watch, held(&sets, annotations) + round)?;
        drop((jobs, tasks));
        // Per head: its new sets merged into its old ones.
        found.par_sort_unstable();
        found.dedup();
        let mut starts: Vec<usize> = (0..found.len())
            .filter(|&i| i == 0 || found[i - 1].0 != found[i].0)
            .collect();
        starts.push(found.len());
        // In parallel against the sets as they are; then the changes written back. A
        // change of the mark alone is news too: the facts derived from it are marked.
        let updates: Vec<(Triple, Annotation)> = starts
            .par_windows(2)
            .filter_map(|w| {
                let group = &found[w[0]..w[1]];
                let fact = group[0].0;
                let (kept, truncated, beyond_own) =
                    sets.get(&fact).map_or((&[][..], false, false), |a| {
                        (a.sets.as_slice(), a.truncated, a.beyond_own)
                    });
                let marks = group.iter().any(|&(_, _, marked)| marked);
                // Nothing new: every found set is covered by a kept one, no new mark (a
                // derivation's mark is one beyond the head's own graphs, so both).
                if (beyond_own || !marks)
                    && group
                        .iter()
                        .all(|(_, set, _)| kept.iter().any(|k| k.is_subset(set)))
                {
                    return None;
                }
                let mut all = kept.to_vec();
                all.extend(group.iter().map(|(_, set, _)| set.clone()));
                let mut next = minimal(all, cap, &own_graph_of(fact, graphs_of));
                next.truncated |= truncated || marks;
                next.beyond_own |= beyond_own || marks;
                (next.sets.as_slice() != kept
                    || next.truncated != truncated
                    || next.beyond_own != beyond_own)
                    .then_some((fact, next))
            })
            .collect();
        drop((found, starts));
        for (fact, next) in updates {
            annotations += next.heap_bytes();
            if let Some(old) = sets.insert(fact, next) {
                annotations = annotations.saturating_sub(old.heap_bytes());
            }
            changed.push(fact);
        }
        check(watch, held(&sets, annotations))?;
    }
    Ok(sets)
}

/// The facts of the closure `facts` that hold for a reader of the statements `readable`
/// accepts: the closure of what it reads, axioms included, by the evaluation that gives
/// the support graph sets ([`support_sets`]), every readable statement in one graph. A
/// fact then has one set or none, so no cap applies and no mark arises. The exact
/// decision for the facts whose kept sets don't decide a reader ([`Annotation::truncated`]),
/// the conservative way: the reader's whole closure, each fact with its one set.
pub fn closure_within(
    facts: &[Triple],
    program: &GroundProgram,
    axioms: &[Triple],
    readable: &(dyn Fn(Triple) -> bool + Sync),
    watch: Watch<'_>,
) -> Result<HashMap<Triple, Annotation>, Stopped> {
    let one_graph = |fact: Triple| match readable(fact) {
        true => vec![0],
        false => Vec::new(),
    };
    let mut sets = support_sets(facts, program, axioms, &one_graph, 1, watch)?;
    sets.retain(|_, annotation| !annotation.is_empty());
    debug_assert!(sets.values().all(|annotation| !annotation.truncated));
    Ok(sets)
}

/// Whether `fact` follows in one step from facts of `source` other than itself: it is an
/// axiom, a bodiless fact one of whose groundings holds without it, or the head of a rule
/// instance (transitivity included) one of whose groundings holds without it and none of
/// whose premises is it. A rule's instance with the fact among its own premises, such as
/// `(x p x), (x p z) -> (x p z)`, doesn't count: it derives nothing the fact itself
/// doesn't give.
///
/// Necessary for following from the other facts, not sufficient: the premises may hold
/// only through the fact itself (`C ⊑ D`, `D ⊑ C`, `a : C` gives `a : D`, which gives
/// `a : C` back). [`independently_derived`] decides.
pub fn derived_otherwise<S: Source + ?Sized>(
    source: &S,
    program: &GroundProgram,
    axioms: &[Triple],
    fact: Triple,
) -> bool {
    if axioms.binary_search(&fact).is_ok() {
        return true;
    }
    let holds = |premises: &[Triple]| premises.iter().all(|&p| p != fact && source.contains(p));
    let grounded = |grounding: Grounding| {
        program
            .premise_alternatives(grounding)
            .into_iter()
            .any(holds)
    };
    if program.bodiless.contains(&fact) && grounded(Grounding::Bodiless(fact)) {
        return true;
    }
    let derives = |rule: &Rule| {
        let mut found = false;
        let mut body = Vec::with_capacity(rule.body.len());
        derivations(source, rule, fact, Seg::All, &mut |bindings| {
            body.clear();
            body.extend(
                rule.body
                    .iter()
                    .map(|atom| instantiate_head(atom, bindings)),
            );
            found = body.iter().all(|&premise| premise != fact);
            found
        });
        found
    };
    if program
        .producers(fact)
        .into_iter()
        .any(|r| grounded(Grounding::Rule(r)) && derives(&program.rules[r]))
    {
        return true;
    }
    let p = fact[1];
    program.transitive.contains(&p)
        && grounded(Grounding::Transitive(p))
        && derives(&transitivity(p))
}

/// Whether the asserted `fact` of the closure `source` is in the least closure of the
/// asserted facts without it (`asserted`: whether a fact is asserted), under `program`
/// (grounded on the whole closure: a grounding's schema premises are premises like the
/// others, so a schema fact that follows only from `fact` grounds nothing here), axioms
/// included. Its forward cone (the facts some derivation of which uses it, transitively)
/// is evaluated again from the rest: every other asserted fact in one graph, `fact` in
/// none, cap 1, by [`update_support_sets`]; a fact outside the cone keeps every
/// derivation, so it holds without `fact`. A cycle through `fact` supports nothing, as no
/// least fixpoint starts from it.
///
/// `Ok(None)` where the cone reaches a schema fact or passes `limit` facts: then the
/// caller decides with the reader-free closure of the asserted facts without `fact`
/// ([`closure_within`]), exact as well. Work: the cone's derivations; `watch` sees its
/// working set after each round.
pub fn independently_derived<S: Source + Sync + ?Sized>(
    source: &S,
    program: &GroundProgram,
    axioms: &[Triple],
    asserted: &(dyn Fn(Triple) -> bool + Sync),
    fact: Triple,
    limit: usize,
    watch: Watch<'_>,
) -> Result<Option<bool>, Stopped> {
    let derivable = Annotation {
        sets: vec![GraphSet::of(0)],
        ..Annotation::default()
    };
    let empty = Annotation::empty_set();
    let previous = |other: Triple| match axioms.binary_search(&other).is_ok() {
        true => empty.clone(),
        false => derivable.clone(),
    };
    let one_graph = |other: Triple| match other != fact && asserted(other) {
        true => vec![0],
        false => Vec::new(),
    };
    let change = Change {
        source,
        in_closure: &|other| source.contains(other),
        touched: &[fact],
        previous: &previous,
        limit,
    };
    let found = update_support_sets(&change, program, axioms, &one_graph, 1, watch)?;
    Ok(found.map(|sets| sets.get(&fact).is_some_and(|found| !found.is_empty())))
}

/// The facts of `candidates` (sorted, each asserted in the closure `source`) that follow
/// from the closure's other facts without their own assertion, sorted, and those this
/// can't decide, sorted. A one-step derivation from other facts ([`derived_otherwise`],
/// checked in parallel) is necessary; [`independently_derived`] then decides each in
/// turn. The undecided ones (their cone reaches a schema fact or passes `limit`) the
/// caller decides with the closure of the asserted facts without each ([`closure_within`]).
/// `watch` sees the candidates' bytes before the checks, and each check's working set.
pub fn rederived<S: Source + Sync + ?Sized>(
    source: &S,
    program: &GroundProgram,
    axioms: &[Triple],
    asserted: &(dyn Fn(Triple) -> bool + Sync),
    candidates: &[Triple],
    limit: usize,
    watch: Watch<'_>,
) -> Result<(Vec<Triple>, Vec<Triple>), Stopped> {
    check(watch, size_of_val(candidates))?;
    let one_step: Vec<Triple> = candidates
        .par_iter()
        .copied()
        .filter(|&fact| derived_otherwise(source, program, axioms, fact))
        .collect();
    check(
        watch,
        (candidates.len() + 2 * one_step.capacity()) * size_of::<Triple>(),
    )?;
    let (mut found, mut undecided) = (Vec::with_capacity(one_step.len()), Vec::new());
    for fact in one_step {
        if axioms.binary_search(&fact).is_ok() {
            found.push(fact);
            continue;
        }
        match independently_derived(source, program, axioms, asserted, fact, limit, watch)? {
            Some(true) => found.push(fact),
            Some(false) => {}
            None => undecided.push(fact),
        }
    }
    Ok((found, undecided))
}

/// The facts a change can alter the independent derivation of ([`rederived`]): those it
/// touched (`touched`, sorted: their graphs, or
/// whether they are in the closure) and their forward cone in `source` (the closure after
/// the change, and the facts it removed): every fact a derivation of which uses a touched
/// fact, transitively. A fact outside it has the derivations it had, over facts that
/// didn't change. Sorted, each once. `None` when the cone reaches a fact the program was
/// grounded on (the grounding changes) or passes `limit` facts: the caller checks all.
pub fn rederived_candidates<S: Source + Sync + ?Sized>(
    source: &S,
    program: &GroundProgram,
    touched: &[Triple],
    limit: usize,
    watch: Watch<'_>,
) -> Result<Option<Vec<Triple>>, Stopped> {
    let schema = program.schema_premises();
    let rules = body_rules(program);
    let rules: Vec<(&Rule, Grounding)> =
        rules.iter().map(|(rule, g)| (rule.as_ref(), *g)).collect();
    let fixed = table_bytes::<Triple, ()>(schema.capacity());
    let Some(cone) = forward_cone(source, &rules, &schema, touched, limit, fixed, watch)? else {
        return Ok(None);
    };
    let mut cone: Vec<Triple> = cone.into_iter().collect();
    check(watch, fixed + cone.capacity() * size_of::<Triple>())?;
    cone.par_sort_unstable();
    Ok(Some(cone))
}

/// The rules with a body and fact heads of `program`, transitivity included, each with its
/// grounding: what a forward cone follows.
fn body_rules(program: &GroundProgram) -> Vec<(std::borrow::Cow<'_, Rule>, Grounding)> {
    program
        .rules
        .iter()
        .enumerate()
        .map(|(r, rule)| (std::borrow::Cow::Borrowed(rule), Grounding::Rule(r)))
        .chain(program.transitive.iter().map(|&p| {
            (
                std::borrow::Cow::Owned(transitivity(p)),
                Grounding::Transitive(p),
            )
        }))
        .filter(|(rule, _)| !rule.body.is_empty() && matches!(rule.head, Head::Facts(_)))
        .collect()
}

/// `touched` and every fact of `source` a derivation of which (by `rules`) uses one of
/// them, transitively. `None` once it reaches a fact of `schema` or passes `limit` facts.
/// `watch` sees `fixed` and what it holds after each round.
fn forward_cone<S: Source + Sync + ?Sized>(
    source: &S,
    rules: &[(&Rule, Grounding)],
    schema: &HashSet<Triple>,
    touched: &[Triple],
    limit: usize,
    fixed: usize,
    watch: Watch<'_>,
) -> Result<Option<HashSet<Triple>>, Stopped> {
    let mut affected: HashSet<Triple> = touched.iter().copied().collect();
    if affected.iter().any(|fact| schema.contains(fact)) {
        return Ok(None);
    }
    let mut frontier: Vec<Triple> = affected.iter().copied().collect();
    while !frontier.is_empty() {
        frontier.sort_unstable();
        let delta = Store::new(std::mem::take(&mut frontier));
        let changed = Changed {
            all: source,
            changed: &delta,
        };
        let heads = instances(&changed, rules, &|k, bindings, out| {
            let rule = rules[k].0;
            if let Head::Facts(heads) = &rule.head {
                out.extend(heads.iter().map(|head| instantiate_head(head, bindings)));
            }
        });
        check(
            watch,
            fixed
                + table_bytes::<Triple, ()>(affected.capacity())
                + delta.bytes().total()
                + heads.capacity() * size_of::<Triple>(),
        )?;
        for head in heads {
            if affected.insert(head) {
                if schema.contains(&head) {
                    return Ok(None);
                }
                frontier.push(head);
            }
        }
        if affected.len() > limit {
            return Ok(None);
        }
    }
    Ok(Some(affected))
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
    /// A fact's sets before the change, with its mark.
    pub previous: &'a (dyn Fn(Triple) -> Annotation + Sync),
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
/// [`GroundProgram::schema_premises`]). `Ok(None)` when the change reaches a schema
/// fact, or affects more than `change.limit` facts: then [`support_sets`] computes them
/// all. `watch` sees the working set's bytes after each round.
///
/// The affected facts start again from their own graphs, unmarked unless those alone
/// pass the cap (a mark is a fact of one fixpoint: kept across a change, a mark whose
/// cause the change removed would stay): one pass over each one's derivations takes in
/// what the unaffected facts support, then semi-naive rounds among the affected facts
/// reach the least fixpoint, as [`support_sets`] does for all.
pub fn update_support_sets<S: Source + Sync + ?Sized>(
    change: &Change<'_, S>,
    program: &GroundProgram,
    axioms: &[Triple],
    graphs_of: &(dyn Fn(Triple) -> Vec<u32> + Sync),
    cap: usize,
    watch: Watch<'_>,
) -> Result<Option<HashMap<Triple, Annotation>>, Stopped> {
    let schema = program.schema_premises();
    let owned = body_rules(program);
    let rules: Vec<(&Rule, Grounding)> =
        owned.iter().map(|(rule, g)| (rule.as_ref(), *g)).collect();
    // The affected facts: the touched ones and what follows from them.
    let fixed = table_bytes::<Triple, ()>(schema.capacity());
    let Some(affected) = forward_cone(
        change.source,
        &rules,
        &schema,
        change.touched,
        change.limit,
        fixed,
        watch,
    )?
    else {
        return Ok(None);
    };
    let members: HashSet<Triple> = affected
        .iter()
        .copied()
        .filter(|&fact| (change.in_closure)(fact))
        .collect();
    let empty = Annotation::empty_set();
    let mut now: HashMap<Triple, Annotation> = members
        .iter()
        .map(|&fact| {
            let axiom = axioms.binary_search(&fact).is_ok();
            let graphs = if axiom { Vec::new() } else { graphs_of(fact) };
            (fact, initial_sets(graphs, axiom, cap))
        })
        .collect();
    let mut annotations: usize = now.values().map(Annotation::heap_bytes).sum();
    let fixed = fixed
        + table_bytes::<Triple, ()>(affected.capacity())
        + table_bytes::<Triple, ()>(members.capacity());
    let held = |now: &HashMap<Triple, Annotation>, annotations: usize| {
        fixed + table_bytes::<Triple, Annotation>(now.capacity()) + annotations
    };
    check(watch, held(&now, annotations))?;
    // The schema facts' part per rule: they aren't affected, so it is fixed.
    let schemas: Vec<Annotation> = {
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
    let found: Vec<(Triple, Annotation)> = {
        let lookup = Recomputing {
            now: &now,
            affected: &affected,
            previous: change.previous,
        };
        let list: Vec<Triple> = members.iter().copied().collect();
        list.par_iter()
            .map(|&fact| {
                let mut from = Annotation::default();
                let mut take = |found: Annotation| {
                    from.truncated |= found.truncated;
                    from.sets.extend(found.sets);
                };
                if program.bodiless.contains(&fact) {
                    take(grounding_sets(
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
                            take(premise_sets_from(&body, schema, &lookup, cap));
                        }
                        false
                    });
                }
                (fact, from)
            })
            .collect()
    };
    check(
        watch,
        held(&now, annotations)
            + found.capacity() * size_of::<(Triple, Annotation)>()
            + found.iter().map(|(_, a)| a.heap_bytes()).sum::<usize>(),
    )?;
    let mut changed: Vec<Triple> = Vec::new();
    for (fact, from) in found {
        if merge(&mut now, fact, from, cap, graphs_of, &mut annotations) {
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
                    if members.contains(&fact) && !from.is_empty() {
                        out.push((fact, from.clone()));
                    }
                }
            })
        };
        check(
            watch,
            held(&now, annotations)
                + delta.bytes().total()
                + found.capacity() * size_of::<(Triple, Annotation)>()
                + found.iter().map(|(_, a)| a.heap_bytes()).sum::<usize>(),
        )?;
        let mut by_fact: HashMap<Triple, Annotation> = HashMap::new();
        for (fact, from) in found {
            let entry = by_fact.entry(fact).or_default();
            entry.truncated |= from.truncated;
            entry.sets.extend(from.sets);
        }
        for (fact, from) in by_fact {
            if merge(&mut now, fact, from, cap, graphs_of, &mut annotations) {
                changed.push(fact);
            }
        }
    }
    Ok(Some(now))
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
/// minimal unions of one set per premise, marked if an alternative's were or the cap
/// cut them; none if no alternative has sets yet.
fn grounding_sets<L: Lookup + ?Sized>(
    program: &GroundProgram,
    grounding: Grounding,
    sets: &L,
    cap: usize,
    empty: &Annotation,
) -> Annotation {
    let (mut all, mut truncated) = (Vec::new(), false);
    for premises in program.premise_alternatives(grounding) {
        let found = premise_sets_from(premises, empty, sets, cap);
        truncated |= found.truncated;
        all.extend(found.sets);
    }
    // A derivation's sets: whatever they lack is beyond the head's own graphs.
    let mut out = minimal(all, cap, &|_| false);
    out.truncated |= truncated;
    out.beyond_own |= truncated;
    out
}

/// The minimal unions of the sets `start` with one set per premise of `body`, marked if
/// `start`'s, a premise's or a step's were; none if a premise has none yet.
fn premise_sets_from<L: Lookup + ?Sized>(
    body: &[Triple],
    start: &Annotation,
    sets: &L,
    cap: usize,
) -> Annotation {
    // The usual case, one set everywhere: a single union, nothing to minimise.
    if let [only] = start.sets.as_slice() {
        let mut one = only.clone();
        let mut truncated = start.truncated;
        let mut single = true;
        for premise in body {
            let Some(own) = sets.sets_of(premise) else {
                return Annotation::default();
            };
            match own.sets.as_slice() {
                [set] => {
                    one = one.union(set);
                    truncated |= own.truncated;
                }
                _ => {
                    single = false;
                    break;
                }
            }
        }
        if single {
            return Annotation {
                sets: vec![one],
                truncated,
                beyond_own: truncated,
            };
        }
    }
    let mut product = start.clone();
    for premise in body {
        let Some(own) = sets.sets_of(premise) else {
            return Annotation::default();
        };
        let mut next = Vec::with_capacity(product.sets.len() * own.sets.len());
        for a in &product.sets {
            for b in own.sets.iter() {
                next.push(a.union(b));
            }
        }
        let truncated = product.truncated || own.truncated;
        product = minimal(next, cap, &|_| false);
        product.truncated |= truncated;
        product.beyond_own |= truncated;
    }
    // A premise without sets yet: the instance gives nothing now, whatever was cut.
    match product.is_empty() {
        true => Annotation::default(),
        false => product,
    }
}

/// Adds `from` (derivations of `fact`) to `fact`'s sets, `annotations` following the heap
/// bytes; whether its sets or its marks changed (news for the facts derived from it).
fn merge(
    sets: &mut HashMap<Triple, Annotation>,
    fact: Triple,
    from: Annotation,
    cap: usize,
    graphs_of: &(dyn Fn(Triple) -> Vec<u32> + Sync),
    annotations: &mut usize,
) -> bool {
    let current = sets.entry(fact).or_default();
    let mut all = current.sets.clone();
    all.extend(from.sets);
    let mut next = minimal(all, cap, &own_graph_of(fact, graphs_of));
    next.truncated |= current.truncated || from.truncated;
    next.beyond_own |= current.beyond_own || from.truncated;
    if next == *current {
        return false;
    }
    *annotations = (*annotations + next.heap_bytes()).saturating_sub(current.heap_bytes());
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
