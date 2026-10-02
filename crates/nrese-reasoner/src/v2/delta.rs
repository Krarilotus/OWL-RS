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

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

use super::batch::{Equality, Store, check, is_replacement_rule, sorted};
use super::eval::{
    AllFacts, GroundProgram, Job, NEVER, RuleKey, Schema, Seg, Source, Stop, instantiate_head,
    rule_key, run_jobs, run_jobs_acyclic, transitive_predicate,
};
use super::ir::{Head, Rule};
use super::ir::{Triple, Violation};
use super::lists::{ListVocabulary, instantiate};

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
    /// The ground program over `source`; its bodiless facts count as handed out. List
    /// rules carry the list facts they come from as premises.
    fn program<S: Source + ?Sized>(&self, source: &S) -> GroundProgram {
        let mut program = GroundProgram::default();
        program.ground(source, self.schema, self.rules);
        if let Some(vocabulary) = self.lists {
            let (rules, premises, diagnostics) =
                super::lists::instantiate_with_premises(vocabulary, &AllFacts(source));
            program.ground_with_premises(source, self.schema, &rules, &premises);
            program.list_diagnostics = diagnostics;
        }
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

/// Whether commits without support counts keep candidates that a non-recursive rule still
/// derives in one step, before the proof search.
const NON_RECURSIVE_CHECK: bool = true;

/// What [`update`] computed.
#[derive(Default)]
pub struct Update {
    /// Facts to add to the inferred stack.
    pub insert: Vec<Triple>,
    /// Facts to remove from the inferred stack.
    pub remove: Vec<Triple>,
    /// Consistency violations that involve a fact the change added.
    pub violations: Vec<Violation>,
    /// List axioms the change left uninstantiable (malformed, cyclic, oversized) that
    /// weren't before.
    pub diagnostics: Vec<super::lists::ListDiagnostic>,
    pub rounds: usize,
    /// Time per phase: program, overdelete, rederive, insert, consistency.
    pub phases: [std::time::Duration; 5],
    /// The ground program of the state after the change, if it differs from the one given
    /// (or none was given); `None` means the given program still applies.
    pub program: Option<GroundProgram>,
    /// With support counts given ([`update_counted`]): the changes to them, to apply with
    /// [`super::supports::Supports::apply`].
    pub support_changes: Vec<(Triple, i64)>,
    /// The given counts don't apply to the state after the change (its ground program
    /// changed): count again ([`super::supports::Supports::count`]).
    pub supports_stale: bool,
}

impl std::fmt::Debug for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Update")
            .field("insert", &self.insert.len())
            .field("remove", &self.remove.len())
            .field("violations", &self.violations)
            .field("diagnostics", &self.diagnostics)
            .field("rounds", &self.rounds)
            .field("phases", &self.phases)
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
    match update_until(base, inserted, deleted, rules, cache, NEVER) {
        Ok(update) => update,
        Err(Interrupted) => unreachable!("NEVER doesn't stop"),
    }
}

/// [`update`] stopped by `stop` (the caller gave up, a deadline passed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupted;

/// [`update`], polling `stop` between rounds and per job morsel. When it fires, the work
/// so far is discarded: nothing has been applied, so the state stays as it was.
pub fn update_until<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    stop: Stop<'_>,
) -> Result<Update, Interrupted> {
    update_counted(base, inserted, deleted, rules, cache, None, stop)
}

