//! The batch executor (reasoner-v2 design §4.1): full materialisation by semi-naive
//! evaluation over a vertically partitioned working set.
//!
//! - **Working set.** One `Relation<R>` per predicate: its pairs sorted by subject and by
//!   object, plus the last round's delta in both orders. Runs that are merged are held
//!   in chunks, so a merge never holds its sources beside its result (P1-F10), and a
//!   round's candidates stay in their morsels' lists until they are merged (P1-F9).
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
use super::eval::Family;
pub use super::eval::Schema;
use super::eval::{
    AllFacts, GroundProgram, Job, NEVER, Probes, Seg, Source, guards_hold, run_jobs_by_morsel,
};
use super::ir::{Head, Rule, Term};
use super::ir::{Triple, Violation};
use super::lists::{ListVocabulary, instantiate};
use super::representatives::EqualityClasses;
use nrese_exec::heap;
use nrese_exec::search::Finger;

mod dense;
mod join;
mod modules;
mod representation;
mod result;
mod runs;
mod store;
#[cfg(test)]
mod width_tests;

use modules::*;
pub(crate) use modules::{Equality, is_replacement_rule, same_as_of};
pub use result::{
    Counters, DerivedRelation, Input, Listing, Materialisation, Output, Phases, StoreBytes,
};
pub use runs::set_chunk_pairs;
use runs::*;
pub(crate) type Store = store::Store<[u64; 2]>;
pub(crate) use representation::Representation;
pub(crate) use store::Store as WorkingStore;
use store::*;

/// The store as the rules read it: without stale facts (a former representative in them).
struct Current<'a, R: Representation> {
    store: &'a WorkingStore<R>,
    stale: &'a TermSet,
}

impl<R: Representation> Source for Current<'_, R> {
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        if self.stale.is_empty() {
            return self.store.scan(pattern, seg, f);
        }
        self.store.scan(pattern, seg, &mut |fact| {
            if !self.stale.mentioned(fact) {
                f(fact);
            }
        });
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        self.store.estimate(pattern, seg)
    }

    fn contains(&self, fact: Triple) -> bool {
        !self.stale.mentioned(fact) && self.store.contains(fact)
    }

    fn matches_len(&self, pattern: [Option<u64>; 3], seg: Seg) -> Option<usize> {
        self.store.matches_len(pattern, seg)
    }

    fn scan_range(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        f: &mut dyn FnMut(Triple),
    ) {
        if self.stale.is_empty() {
            return self.store.scan_range(pattern, seg, range, f);
        }
        self.store.scan_range(pattern, seg, range, &mut |fact| {
            if !self.stale.mentioned(fact) {
                f(fact);
            }
        });
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
    materialise_owned_until(input, rules, lists, schema, NEVER).expect("never stopped")
}

/// [`materialise_owned`], polling `stop` as [`materialise_grouped_until`] does.
pub fn materialise_owned_until(
    input: Vec<Triple>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let program = Program {
        rules,
        lists,
        schema,
    };
    materialise_dense(Input::Facts(input), program, None, Output::Facts, stop)
}

/// [`materialise`] over input already grouped the way the working set stores it: per
/// predicate (each once), its `(object, subject)` pairs, sorted and distinct. A store can
/// stream this from a predicate-object-subject index without an intermediate triple list or
/// sort, holding about 32 bytes per input fact instead of about 72.
pub fn materialise_grouped(
    input: Vec<(u64, Vec<[u64; 2]>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    materialise_grouped_until(input, rules, lists, schema, Output::Facts, NEVER)
        .expect("never stopped")
}

/// [`materialise_grouped`], handing back what it derived as `output` says, polling `stop`
/// in every round, rule job and module; a fired `stop` ends it with [`Interrupted`] and
/// nothing of the partial closure.
pub fn materialise_grouped_until(
    input: Vec<(u64, Vec<[u64; 2]>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    output: Output,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let program = Program {
        rules,
        lists,
        schema,
    };
    materialise_dense(Input::Grouped(input), program, None, output, stop)
}

/// The closure of `input` under `rules` with equality by representatives
/// ([`super::representatives`]): each `owl:sameAs` class one term, its smallest id, the
/// replacement rules (`eq-rep-*`) and `eq-sym`/`eq-trans` replaced by the classes
/// themselves. A round's new `sameAs` between two representatives merges their classes,
/// and the facts mentioning the representative that lost its place are rewritten into the
/// next round's delta: no outer loop, however long a cascade of merges. `listing` says
/// what [`Materialisation::derived`] holds; rules without equality reasoning give the
/// plain closure.
pub fn materialise_representatives_until(
    input: Input,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    listing: Listing,
    output: Output,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let program = Program {
        rules,
        lists,
        schema,
    };
    materialise_dense(input, program, Some(listing), output, stop)
}

/// What a materialisation evaluates: the rules, the list vocabulary their list axioms
/// use, and the schema their grounding reads.
#[derive(Clone, Copy)]
struct Program<'a> {
    rules: &'a [Rule],
    lists: Option<&'a ListVocabulary>,
    schema: &'a Schema,
}

/// The materialisation of `input` under `program`, over the ids numbered densely
/// ([`dense`]): the input and the program in numbers, the rounds over them, the result in
/// ids again. With `listing`, equality by representatives where the rules reason with
/// equality ([`materialise_representatives_until`]).
fn materialise_dense(
    input: Input,
    program: Program<'_>,
    listing: Option<Listing>,
    output: Output,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    materialise_with_capacity(input, program, listing, output, stop, u32::MAX as usize)
}

/// Capacity is representation selection, not an execution limit. Tests use small values.
fn materialise_with_capacity(
    input: Input,
    program: Program<'_>,
    listing: Option<Listing>,
    output: Output,
    stop: super::eval::Stop<'_>,
    capacity: usize,
) -> Result<Materialisation, Interrupted> {
    if stop() || super::eval::over_memory_limit() {
        return Err(Interrupted);
    }
    let clock = std::time::Instant::now();
    let dense = dense::Dense::of(
        &input,
        program.rules,
        program.lists,
        program.schema,
        capacity,
    );
    if stop() || super::eval::over_memory_limit() {
        return Err(Interrupted);
    }
    let mut result = match dense {
        Some(dense) => {
            let rules: Vec<Rule> = program.rules.iter().map(|rule| dense.rule(rule)).collect();
            let lists = program.lists.map(|lists| dense.lists(lists));
            let schema = dense.schema(program.schema);
            let store = dense.store(input);
            let mut result = solve(
                store,
                clock.elapsed(),
                &rules,
                lists.as_ref(),
                &schema,
                listing,
                output,
                stop,
            )?;
            dense.back(&mut result);
            result.counters.ids_bytes = dense.table().len() * std::mem::size_of::<u64>();
            result
        }
        None => {
            let store = match input {
                Input::Facts(facts) => WorkingStore::<[u64; 2]>::new(facts),
                Input::Grouped(groups) => WorkingStore::<[u64; 2]>::from_groups(groups),
            };
            solve(
                store,
                clock.elapsed(),
                program.rules,
                program.lists,
                program.schema,
                listing,
                output,
                stop,
            )?
        }
    };
    if output == Output::Relations && !result.derived.is_empty() {
        result
            .relations
            .extend(DerivedRelation::of_facts(std::mem::take(
                &mut result.derived,
            )));
    }
    // Remapping/listed output can allocate too: discard it if the existing stop fired.
    if stop() || super::eval::over_memory_limit() {
        return Err(Interrupted);
    }
    Ok(result)
}

/// Exercise the real selector with bounded test data, without adding a resource limit.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialise_test_capacity(
    input: Input,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    listing: Option<Listing>,
    output: Output,
    stop: super::eval::Stop<'_>,
    capacity: usize,
) -> Result<Materialisation, Interrupted> {
    materialise_with_capacity(
        input,
        Program {
            rules,
            lists,
            schema,
        },
        listing,
        output,
        stop,
        capacity,
    )
}

/// The one solver entry, instantiated for packed numbers or full-width IDs.
#[allow(clippy::too_many_arguments)]
fn solve<R: Representation>(
    store: WorkingStore<R>,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    listing: Option<Listing>,
    output: Output,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let representatives = listing.and_then(|listing| {
        super::representatives::same_as(rules).map(|same_as| Representatives {
            same_as,
            classes: EqualityClasses::default(),
            stale: TermSet::default(),
            shared: TermSet::default(),
            listing,
            merges: 0,
        })
    });
    if representatives.is_none() && listing == Some(Listing::Representatives) {
        let input = store.facts_where(&|_| true);
        let mut result = run(store, load, rules, lists, schema, None, Output::Facts, stop)?;
        result.derived.extend(input);
        result.derived.par_sort_unstable();
        result.derived.dedup();
        Ok(result)
    } else {
        run(
            store,
            load,
            rules,
            lists,
            schema,
            representatives,
            output,
            stop,
        )
    }
}