/// [`update_until`] with the support counts of the state before the change
/// ([`super::supports`]): a candidate for overdeletion that keeps a non-recursive
/// derivation stays without a proof search (Hu, Motik and Horrocks), and the result
/// carries the counts' changes. The counts must be those of `cache`'s program (or of the
/// program built here, without a cache).
pub fn update_counted<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    supports: Option<&super::supports::Supports>,
    stop: Stop<'_>,
) -> Result<Update, Interrupted> {
    let check_stop = || if stop() { Err(Interrupted) } else { Ok(()) };
    let mut result = Update::default();
    let mut clock = std::time::Instant::now();
    let mut lap = |phase: usize, result: &mut Update| {
        result.phases[phase] += clock.elapsed();
        clock = std::time::Instant::now();
    };
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
    let same_as = super::batch::same_as_of(rules.rules);
    // Support counting: which ground rules are non-recursive, the instances lost and
    // gained. Off (the counts stale) once the program itself changes.
    // Without counts, a candidate is still kept when a non-recursive rule derives it in
    // one step from what is left (its premises are below it: if one goes later, its loss
    // brings the candidate back).
    let non_recursive: Vec<bool> = match supports.is_some() || NON_RECURSIVE_CHECK {
        true => {
            let before = Overlay {
                base,
                hidden: &inserted_set,
                extra: &Store::new(deleted.to_vec()),
            };
            super::supports::non_recursive(&old_program.rules, schema.rdf_type(), &|term| {
                super::supports::same_as_partners(&before, same_as, term)
            })
        }
        false => Vec::new(),
    };
    let mut counting = supports.is_some();
    // The lost instances counted so far (rule and bindings): overdeletion may find one
    // again in a later round, through another premise it overdeletes then.
    let mut lost: HashSet<(usize, Vec<Option<u64>>)> = HashSet::new();
    // `sameAs` between classes or properties links keys: a commit changing one leaves
    // the counts stale.
    let keys = match supports {
        Some(_) => super::supports::key_terms(&old_program.rules, schema.rdf_type()),
        None => HashSet::new(),
    };
    let key_same_as =
        |f: &Triple| same_as == Some(f[1]) && (keys.contains(&f[0]) || keys.contains(&f[2]));
    let mut changes: HashMap<Triple, i64> = HashMap::new();

    lap(0, &mut result);
    // 1. Overdelete, over the old state. With B/F, a candidate with a proof that avoids
    // the deleted facts is kept and doesn't propagate. B/F needs the old ground program to
    // stay valid: if a schema or list fact is affected by the change, overdeletion
    // restarts as plain DRed.
    let mut overdeleted: HashSet<Triple> = HashSet::new();
    // Transitive properties whose closure lost pairs: re-closed in phase 3.
    let mut recloses: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut backward = !deleted.iter().any(|&f| rules.is_program_fact(f));
    'overdelete: while !deleted.is_empty() {
        overdeleted.clear();
        recloses.clear();
        let prover = Prover {
            base,
            program: old_program,
            proved: HashSet::new(),
            failed: HashSet::new(),
            active: HashSet::new(),
            budget: 10_000,
        };
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
        let mut vanished_done: HashSet<RuleKey> = HashSet::new();
        loop {
            check_stop()?;
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
            // Transitivity whose declaration is deleted: every inferred pair may lose its
            // support.
            let mut component: Vec<Triple> = Vec::new();
            for &p in &scratch.transitive {
                old.scan([None, Some(p), None], Seg::All, &mut |t| component.push(t));
            }
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
                        Some(p) => {
                            old.scan([None, Some(p), None], Seg::All, &mut |t| component.push(t));
                        }
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
            let mut counted_jobs = Vec::new();
            for (r, i) in old_program.variants(&extra) {
                let job = Job::variant(&old, &old_program.rules[r], i);
                if counting && non_recursive[r] {
                    counted_jobs.extend(job.map(|job| (r, job)));
                } else {
                    jobs.extend(job);
                }
            }
            // Transitive properties, by component: a lost edge (a, b) may support every pair
            // from a predecessor of a (or a) to a successor of b (or b). The relation is
            // closed, so one hop finds them; the pairs are re-closed in phase 3.
            for &p in &old_program.transitive {
                let (mut sources, mut targets) = (Vec::new(), Vec::new());
                extra.scan([None, Some(p), None], Seg::Delta, &mut |t| {
                    sources.push(t[0]);
                    targets.push(t[2]);
                });
                if sources.is_empty() {
                    continue;
                }
                let newly = recloses.insert(p);
                let mut before: HashSet<u64> = sources.iter().copied().collect();
                for &a in &sources {
                    old.scan([None, Some(p), Some(a)], Seg::All, &mut |t| {
                        before.insert(t[0]);
                    });
                }
                let mut after: HashSet<u64> = targets.iter().copied().collect();
                for &b in &targets {
                    old.scan([Some(b), Some(p), None], Seg::All, &mut |t| {
                        after.insert(t[2]);
                    });
                }
                // With B/F (instances valid), pairs still connected by base edges keep a
                // proof: base edges are still-asserted facts and their one-atom images
                // (symmetry, inverses, subproperties) around the component; their closure
                // is computed by SCC condensation.
                let connected: HashSet<(u64, u64)> = if backward {
                    let mut edges = Vec::new();
                    for &u in before.iter().chain(&after) {
                        old.scan([Some(u), None, None], Seg::All, &mut |t| {
                            if extra.contains(t) || !base.is_asserted(t) {
                                return;
                            }
                            if t[1] == p {
                                edges.push((t[0], t[2]));
                            }
                            for image in old_program.one_step_images(t) {
                                if image[1] == p && !extra.contains(image) {
                                    edges.push((image[0], image[2]));
                                }
                            }
                        });
                    }
                    nrese_exec::graph::transitive_closure_until(&edges, stop)
                        .ok_or(Interrupted)?
                        .into_iter()
                        .collect()
                } else {
                    HashSet::new()
                };
                let before_len = component.len();
                for &x in &before {
                    old.scan([Some(x), Some(p), None], Seg::All, &mut |t| {
                        if after.contains(&t[2]) && !connected.contains(&(t[0], t[2])) {
                            component.push(t);
                        }
                    });
                }
                if newly && component.len() == before_len {
                    recloses.remove(&p);
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
            let mut candidates = run_jobs_acyclic(&old, &jobs, &overdeletable, stop);
            check_stop()?;
            // Every lost non-recursive instance, once, whatever its head: the counts stay
            // exact.
            let (rule_of, counted): (Vec<usize>, Vec<Job<'_>>) =
                std::mem::take(&mut counted_jobs).into_iter().unzip();
            for (j, bindings, facts) in super::eval::run_jobs_instances(&old, &counted, stop) {
                if !lost.insert((rule_of[j], bindings)) {
                    continue;
                }
                for fact in facts {
                    *changes.entry(fact).or_default() -= 1;
                    if overdeletable(fact) {
                        candidates.push(fact);
                    }
                }
            }
            check_stop()?;
            candidates.extend(bodiless.into_iter().filter(|&f| overdeletable(f)));
            candidates.extend(component.into_iter().filter(|&f| overdeletable(f)));
            drop(jobs);
            drop(counted_jobs);
            if !counting && backward && !non_recursive.is_empty() {
                let mut gone: HashSet<Triple> = inserted_set.clone();
                gone.extend(overdeleted.iter().copied());
                let left = Overlay {
                    base,
                    hidden: &gone,
                    extra: &empty,
                };
                candidates.sort_unstable();
                candidates.dedup();
                // Independent per candidate: in parallel.
                candidates = candidates
                    .into_par_iter()
                    .filter(|&f| stop() || !old_program.derivable_by(&left, f, &non_recursive))
                    .collect();
                check_stop()?;
            }
            if counting && let Some(supports) = supports {
                // A candidate with a non-recursive derivation left keeps it: no proof
                // search. If that derivation's premises go later in this commit, its loss
                // brings the candidate back.
                candidates.retain(|&f| {
                    i64::from(supports.of(f)) + changes.get(&f).copied().unwrap_or(0) <= 0
                });
            }
            if backward {
                // Proofs use ground instances whose schema facts are baked in: they are
                // sound only while no schema or list fact is affected by the change.
                if candidates.iter().any(|&f| rules.is_program_fact(f)) {
                    backward = false;
                    counting = false;
                    changes.clear();
                    lost.clear();
                    continue 'overdelete;
                }
                candidates.sort_unstable();
                candidates.dedup();
                // Each worker its own prover (its own caches and budget): candidates in
                // parallel.
                let fresh_prover = || Prover {
                    base,
                    program: old_program,
                    proved: HashSet::new(),
                    failed: HashSet::new(),
                    active: HashSet::new(),
                    budget: prover.budget,
                };
                candidates = candidates
                    .into_par_iter()
                    .map_init(fresh_prover, |prover, f| {
                        (stop() || !prover.prove(&old, &extra, f, 0).0).then_some(f)
                    })
                    .flatten()
                    .collect();
                check_stop()?;
            }
            let delta = extra.advance(candidates);
            if delta.is_empty() {
                break;
            }
            overdeleted.extend(delta);
        }
        break;
    }

    lap(1, &mut result);
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
        .filter(|&fact| {
            !stop()
                && !base.is_asserted(fact)
                && settled.derivable_with(&remaining, fact, !recloses.contains(&fact[1]))
        })
        .collect();
    check_stop()?;
    let settled_consistency = program.consistency.len();

    lap(2, &mut result);
    // 3. Insert: semi-naive from the rederived and inserted facts.
    let schema_rules = rules.schema_rules();
    let mut seeds = rederived;
    seeds.extend_from_slice(inserted);
    let mut extra = Store::new(seeds);
    let mut module_output: HashSet<Triple> = HashSet::new();
    // The equality module replaces eq-rep-s/p/o here, as in the batch executor.
    let mut equality = Equality::for_rules(rules.rules);
    let module_equality = equality.is_some();
    let replaced = |rule: &Rule| module_equality && is_replacement_rule(rule);
    let mut reclose = recloses;
    loop {
        check_stop()?;
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
        if delta_facts.iter().any(|&f| rules.is_list_fact(f))
            && let Some(vocabulary) = rules.lists
        {
            let (list_rules, premises, diagnostics) =
                super::lists::instantiate_with_premises(vocabulary, &AllFacts(&state));
            let program = program.to_mut();
            program.ground_with_premises(&state, schema, &list_rules, &premises);
            program.list_diagnostics = diagnostics;
        }
        let mut candidates = if program.has_pending_facts() {
            program.to_mut().take_facts()
        } else {
            Vec::new()
        };
        candidates.retain(|&f| !state.contains(f));
        if evaluated != program.rules.len() {
            counting = false;
        }
        let mut jobs = Vec::new();
        let mut counted_jobs = Vec::new();
        for (r, i) in program.variants(&extra) {
            if r < evaluated && !replaced(&program.rules[r]) {
                let job = Job::variant(&state, &program.rules[r], i);
                if counting && non_recursive[r] {
                    counted_jobs.extend(job);
                } else {
                    jobs.extend(job);
                }
            }
        }
        for rule in &program.rules[evaluated..] {
            if !replaced(rule) {
                jobs.extend(Job::full(&state, rule));
            }
        }
        candidates.extend(run_jobs(&state, &jobs, &|fact| !state.contains(fact), stop));
        check_stop()?;
        for fact in run_jobs(&state, &counted_jobs, &|_| true, stop) {
            *changes.entry(fact).or_default() += 1;
            if !state.contains(fact) {
                candidates.push(fact);
            }
        }
        check_stop()?;
        if let Some(equality) = &mut equality {
            candidates.extend(equality.run(&state));
        }
        drop(jobs);
        // Transitive properties: newly declared ones closed in full (SCC condensation), the
        // others per new edge. The module's own pairs keep the relation closed, so the
        // next round skips them; edges from other rules are closed then.
        let mut produced: Vec<Triple> = Vec::new();
        for &p in &program.transitive {
            if !transitive_before.contains(&p) || reclose.remove(&p) {
                let mut all = Vec::new();
                state.scan([None, Some(p), None], Seg::All, &mut |t| {
                    all.push((t[0], t[2]))
                });
                produced.extend(
                    nrese_exec::graph::transitive_closure_until(&all, stop)
                        .ok_or(Interrupted)?
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

    check_stop()?;
    lap(3, &mut result);
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
    check_stop()?;
    result.violations = sorted(found);
    lap(4, &mut result);
    result.diagnostics = program
        .list_diagnostics
        .iter()
        .filter(|d| !old_program.list_diagnostics.contains(d))
        .copied()
        .collect();
    let program_changed = matches!(program, Cow::Owned(_));
    result.program = match program {
        Cow::Owned(program) => Some(program),
        Cow::Borrowed(_) => None,
    }
    .or(computed);
    if supports.is_some() {
        let equality_changed = inserted
            .iter()
            .chain(deleted)
            .chain(&result.insert)
            .chain(&result.remove)
            .any(key_same_as);
        result.supports_stale = !counting || program_changed || equality_changed;
        if !result.supports_stale {
            result.support_changes = changes.into_iter().filter(|&(_, c)| c != 0).collect();
            result.support_changes.sort_unstable();
        }
    }
    Ok(result)
}

/// Backward proofs for B/F deletion (Motik et al., AAAI 2015): a fact is kept if it has a
/// derivation tree down to asserted facts (or bodiless program facts) that avoids the
/// deleted ones. Proofs are memoised; facts on the current path don't count (no circular
/// support), and a failure is memoised only if no cycle was cut beneath it. The search
/// has a node budget: when it runs out the fact counts as unproved, which only costs
/// overdeletion that the rederive phase repairs.
struct Prover<'a, B: Base + ?Sized> {
    base: &'a B,
    program: &'a GroundProgram,
    proved: HashSet<Triple>,
    failed: HashSet<Triple>,
    active: HashSet<Triple>,
    budget: usize,
}

/// Derivations tried per fact, and the proof depth.
const PROOF_BRANCHES: usize = 32;
const PROOF_DEPTH: usize = 64;

impl<B: Base + ?Sized> Prover<'_, B> {
    /// Whether `fact` is provable over `state` without the facts in `gone`; the second
    /// value says whether a cycle (or the budget) cut the search.
    fn prove<S: Source + ?Sized>(
        &mut self,
        state: &S,
        gone: &Store,
        fact: Triple,
        depth: usize,
    ) -> (bool, bool) {
        if self.proved.contains(&fact) {
            return (true, false);
        }
        if self.failed.contains(&fact) || gone.contains(fact) {
            return (false, false);
        }
        if self.base.is_asserted(fact) {
            self.proved.insert(fact);
            return (true, false);
        }
        // Transitive properties go through the component path of overdeletion instead (a
        // proof search through a k-clique is O(k^3)): never proved here, so it's a failure
        // that can be memoised.
        if self.program.transitive.contains(&fact[1]) {
            return (false, false);
        }
        if self.active.contains(&fact) || self.budget == 0 || depth > PROOF_DEPTH {
            return (false, true);
        }
        self.budget -= 1;
        self.active.insert(fact);
        let mut cut = false;
        let mut bodies = self.program.derivation_bodies(state, fact, PROOF_BRANCHES);
        if self.program.bodiless.contains(&fact) {
            bodies.insert(0, self.program.bodiless_premises(fact).to_vec());
        }
        for body in bodies {
            if body.iter().any(|&g| gone.contains(g)) {
                continue;
            }
            let mut holds = true;
            for g in body {
                let (proved, cycle) = self.prove(state, gone, g, depth + 1);
                cut |= cycle;
                if !proved {
                    holds = false;
                    break;
                }
            }
            if holds {
                self.active.remove(&fact);
                self.proved.insert(fact);
                return (true, false);
            }
        }
        self.active.remove(&fact);
        if !cut {
            self.failed.insert(fact);
        }
        (false, cut)
    }
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