/// The rules equality by representatives runs: without the ones the classes stand for.
fn under_representatives(rules: &[Rule]) -> Vec<Rule> {
    rules
        .iter()
        .filter(|r| !is_replacement_rule(r) && !matches!(r.name.as_str(), "eq-sym" | "eq-trans"))
        .cloned()
        .collect()
}

/// Semi-naive evaluation to the fixpoint, from a store holding the input as its delta;
/// with equality by representatives if `representatives` is given.
#[allow(clippy::too_many_arguments)]
fn run<R: Representation>(
    mut store: WorkingStore<R>,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    mut representatives: Option<Representatives>,
    output: Output,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    // The process's memory limit stops it too, whatever the caller polls.
    let stop: super::eval::Stop<'_> = &|| stop() || super::eval::over_memory_limit();
    let check_stop = || if stop() { Err(Interrupted) } else { Ok(()) };
    let mut phases = Phases {
        load,
        passes: 1,
        ..Phases::default()
    };
    let mut result = Materialisation::default();
    let mut program = GroundProgram::default();
    let mut transitive = Transitive::default();
    // Equality by copying (the module), unless it is by representatives.
    let mut equality = Equality::for_rules(rules).filter(|_| representatives.is_none());
    let mut rules: std::borrow::Cow<'_, [Rule]> = match &representatives {
        Some(_) => under_representatives(rules).into(),
        None => rules.into(),
    };
    let mut regrounding = true;
    // Ground every rule again (rules whose constants were merged, rewritten).
    let mut reground_all = false;
    // Facts to add in the first round, checked against the store: the input's stale
    // facts rewritten, where the input asserts equalities.
    let mut pending: Vec<Triple> = Vec::new();
    if let Some(reps) = &mut representatives {
        let mut pairs = Vec::new();
        store.scan([None, Some(reps.same_as), None], Seg::All, &mut |[
            s,
            _,
            o,
        ]| {
            if s != o {
                pairs.push((s, o));
            }
        });
        let lost = reps.merge(&pairs);
        if !lost.is_empty() {
            pending = reps.rewrites(&store, &lost);
            if rules.iter().any(|rule| names_any(rule, &lost)) {
                rules = rules
                    .iter()
                    .map(|r| rewrite_rule(r, &reps.classes))
                    .collect();
            }
        }
    }
    let none = TermSet::default();
    loop {
        check_stop()?;
        result.rounds += 1;
        // Return the last round's freed buffers only when the binary asks for it.
        nrese_engine::memory::release_between_rounds();
        heap::phase("reasoner: grounding");
        let clock = std::time::Instant::now();
        let stale = representatives.as_ref().map_or(&none, |r| &r.stale);
        // Rules from `evaluated` on were added this round: evaluated once in full.
        let evaluated = program.rules.len();
        if regrounding || reground_all {
            let current = Current {
                store: &store,
                stale,
            };
            let mut source = fact_rules(&current, &rules, lists, &mut result.diagnostics);
            if equality.is_some() {
                source.retain(|r| !is_replacement_rule(r));
            }
            // The first round grounds everything; later ones through new schema facts
            // only (list rules in full, deduplicated).
            if result.rounds == 1 || reground_all {
                program.ground(&current, schema, &source);
            } else {
                program.ground_delta(&current, schema, &source);
            }
            reground_all = false;
            for &p in &program.transitive {
                transitive.register(p);
            }
            for (&p, &family) in &program.family_relations {
                store.claim(p, Family::Schema(family));
            }
        }
        // The object orders the rules can use, built where they became needed and
        // dropped where nothing reads them (P1-F11).
        let equal = equality
            .as_ref()
            .is_some_and(|e| store.estimate([None, Some(e.same_as), None], Seg::All) > 0);
        let reads = Reads::of(program.rules.iter());
        transitive.prepare(&store);
        // A closed transitive predicate's joins look it up by object (`Transitive::joined`).
        store.objects(&|p| {
            equal || reads.by_object(p) || schema.read_in_grounding(p) || transitive.closed(p)
        });
        phases.grounding += clock.elapsed();
        heap::phase("reasoner: joins");
        let clock = std::time::Instant::now();
        let current = Current {
            store: &store,
            stale,
        };
        // Heads naming a former representative (constants of rules grounded before it
        // lost its place) derive its representative's fact instead.
        let classes = representatives.as_ref().map(|r| &r.classes);
        let rewrite = |fact: Triple| match classes {
            Some(classes) if stale.mentioned(fact) => classes.rewrite(fact),
            _ => fact,
        };
        let rewrite: Option<&(dyn Fn(Triple) -> Triple + Sync)> =
            (!stale.is_empty()).then_some(&rewrite);
        // Semi-naive variants of the evaluated rules that can match the delta, full
        // evaluation of the new ones.
        let mut facts = program.take_facts();
        if let Some(rewrite) = rewrite {
            facts.iter_mut().for_each(|fact| *fact = rewrite(*fact));
        }
        facts.retain(|&f| !store.contains(f));
        let mut candidates: Vec<Candidates<R>> = vec![(None, segments::<R>(facts))];
        pending.retain(|&fact| !store.contains(fact));
        candidates.push((None, segments::<R>(std::mem::take(&mut pending))));
        // Each job with the closed family its rule belongs to, if any: a variant of such
        // a rule reads the delta without what the family produced, and what a job of the
        // family derives goes to the family's part of the delta.
        let mut jobs = Vec::new();
        let mut families: Vec<Option<Family>> = Vec::new();
        // Transitive predicates closed already: their joins over what others added.
        for (p, rule) in transitive.joined(&store) {
            let delta = Seg::DeltaNotBy(Transitive::family(p));
            for i in 0..2 {
                let seg_of = |j: usize| if j == i { delta } else { Seg::All };
                if let Some(job) = Job::new(&current, rule, i, seg_of) {
                    jobs.push(job);
                    families.push(None);
                }
            }
        }
        for (r, i) in program.variants(&store) {
            if r < evaluated {
                let family = program.closed_family(r).map(Family::Schema);
                let delta = family.map_or(Seg::Delta, Seg::DeltaNotBy);
                if let Some(job) = Job::variant_reading(&current, &program.rules[r], i, delta) {
                    jobs.push(job);
                    families.push(family);
                }
            }
        }
        for (r, rule) in program.rules.iter().enumerate().skip(evaluated) {
            if let Some(job) = Job::full(&current, rule) {
                jobs.push(job);
                families.push(program.closed_family(r).map(Family::Schema));
            }
        }
        let probes = Probes::for_jobs(jobs.len());
        let starts = std::sync::atomic::AtomicU64::new(0);
        let keep = |facts: &mut Vec<R::Candidate>| {
            let n = store.retain_new(facts);
            starts.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        };
        // The joins' kernel over the working set, where no stale fact needs leaving out.
        let plans: Vec<Option<join::Plan<'_, R>>> = match stale.is_empty() {
            true => jobs
                .iter()
                .map(|job| join::Plan::<R>::of(&store, job))
                .collect(),
            false => Vec::new(),
        };
        let kernel = |j: usize, range: std::ops::Range<usize>, out: &mut Vec<R::Candidate>| {
            let plan = plans.get(j)?.as_ref()?;
            Some(plan.run(&store, &jobs[j], range, out))
        };
        candidates.extend(run_jobs_by_morsel(
            &current,
            &jobs,
            &keep,
            rewrite,
            Some(&kernel),
            stop,
            &probes,
            &|job, facts| (families[job], split_packed::<R>(facts)),
        ));
        let counters = &mut result.counters;
        counters.driver_bytes_copied += jobs.iter().map(Job::copied_bytes).sum::<usize>();
        let asked = probes.probes.into_inner();
        counters.probes.push(asked);
        counters.unordered_probes += probes.unordered.into_inner();
        counters.probe_starts += starts.into_inner();
        let mut round = 0;
        for (job, count) in jobs.iter().zip(probes.bindings) {
            let count = count.into_inner();
            round += count;
            match counters.bindings_by_rule.get_mut(&job.rule.name) {
                Some(total) => *total += count,
                None if count > 0 => {
                    counters
                        .bindings_by_rule
                        .insert(job.rule.name.clone(), count);
                }
                None => {}
            }
        }
        counters.bindings.push(round);
        phases.probes += asked;
        phases.bindings += probes.emitted.into_inner();
        for (total, nanos) in phases.join_parts.iter_mut().zip(&probes.nanos) {
            *total +=
                std::time::Duration::from_nanos(nanos.load(std::sync::atomic::Ordering::Relaxed));
        }
        drop(jobs);
        check_stop()?;
        phases.joins += clock.elapsed();
        heap::phase("reasoner: modules");
        let clock = std::time::Instant::now();
        candidates.extend(
            transitive
                .run(&store, representatives.as_ref(), stop)
                .ok_or(Interrupted)?,
        );
        result.counters.closure_pairs.push(transitive.read);
        if let Some(equality) = &mut equality {
            candidates.push((None, segments(equality.run(&store))));
        }
        let mut merged = false;
        // New<R> equalities merge classes: the round's candidates and the stored facts that
        // mention a representative that lost its place are rewritten (egglog's rebuild,
        // within the round). The other candidates stay as they are, in their lists.
        if let Some(reps) = &mut representatives {
            let same_as = reps.same_as;
            let pairs: Vec<(u64, u64)> = candidates
                .iter()
                .flat_map(|(_, segments)| segments)
                .filter(|(p, _)| *p == same_as)
                .flat_map(|(_, pairs)| {
                    pairs
                        .iter()
                        .map(|&q| (R::first(q), R::second(q)))
                        .filter(|(s, o)| s != o)
                })
                .collect();
            let lost = reps.merge(&pairs);
            if !lost.is_empty() {
                let mut moved: Vec<Triple> = Vec::new();
                for (_, list) in &mut candidates {
                    for (p, pairs) in list.iter_mut() {
                        let p = *p;
                        if lost.contains(&p) {
                            moved.extend(pairs.drain(..).map(|q| [R::first(q), p, R::second(q)]));
                        } else {
                            // Sorted and distinct still.
                            pairs.retain(|&q| {
                                let (s, o) = (R::first(q), R::second(q));
                                let stale = lost.contains(&s) || lost.contains(&o);
                                if stale {
                                    moved.push([s, p, o]);
                                }
                                !stale
                            });
                        }
                    }
                }
                moved.extend(reps.rewrites(&store, &lost));
                moved
                    .par_iter_mut()
                    .for_each(|fact| *fact = reps.classes.rewrite(*fact));
                moved.retain(|&fact| !store.contains(fact));
                // Rewritten: no longer what a family produced.
                candidates.push((None, segments::<R>(moved)));
                merged = true;
                if rules.iter().any(|rule| names_any(rule, &lost)) {
                    rules = rules
                        .iter()
                        .map(|r| rewrite_rule(r, &reps.classes))
                        .collect();
                    reground_all = true;
                }
            }
        }
        check_stop()?;
        phases.modules += clock.elapsed();
        heap::phase("reasoner: merge");
        let clock = std::time::Instant::now();
        // Every candidate was checked against the store, which a round doesn't change.
        let (new, schema_facts) = store.advance_new(candidates, schema);
        phases.merge += clock.elapsed();
        phases.new_facts += new as u64;
        result.counters.store_bytes.push(store.bytes());
        if new == 0 && !reground_all {
            break;
        }
        transitive.observe(&store);
        if merged {
            transitive.recompute_all();
        }
        regrounding = schema_facts;
    }
    result.ground_rules = program.rules.len();
    result.transitive = transitive.predicates.len();
    result.counters.member_scans = equality.as_ref().map_or(0, |e| e.member_scans);
    // Return what the last round freed before the checks and inferred-stack build.
    nrese_engine::memory::release_between_rounds();
    heap::phase("reasoner: consistency");
    let clock = std::time::Instant::now();
    let stale = representatives.as_ref().map_or(&none, |r| &r.stale);
    {
        let current = Current {
            store: &store,
            stale,
        };
        program.ground(
            &current,
            schema,
            &consistency_rules(&current, &rules, lists),
        );
    }
    let reads = Reads::of(
        program.rules.iter().chain(
            program
                .consistency
                .iter()
                .map(|(_, grounded)| &grounded.rule),
        ),
    );
    let equal = equality
        .as_ref()
        .is_some_and(|e| store.estimate([None, Some(e.same_as), None], Seg::All) > 0);
    store.objects(&|p| equal || reads.by_object(p) || schema.read_in_grounding(p));
    let mut found = HashSet::new();
    check(
        &Current {
            store: &store,
            stale,
        },
        &program,
        0..program.consistency.len(),
        &[],
        &mut found,
    );
    result.violations = sorted(found);
    result.counters.lookups_without_order = store
        .relations
        .iter()
        .map(|relation| relation.missed.load(std::sync::atomic::Ordering::Relaxed))
        .sum();
    result.counters.merged_pairs = store.merged;
    phases.merge_parts[0] = store.news_time;
    for relation in &store.relations {
        for (total, part) in phases.merge_parts[1..]
            .iter_mut()
            .zip(relation.advance_parts)
        {
            *total += part;
        }
    }
    phases.consistency = clock.elapsed();
    result.phases = phases;
    heap::phase("reasoner: derived");
    let derived = match representatives {
        // Without classes, what was added (as `list` has it).
        None => Err(store),
        Some(reps) if reps.listing != Listing::Representatives && reps.classes.is_empty() => {
            result.merges = reps.merges;
            Err(store)
        }
        Some(reps) => {
            let derived = list(store, &reps);
            result.classes = reps.classes;
            result.merges = reps.merges;
            Ok(derived)
        }
    };
    match (derived, output) {
        (Err(store), Output::Relations) => result.relations = store.into_relations(),
        (Err(store), Output::Facts) => {
            result.derived = store.into_derived();
            result.derived.par_sort_unstable();
        }
        // A listing: as facts, made relations once they are ids again.
        (Ok(mut derived), _) => {
            derived.par_sort_unstable();
            result.derived = derived;
        }
    }
    heap::phase("reasoner: done");
    Ok(result)
}

/// What a materialisation with equality by representatives lists ([`Listing`]).
fn list<R: Representation>(store: WorkingStore<R>, reps: &Representatives) -> Vec<Triple> {
    let (stale, shared) = (&reps.stale, &reps.shared);
    let current = |fact: Triple| !stale.mentioned(fact);
    match reps.listing {
        // (Without classes, `run` takes what was added, not a listing.)
        Listing::Representatives => store.into_listed(&current, &current, Vec::new()),
        Listing::Stored => store.into_listed(
            &|fact| current(fact) && shared.mentioned(fact),
            &current,
            Vec::new(),
        ),
        Listing::Expanded => {
            // The facts that mention a class, expanded while the input is there to leave
            // its facts out; the others as they are.
            let expanded: Vec<Triple> = store
                .facts_where(&|fact| current(fact) && shared.mentioned(fact))
                .into_par_iter()
                .flat_map_iter(|fact| reps.classes.expand(fact))
                .filter(|&[s, p, o]| {
                    !store
                        .relation(p)
                        .is_some_and(|relation| relation.input.contains(s, o))
                })
                .collect();
            store.into_listed(
                &|_| false,
                &|fact| current(fact) && !shared.mentioned(fact),
                expanded,
            )
        }
    }
}

/// What ground rules read of the relations: which relations they can look up by object
/// alone, where the `(object, subject)` order is needed. An atom is looked up by object
/// alone if its object is a constant or a variable another atom binds, and its subject
/// isn't a constant; a variable predicate can be any relation. Every other lookup goes
/// by subject (both bound: a membership check by subject; nothing bound: a scan).
#[derive(Default)]
struct Reads {
    by_object: HashSet<u64>,
    everything: bool,
}

impl Reads {
    fn of<'a>(rules: impl Iterator<Item = &'a Rule>) -> Self {
        use super::ir::Term;
        let mut reads = Self::default();
        for rule in rules {
            for (i, atom) in rule.body.iter().enumerate() {
                let [subject, predicate, object] = atom.0;
                let bound = match object {
                    Term::Const(_) => true,
                    Term::Var(_) => rule
                        .body
                        .iter()
                        .enumerate()
                        .any(|(j, other)| j != i && other.0.contains(&object)),
                };
                if bound && !matches!(subject, Term::Const(_)) {
                    match predicate {
                        Term::Const(p) => {
                            reads.by_object.insert(p);
                        }
                        Term::Var(_) => reads.everything = true,
                    }
                }
            }
        }
        reads
    }

    fn by_object(&self, p: u64) -> bool {
        self.everything || self.by_object.contains(&p)
    }
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
