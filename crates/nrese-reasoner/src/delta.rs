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

pub mod policy;

use std::borrow::Cow;

use hashbrown::HashSet;
use rayon::prelude::*;

use std::sync::Arc;

use nrese_exec::Budget;

use super::batch::{Equality, Store, check, is_replacement_rule};
use super::eval::{
    AllFacts, GroundProgram, Job, NEVER, Poller, Probe, RuleKey, Schema, Seg, Source, Stop, Visit,
    Visits, instantiate_head, rule_key, run_jobs, run_jobs_acyclic, transitive_predicate,
};
use super::ir::Triple;
use super::ir::{Head, Rule};
use super::lists::{ListVocabulary, instantiate};
use super::violations::{Violations, unlimited};

/// The materialised state: asserted facts after the change, and inferred facts before it.
///
/// Every read takes the reading task's poller and counts on it as a [`Source`] read does
/// (the call, then each position it visits, the ones it filters out included); on a
/// stopped poller it does no more work and its answer means nothing. As for [`Source`], the
/// counting is the trait's: an implementation provides [`VisitsBase`], and `Base` is
/// implemented for every one of those and nothing else.
pub trait Base: Sync + sealed::Read {
    /// Calls `f` with every fact matching `pattern` (`None` = any term). A fact may be
    /// reported more than once (asserted in several graphs).
    fn scan(&self, pattern: [Option<u64>; 3], poll: &Poller<'_>, f: &mut dyn FnMut(Triple));
    /// An upper bound on the matches of `pattern`; zero only if nothing matches (or, on a
    /// stopped poller, nothing).
    fn estimate(&self, pattern: [Option<u64>; 3], poll: &Poller<'_>) -> usize;
    fn contains(&self, fact: Triple, poll: &Poller<'_>) -> bool;
    /// Whether `fact` is asserted (after the change).
    fn is_asserted(&self, fact: Triple, poll: &Poller<'_>) -> bool;
    /// Calls `f` with every asserted fact matching `pattern` (after the change), as
    /// [`Base::scan`] does: a base whose asserted facts are apart reads them alone.
    fn scan_asserted(
        &self,
        pattern: [Option<u64>; 3],
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    );
}

/// What a [`Base`] implementation provides: its reads, each counted by the caller's side
/// before it runs, its scans delivering through a [`Visit`] ([`Visits`] for sources).
pub trait VisitsBase: Sync {
    /// Hands every position matching `pattern` to `visit` ([`Visits::visit`]).
    fn visit(&self, pattern: [Option<u64>; 3], visit: &mut Visit<'_, '_>);
    /// [`Base::estimate`], the call already counted.
    fn size(&self, pattern: [Option<u64>; 3], probe: Probe<'_, '_>) -> usize;
    /// [`Base::contains`], the call already counted.
    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> bool;
    /// [`Base::is_asserted`], the call already counted.
    fn asserts(&self, fact: Triple, probe: Probe<'_, '_>) -> bool;
    /// [`Base::scan_asserted`]: by default the scan, each fact it delivers asked whether it
    /// is asserted (on the scan's poller).
    fn visit_asserted(&self, pattern: [Option<u64>; 3], visit: &mut Visit<'_, '_>) {
        let probe = Probe::of(visit.poller());
        visit.filtered(&|fact| self.asserts(fact, probe), |inner| {
            self.visit(pattern, inner)
        });
    }
}

mod sealed {
    /// Only [`super::VisitsBase`] implementations are [`super::Base`]s.
    pub trait Read {}
    impl<T: super::VisitsBase + ?Sized> Read for T {}
}

impl<T: VisitsBase + ?Sized> Base for T {
    #[inline]
    fn scan(&self, pattern: [Option<u64>; 3], poll: &Poller<'_>, f: &mut dyn FnMut(Triple)) {
        if !poll.tick() {
            self.visit(pattern, &mut Visit::new(poll, f));
        }
    }

    #[inline]
    fn estimate(&self, pattern: [Option<u64>; 3], poll: &Poller<'_>) -> usize {
        match poll.tick() {
            true => 0,
            false => self.size(pattern, Probe::of(poll)),
        }
    }

    #[inline]
    fn contains(&self, fact: Triple, poll: &Poller<'_>) -> bool {
        !poll.tick() && self.holds(fact, Probe::of(poll))
    }

    #[inline]
    fn is_asserted(&self, fact: Triple, poll: &Poller<'_>) -> bool {
        !poll.tick() && self.asserts(fact, Probe::of(poll))
    }

    fn scan_asserted(
        &self,
        pattern: [Option<u64>; 3],
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    ) {
        if !poll.tick() {
            self.visit_asserted(pattern, &mut Visit::new(poll, f));
        }
    }
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

/// What [`update`] computed.
#[derive(Default)]
pub struct Update {
    /// Facts to add to the inferred stack.
    pub insert: Vec<Triple>,
    /// Facts to remove from the inferred stack.
    pub remove: Vec<Triple>,
    /// Consistency violations that involve a fact the change added, charged to the
    /// update's budget.
    pub violations: Violations,
    /// List axioms the change left uninstantiable (malformed, cyclic, oversized) that
    /// weren't before.
    pub diagnostics: Vec<super::lists::ListDiagnostic>,
    pub rounds: usize,
    /// Time per phase: program, overdelete, rederive, insert, consistency.
    pub phases: [std::time::Duration; 5],
    /// The ground program of the state after the change, if it differs from the one given
    /// (or none was given); `None` means the given program still applies.
    pub program: Option<GroundProgram>,
    /// What the maintenance policy weighed and did ([`policy`]).
    pub policy: policy::Counts,
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
    fn in_base(&self, fact: Triple, poll: &Poller<'_>) -> bool {
        !self.hidden.contains(&fact) && !self.extra.contains(fact, poll)
    }
}

/// Reads count on the base's and the extra store's (each position they visit), and each
/// base fact's check against the overlay on the extra store's probe.
impl<B: Base + ?Sized> Visits for Overlay<'_, B> {
    fn visit(&self, pattern: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>) {
        let seg = seg.plain();
        self.extra.visit(pattern, seg, visit);
        if seg != Seg::Delta && !visit.stopped() {
            let poll = visit.poller();
            // The base's scan counts each position it visits on this visit's poller.
            self.base.scan(pattern, poll, &mut |fact| {
                if self.in_base(fact, poll) {
                    visit.counted(fact);
                }
            });
        }
    }

    fn size(&self, pattern: [Option<u64>; 3], seg: Seg, probe: Probe<'_, '_>) -> usize {
        let seg = seg.plain();
        let extra = self.extra.size(pattern, seg, probe);
        match seg {
            // The base's estimate saturates (`Snapshot::count_in`), so the sum does too.
            Seg::Old | Seg::All => {
                extra.saturating_add(self.base.estimate(pattern, probe.poller()))
            }
            _ => extra,
        }
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> bool {
        self.extra.holds(fact, probe)
            || (!self.hidden.contains(&fact) && self.base.contains(fact, probe.poller()))
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
        Err(_) => unreachable!("NEVER doesn't stop"),
    }
}

/// [`update`] stopped by `stop` (the caller gave up, a deadline passed), or by the
/// process's memory limit ([`super::eval::OperationStop`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Interrupted {
    /// Why the process's memory limit stopped it, if that is what stopped it: past the
    /// limit, or the memory unreadable.
    pub memory: Option<nrese_exec::memory::MemoryStopCause>,
}

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
    update_with(
        base,
        inserted,
        deleted,
        rules,
        cache,
        stop,
        policy::Policy::default(),
    )
}

/// [`update_until`] with the plans `policy` allows ([`policy::Policy`]).
pub fn update_with<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    stop: Stop<'_>,
    policy: policy::Policy,
) -> Result<Update, Interrupted> {
    update_within(
        base,
        inserted,
        deleted,
        rules,
        cache,
        stop,
        policy,
        &unlimited(),
    )
}

/// [`update_with`], its consistency violations charged to `memory` (the operation's own
/// part: a refusal ends their list, never the update, and keeps the change inconsistent).
#[allow(clippy::too_many_arguments)]
pub fn update_within<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    stop: Stop<'_>,
    policy: policy::Policy,
    memory: &Arc<Budget>,
) -> Result<Update, Interrupted> {
    // The process's memory limit stops it too, whatever the caller polls; once stopped it
    // stays stopped, so no morsel skipped is taken for done.
    let stop = super::eval::OperationStop::new(stop);
    update_in(base, inserted, deleted, rules, cache, &stop, policy, memory)
}

/// [`update_within`] under one operation's `stop`, whose memory limit and room its caller
/// chose ([`super::eval::OperationStop::within`]): replication's copies are admitted by
/// its room.
#[allow(clippy::too_many_arguments)]
pub fn update_in<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    stop: &super::eval::OperationStop<'_>,
    policy: policy::Policy,
    memory: &Arc<Budget>,
) -> Result<Update, Interrupted> {
    let update = update_body(base, inserted, deleted, rules, cache, stop, policy, memory);
    // A read on a stopped poller answers "nothing" (REAS-2): whatever the body decided
    // after the stop (a plan, a phase's facts) is discarded here, at one place, as much as
    // at each stage's own check.
    match update {
        Ok(update) if !stop.fired() => Ok(update),
        _ => Err(stop.interrupted()),
    }
}

/// [`update_with`]'s work, under the operation's latched stop, `operation`.
#[allow(clippy::too_many_arguments)]
fn update_body<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    deleted: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    operation: &super::eval::OperationStop<'_>,
    policy: policy::Policy,
    memory: &Arc<Budget>,
) -> Result<Update, Interrupted> {
    let stop: Stop<'_> = &|| operation.fired();
    let check_stop = || {
        if stop() {
            Err(Interrupted::default())
        } else {
            Ok(())
        }
    };
    let mut result = Update::default();
    if policy.rebuilds_at_once() {
        result.policy.plan = policy::Plan::Rebuild;
        return Ok(result);
    }
    // The update's sequential reads (the policy's, the component paths, the region's, the
    // job constructions, the transitive closure per edge) count on this; each stage asks
    // the latched stop before its output is used (REAS-2, sweep M02–M17).
    let stage = Poller::new(stop).ok_or_else(Interrupted::default)?;
    // The region the maintenance policy recomputes ([`policy::Plan::Recompute`]): the
    // terms whose facts are overdeleted at once and derived again. Deleted equalities
    // may split classes; the policy decides from the classes' size, before any other
    // work, as a rebuild needs none of it.
    let same_as = super::batch::same_as_of(rules.rules);
    let is_equality = |t: &Triple| Some(t[1]) == same_as && t[0] != t[2];
    let mut region: HashSet<u64> = HashSet::new();
    if let Some(same_as) = same_as {
        let equalities = deleted.iter().copied().filter(is_equality);
        let decision = policy::plan_for_equalities(
            base,
            same_as,
            equalities,
            policy,
            &mut result.policy,
            &stage,
        );
        check_stop()?;
        match decision {
            policy::Decision::Keep => {}
            policy::Decision::Recompute(terms) => region = terms,
            policy::Decision::Rebuild => {
                result.policy.plan = policy::Plan::Rebuild;
                return Ok(result);
            }
        }
    }
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
    // The ground rules that are non-recursive ([`super::recursion`]): a candidate for
    // overdeletion that one of them still derives in one step from what is left keeps
    // that derivation (its premises are below it: if one goes later in the commit, its
    // loss brings the candidate back), without a proof search. Bodiless instances aren't
    // among them: their schema facts may come from the candidate (scm-eqc1 and scm-eqc2
    // derive each other's).
    let non_recursive = {
        let before = Overlay {
            base,
            hidden: &inserted_set,
            extra: &Store::new(deleted.to_vec()),
        };
        super::recursion::non_recursive(&old_program.rules, schema.rdf_type(), &|term| {
            super::recursion::same_as_partners(&before, same_as, term, &stage)
        })
    };

    lap(0, &mut result);
    // 1. Overdelete, over the old state. With B/F, a candidate with a proof that avoids
    // the deleted facts is kept and doesn't propagate. B/F needs the old ground program to
    // stay valid: if a schema or list fact is affected by the change, overdeletion
    // restarts as plain DRed.
    let mut overdeleted: HashSet<Triple> = HashSet::new();
    // Transitive properties whose closure lost pairs: re-closed in phase 3.
    let mut recloses: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut backward = !deleted.iter().any(|&f| rules.is_program_fact(f));
    let mut region_facts: Vec<Triple> = Vec::new();
    let mut region_asserted: HashSet<Triple> = HashSet::new();
    'overdelete: while !deleted.is_empty() {
        overdeleted.clear();
        recloses.clear();
        // The region's facts, overdeleted at once: every inferred fact that mentions one
        // of its terms (the asserted ones are what phase 2 settles it from).
        region_asserted = asserted_mentioning(base, &region, &stage);
        region_facts = facts_mentioning(base, &region, &stage);
        region_facts.retain(|f| !region_asserted.contains(f) && !inserted_set.contains(f));
        if region_facts.iter().any(|&f| rules.is_program_fact(f)) {
            // Ground instances that bake those facts in are no proofs any more.
            backward = false;
        }
        overdeleted.extend(region_facts.iter().copied());
        let mut seed = deleted.to_vec();
        seed.extend(region_facts.iter().copied());
        let mut extra = Store::new(seed);
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
                old.scan([None, Some(p), None], Seg::All, &stage, &mut |t| {
                    component.push(t)
                });
            }
            let mut bodiless = scratch.take_facts();
            // List rules whose list lost a fact: in full as well.
            let mut delta_facts = Vec::new();
            extra.scan([None, None, None], Seg::Delta, &stage, &mut |f| {
                delta_facts.push(f)
            });
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
                            old.scan([None, Some(p), None], Seg::All, &stage, &mut |t| {
                                component.push(t)
                            });
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
            // The equality rules read the delta without the region's facts: what they
            // derive from them mentions the region's terms too, so it is overdeleted
            // already (and running them over a class costs a copy per pair of members).
            let outside = Outside {
                source: &old,
                region: &region,
            };
            let (mut jobs, mut equality_jobs) = (Vec::new(), Vec::new());
            for (r, i) in old_program.variants(&extra, &stage) {
                let rule = &old_program.rules[r];
                if !region.is_empty() && is_equality_rule(rule) {
                    equality_jobs.extend(Job::variant(&outside, rule, i, &stage));
                } else {
                    jobs.extend(Job::variant(&old, rule, i, &stage));
                }
            }
            // Transitive properties, by component: a lost edge (a, b) may support every pair
            // from a predecessor of a (or a) to a successor of b (or b). The relation is
            // closed, so one hop finds them; the pairs are re-closed in phase 3. Each scan
            // per endpoint and each probe per fact it visits counts on the stage's poller
            // (sweep M06, M07).
            for &p in &old_program.transitive {
                let (mut sources, mut targets) = (Vec::new(), Vec::new());
                extra.scan([None, Some(p), None], Seg::Delta, &stage, &mut |t| {
                    // The region's equalities are all overdeleted already.
                    if Some(p) != same_as || !mentions(&region, t) {
                        sources.push(t[0]);
                        targets.push(t[2]);
                    }
                });
                if sources.is_empty() {
                    continue;
                }
                let newly = recloses.insert(p);
                let mut before: HashSet<u64> = sources.iter().copied().collect();
                for &a in &sources {
                    old.scan([None, Some(p), Some(a)], Seg::All, &stage, &mut |t| {
                        before.insert(t[0]);
                    });
                }
                let mut after: HashSet<u64> = targets.iter().copied().collect();
                for &b in &targets {
                    old.scan([Some(b), Some(p), None], Seg::All, &stage, &mut |t| {
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
                        old.scan([Some(u), None, None], Seg::All, &stage, &mut |t| {
                            if extra.contains(t, &stage) || !base.is_asserted(t, &stage) {
                                return;
                            }
                            if t[1] == p {
                                edges.push((t[0], t[2]));
                            }
                            for image in old_program.one_step_images(t) {
                                if image[1] == p && !extra.contains(image, &stage) {
                                    edges.push((image[0], image[2]));
                                }
                            }
                        });
                    }
                    // A component read part-way is not closed: the stop ends it here.
                    check_stop()?;
                    nrese_exec::graph::transitive_closure_until(&edges, stop)
                        .ok_or_else(Interrupted::default)?
                        .into_iter()
                        .collect()
                } else {
                    HashSet::new()
                };
                let before_len = component.len();
                for &x in &before {
                    old.scan([Some(x), Some(p), None], Seg::All, &stage, &mut |t| {
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
                jobs.extend(Job::full(&old, rule, &stage));
            }
            // Only inferred facts of the old state are overdeleted; asserted ones stay.
            let overdeletable = |f: Triple, poll: &Poller<'_>| {
                base.contains(f, poll)
                    && !base.is_asserted(f, poll)
                    && !extra.contains(f, poll)
                    && !inserted_set.contains(&f)
            };
            let mut candidates = run_jobs_acyclic(&old, &jobs, &overdeletable, stop);
            candidates.extend(run_jobs_acyclic(
                &outside,
                &equality_jobs,
                &overdeletable,
                stop,
            ));
            check_stop()?;
            candidates.extend(bodiless.into_iter().filter(|&f| overdeletable(f, &stage)));
            candidates.extend(component.into_iter().filter(|&f| overdeletable(f, &stage)));
            candidates.retain(|&f| !mentions(&region, f));
            drop(jobs);
            // Equalities about to be overdeleted (their premises gone) may split classes
            // too: the policy may recompute them, from the start.
            if let Some(same_as) = same_as {
                let equalities = candidates
                    .iter()
                    .copied()
                    .filter(is_equality)
                    .filter(|&t| !mentions(&region, t));
                let decision = policy::plan_for_equalities(
                    base,
                    same_as,
                    equalities,
                    policy,
                    &mut result.policy,
                    &stage,
                );
                // The probes above and the policy's reads: their answers mean something
                // only if the stop hasn't fired.
                check_stop()?;
                match decision {
                    policy::Decision::Keep => {}
                    policy::Decision::Recompute(terms) => {
                        region.extend(terms);
                        continue 'overdelete;
                    }
                    policy::Decision::Rebuild => {
                        result = Update {
                            policy: result.policy,
                            ..Update::default()
                        };
                        result.policy.plan = policy::Plan::Rebuild;
                        return Ok(result);
                    }
                }
            }
            if backward {
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
                    .filter(|&f| !old_program.derivable_by(&left, f, &non_recursive, false, stop))
                    .collect();
                check_stop()?;
            }
            if backward {
                // Proofs use ground instances whose schema facts are baked in: they are
                // sound only while no schema or list fact is affected by the change.
                if candidates.iter().any(|&f| rules.is_program_fact(f)) {
                    backward = false;
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
                    budget: PROOF_BUDGET,
                    poll: super::eval::Poller::new(stop),
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

    // A recomputed region's pairs of a transitive property (other than `owl:sameAs`, whose
    // closure phase 2 settles) were overdeleted at once, around the component path: their
    // closure is computed again in phase 3.
    for f in &region_facts {
        if Some(f[1]) != same_as && old_program.transitive.contains(&f[1]) {
            recloses.insert(f[1]);
        }
    }
    lap(1, &mut result);
    // 2. Rederive: overdeleted and deleted facts with a derivation from what remains. The
    // program is that of the settled state, without the inserted facts: rule instances
    // that exist because of them are then new in phase 3 and evaluated in full.
    let mut hidden: HashSet<Triple> = overdeleted.clone();
    hidden.extend(deleted.iter().copied());
    let mut unsettled = hidden.clone();
    unsettled.extend(inserted.iter().copied());
    let program_changed = deleted
        .iter()
        .chain(&overdeleted)
        .any(|&f| rules.is_program_fact(f));
    let settle = |unsettled: &HashSet<Triple>| -> Cow<'_, GroundProgram> {
        if program_changed {
            Cow::Owned(rules.program(&Overlay {
                base,
                hidden: unsettled,
                extra: &empty,
            }))
        } else {
            Cow::Borrowed(old_program)
        }
    };
    let mut program: Cow<'_, GroundProgram> = settle(&unsettled);
    // A recomputed region first, in bulk ([`policy::Plan::Recompute`]): its facts that
    // the other rules derive in one step from what remains, and every copy of a remaining
    // fact within the classes the remaining equalities make (what the equality rules
    // derive, all at once), until nothing more comes back. What doesn't can only come
    // back in phase 3. Its facts don't seed phase 3: their copies are back already.
    let mut put_back: HashSet<Triple> = HashSet::new();
    let mut rederived: Vec<Triple> = Vec::new();
    if !region.is_empty() {
        // Overdeleted facts only: a deleted one is no longer in the state, so it comes
        // back as a seed of phase 3 (below), not by staying.
        let mut open: Vec<Triple> = region_facts.clone();
        let mut sources: Vec<Triple> = region_asserted
            .iter()
            .copied()
            .filter(|f| !inserted_set.contains(f))
            .collect();
        loop {
            check_stop()?;
            let remaining = Overlay {
                base,
                hidden: &unsettled,
                extra: &empty,
            };
            let not_equality: Vec<bool> = program
                .rules
                .iter()
                .map(|rule| !is_equality_rule(rule))
                .collect();
            // Bodiless instances too: their schema facts are what remains, every open fact
            // hidden.
            let by_rules: Vec<Triple> = open
                .par_iter()
                .copied()
                .filter(|&f| program.derivable_by(&remaining, f, &not_equality, true, stop))
                .collect();
            let by_rules: HashSet<Triple> = by_rules.into_iter().collect();
            check_stop()?;
            // The classes of the remaining equalities: the region's, and those of the
            // other terms its facts mention (a fact of a member may be copied through
            // the class of its predicate too).
            let mut classes = nrese_exec::classes::Classes::default();
            for f in sources.iter().chain(&by_rules).filter(|f| is_equality(f)) {
                classes.union(f[0], f[2]);
            }
            if let Some(same_as) = same_as {
                let mut others: HashSet<u64> = HashSet::new();
                for f in sources.iter().chain(&by_rules).chain(&open) {
                    others.extend(f.iter().copied().filter(|t| !region.contains(t)));
                }
                // A scan per term its facts mention, on the stage's poller (sweep M08).
                for term in others {
                    let pattern = [Some(term), Some(same_as), None];
                    remaining.scan(pattern, Seg::All, &stage, &mut |f| {
                        if f[0] != f[2] {
                            classes.union(f[0], f[2]);
                        }
                    });
                }
                // Classes read part-way decide nothing (and settling again grounds).
                check_stop()?;
            }
            let keys: HashSet<Triple> = sources
                .iter()
                .chain(&by_rules)
                .map(|&f| classes.rewrite(f))
                .collect();
            let (back, still): (Vec<Triple>, Vec<Triple>) = open
                .into_iter()
                .partition(|f| by_rules.contains(f) || keys.contains(&classes.rewrite(*f)));
            open = still;
            if back.is_empty() {
                break;
            }
            for f in &back {
                unsettled.remove(f);
                hidden.remove(f);
            }
            if program_changed && back.iter().any(|&f| rules.is_program_fact(f)) {
                program = settle(&unsettled);
            }
            rederived.extend(by_rules);
            put_back.extend(back.iter().copied());
            sources.extend(back);
        }
    }
    // The rest: overdeleted and deleted facts with a one-step derivation from what
    // remains.
    let remaining = Overlay {
        base,
        hidden: &unsettled,
        extra: &empty,
    };
    let candidates: Vec<Triple> = overdeleted
        .iter()
        .copied()
        .filter(|f| !mentions(&region, *f))
        .chain(deleted.iter().copied())
        .collect();
    let settled: &GroundProgram = &program;
    // Each task's probe of a candidate counts on its own poller, before the search makes
    // one of its own.
    let others: Vec<Triple> = candidates
        .par_iter()
        .copied()
        .map_init(
            || Poller::new(stop),
            |poll, fact| {
                let poll = poll.as_ref()?;
                (!base.is_asserted(fact, poll)
                    && settled.derivable_with(&remaining, fact, !recloses.contains(&fact[1]), stop))
                .then_some(fact)
            },
        )
        .flatten()
        .collect();
    rederived.extend(others);
    check_stop()?;
    let settled_consistency = program.consistency.len();

    lap(2, &mut result);
    // 3. Insert: semi-naive from the rederived and inserted facts.
    let schema_rules = rules.schema_rules();
    result.policy.rederived = (rederived.len() + put_back.len()) as u64;
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
        extra.scan([None, None, None], Seg::Delta, &stage, &mut |f| {
            delta_facts.push(f)
        });
        // The program grows from these: only from a delta read whole.
        check_stop()?;
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
        candidates.retain(|&f| !state.contains(f, &stage));
        let mut jobs = Vec::new();
        for (r, i) in program.variants(&extra, &stage) {
            if r < evaluated && !replaced(&program.rules[r]) {
                jobs.extend(Job::variant(&state, &program.rules[r], i, &stage));
            }
        }
        for rule in &program.rules[evaluated..] {
            if !replaced(rule) {
                jobs.extend(Job::full(&state, rule, &stage));
            }
        }
        let keep = |fact: Triple, poll: &Poller<'_>| !state.contains(fact, poll);
        candidates.extend(run_jobs(&state, &jobs, &keep, stop));
        check_stop()?;
        if let Some(equality) = &mut equality {
            candidates.extend(equality.run(&state, operation)?);
        }
        drop(jobs);
        // Transitive properties: newly declared ones closed in full (SCC condensation), the
        // others per new edge. The module's own pairs keep the relation closed, so the
        // next round skips them; edges from other rules are closed then.
        let mut produced: Vec<Triple> = Vec::new();
        for &p in &program.transitive {
            if !transitive_before.contains(&p) || reclose.remove(&p) {
                let mut all = Vec::new();
                state.scan([None, Some(p), None], Seg::All, &stage, &mut |t| {
                    all.push((t[0], t[2]))
                });
                produced.extend(
                    nrese_exec::graph::transitive_closure_until(&all, stop)
                        .ok_or_else(Interrupted::default)?
                        .into_iter()
                        .map(|(s, o)| [s, p, o])
                        .filter(|&f| !state.contains(f, &stage)),
                );
                continue;
            }
            let mut edges = Vec::new();
            state.scan([None, Some(p), None], Seg::Delta, &stage, &mut |t| {
                if !module_output.contains(&t) {
                    edges.push((t[0], t[2]));
                }
            });
            produced.extend(close_edges(&state, p, &edges, &stage));
        }
        // The closures' reads count on the stage's poller: a round's candidates are only
        // added from reads that weren't stopped.
        check_stop()?;
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
    extra.scan([None, None, None], Seg::All, &stage, &mut |f| added.push(f));
    added.sort_unstable();
    added.dedup();
    // Two probes per added fact, on the stage's poller; the consistency phase's check and
    // the update's last one ask the latched stop before any of it is returned.
    let visible_before =
        |f: Triple| base.contains(f, &stage) && !hidden.contains(&f) && !inserted_set.contains(&f);
    result.insert = added
        .iter()
        .copied()
        .filter(|&f| {
            !base.is_asserted(f, &stage) && !(visible_before(f) || overdeleted.contains(&f))
        })
        .collect();
    let kept: HashSet<Triple> = added.iter().copied().collect();
    result.remove = overdeleted
        .iter()
        .copied()
        .filter(|f| !kept.contains(f) && !put_back.contains(f))
        .collect();
    result.remove.sort_unstable();
    result.policy.plan = if region.is_empty() {
        policy::Plan::Dred
    } else {
        policy::Plan::Recompute
    };
    result.policy.region_terms = region.len() as u64;
    result.policy.region_facts = region_facts.len() as u64;
    result.policy.overdeleted = overdeleted.len() as u64;

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
        .consistency_variants(&fresh, &stage)
        .into_iter()
        .filter(|&(c, _)| c < settled_consistency)
        .collect();
    let mut found = Violations::new(Arc::clone(memory));
    check(
        &state,
        &program,
        settled_consistency..program.consistency.len(),
        &variants,
        &mut found,
        stop,
    )?;
    check_stop()?;
    result.violations = found;
    lap(4, &mut result);
    result.diagnostics = program
        .list_diagnostics
        .iter()
        .filter(|d| !old_program.list_diagnostics.contains(d))
        .copied()
        .collect();
    result.program = match program {
        Cow::Owned(program) => Some(program),
        Cow::Borrowed(_) => None,
    }
    .or(computed);
    Ok(result)
}

/// The consistency violations a change that only inserts introduces, given the facts it
/// adds: `inserted` (asserted, new to the state) and `derived` (inferred, not visible
/// before). [`update_with`]'s own last phase without its insert rounds, for a caller that
/// derived the new facts in bulk (a first load, [`crate::engine::maintain`]): the same
/// report, through the new facts only, with the ground program extended by the instances of
/// new schema and list facts as phase 3 extends it; charged to `memory` as the delta's own
/// is ([`update_within`]), and stopped as the operation it is part of is (`operation`: its
/// latch and its cause, shared with the rest of the maintenance, [`update_in`]).
pub fn introduced_violations<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    derived: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    operation: &super::eval::OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<Violations, Interrupted> {
    introduced_violations_body(
        base,
        inserted,
        derived,
        rules,
        cache,
        &|| operation.fired(),
        memory,
    )
    .map_err(|_| operation.interrupted())
}

fn introduced_violations_body<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    derived: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    stop: Stop<'_>,
    memory: &Arc<Budget>,
) -> Result<Violations, Interrupted> {
    let inserted_set: HashSet<Triple> = inserted.iter().copied().collect();
    let empty = Store::default();
    let computed: Option<GroundProgram> = cache.is_none().then(|| {
        rules.program(&Overlay {
            base,
            hidden: &inserted_set,
            extra: &empty,
        })
    });
    let old_program: &GroundProgram = cache.or(computed.as_ref()).expect("one of them");
    let settled_consistency = old_program.consistency.len();
    let mut new_facts: Vec<Triple> = inserted.iter().chain(derived).copied().collect();
    new_facts.sort_unstable();
    new_facts.dedup();
    let mut fresh = Store::new(Vec::new());
    fresh.advance(new_facts);
    let hidden = HashSet::new();
    let state = Overlay {
        base,
        hidden: &hidden,
        extra: &fresh,
    };
    let mut program: Cow<'_, GroundProgram> = Cow::Borrowed(old_program);
    let stage = Poller::new(stop).ok_or_else(Interrupted::default)?;
    let mut delta_facts = Vec::new();
    fresh.scan([None, None, None], Seg::Delta, &stage, &mut |f| {
        delta_facts.push(f)
    });
    if stop() {
        return Err(Interrupted::default());
    }
    if delta_facts.iter().any(|&f| rules.schema.is_schema_fact(f)) {
        program
            .to_mut()
            .ground_delta(&state, rules.schema, &rules.schema_rules());
    }
    if delta_facts.iter().any(|&f| rules.is_list_fact(f))
        && let Some(vocabulary) = rules.lists
    {
        let (list_rules, premises, diagnostics) =
            super::lists::instantiate_with_premises(vocabulary, &AllFacts(&state));
        let program = program.to_mut();
        program.ground_with_premises(&state, rules.schema, &list_rules, &premises);
        program.list_diagnostics = diagnostics;
    }
    let variants: Vec<(usize, usize)> = program
        .consistency_variants(&fresh, &stage)
        .into_iter()
        .filter(|&(c, _)| c < settled_consistency)
        .collect();
    let mut found = Violations::new(Arc::clone(memory));
    // It asks the latched stop again before its report is made: the variants above too.
    check(
        &state,
        &program,
        settled_consistency..program.consistency.len(),
        &variants,
        &mut found,
        stop,
    )?;
    Ok(found)
}

/// The facts of `base` that mention one of `terms`, in any position: each through its
/// first position that holds one of them, so once (more often only if the base reports it
/// more often, asserted in several graphs; every use takes it as a set).
fn facts_mentioning<B: Base + ?Sized>(
    base: &B,
    terms: &HashSet<u64>,
    poll: &Poller<'_>,
) -> Vec<Triple> {
    let mut facts: Vec<Triple> = Vec::new();
    for &term in terms {
        for position in 0..3 {
            let mut pattern = [None; 3];
            pattern[position] = Some(term);
            base.scan(pattern, poll, &mut |t| {
                if !t[..position].iter().any(|earlier| terms.contains(earlier)) {
                    facts.push(t);
                }
            });
        }
    }
    facts
}

/// The asserted facts of `base` that mention one of `terms`.
fn asserted_mentioning<B: Base + ?Sized>(
    base: &B,
    terms: &HashSet<u64>,
    poll: &Poller<'_>,
) -> HashSet<Triple> {
    let mut facts = HashSet::new();
    for &term in terms {
        for pattern in [
            [Some(term), None, None],
            [None, Some(term), None],
            [None, None, Some(term)],
        ] {
            base.scan_asserted(pattern, poll, &mut |t| {
                facts.insert(t);
            });
        }
    }
    facts
}

/// Whether `fact` mentions one of `terms`.
fn mentions(terms: &HashSet<u64>, fact: Triple) -> bool {
    !terms.is_empty() && fact.iter().any(|term| terms.contains(term))
}

/// The equality rules, which a recomputed region leaves out of overdeletion and
/// rederivation ([`policy::Plan::Recompute`]).
fn is_equality_rule(rule: &Rule) -> bool {
    matches!(
        rule.name.as_str(),
        "eq-rep-s" | "eq-rep-p" | "eq-rep-o" | "eq-sym" | "eq-trans"
    )
}

/// A source whose delta leaves out the facts of the region being recomputed: what the
/// equality rules read during overdeletion.
struct Outside<'a, S: ?Sized> {
    source: &'a S,
    region: &'a HashSet<u64>,
}

/// Reads count on the inner source's.
impl<S: Source + ?Sized> Visits for Outside<'_, S> {
    fn visit(&self, pattern: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>) {
        // The inner scan counts each position it visits on this visit's poller.
        let delta = seg.plain() == Seg::Delta;
        self.source.scan(pattern, seg, visit.poller(), &mut |fact| {
            if !(delta && mentions(self.region, fact)) {
                visit.counted(fact);
            }
        });
    }

    fn size(&self, pattern: [Option<u64>; 3], seg: Seg, probe: Probe<'_, '_>) -> usize {
        self.source.estimate(pattern, seg, probe.poller())
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> bool {
        self.source.contains(fact, probe.poller())
    }
}

/// Backward proofs for B/F deletion (Motik et al., AAAI 2015): a fact is kept if it has a
/// derivation tree down to asserted facts (or bodiless program facts) that avoids the
/// deleted ones. Proofs are memoised; facts on the current path don't count (no circular
/// support), and a failure is memoised only if no cycle was cut beneath it. The search
/// has a node budget: when it runs out the fact counts as unproved, which only costs
/// overdeletion that the rederive phase repairs.
struct Prover<'a, 'p, B: Base + ?Sized> {
    base: &'a B,
    program: &'a GroundProgram,
    proved: HashSet<Triple>,
    failed: HashSet<Triple>,
    active: HashSet<Triple>,
    budget: usize,
    /// The update's stop, one poller per parallel task: each alternative, producer, search
    /// and scanned fact of a proof is attempted work (sol sweep M09). `None`: stopped before
    /// the task began.
    poll: Option<super::eval::Poller<'p>>,
}

/// Proof steps a prover takes at most before it gives up (the candidate is then overdeleted
/// and rederived if it holds).
const PROOF_BUDGET: usize = 10_000;

/// Derivations tried per fact, and the proof depth.
const PROOF_BRANCHES: usize = 32;
const PROOF_DEPTH: usize = 64;

impl<B: Base + ?Sized> Prover<'_, '_, B> {
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
        // The probes below count on the task's poller too (REAS-2): stopped, they answer
        // "not there", which proves nothing, and the stop cuts the search (not memoised).
        let Some(poll) = self.poll.as_ref() else {
            return (false, true);
        };
        if self.failed.contains(&fact) || gone.contains(fact, poll) {
            return (false, false);
        }
        if self.base.is_asserted(fact, poll) {
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
        let bodies = self
            .program
            .derivation_bodies(state, fact, PROOF_BRANCHES, poll);
        // Stopped: the bodies are incomplete, so a failure here decides nothing (not kept).
        let mut cut = poll.stopped();
        self.budget -= 1;
        self.active.insert(fact);
        for body in bodies {
            let Some(poll) = self.poll.as_ref() else {
                break;
            };
            if body.iter().any(|&g| gone.contains(g, poll)) {
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
/// pairs added so far, which keeps it closed. Returns the new pairs. Each scan, each pair
/// visited and each probe counts on `poll`; once that has stopped it returns what it has,
/// which is incomplete (sweep M11: a predecessor × successor product per edge).
fn close_edges<S: Source + ?Sized>(
    state: &S,
    p: u64,
    edges: &[(u64, u64)],
    poll: &Poller<'_>,
) -> Vec<Triple> {
    use hashbrown::HashMap;
    let mut added: HashSet<(u64, u64)> = HashSet::new();
    let mut successors: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut predecessors: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut out = Vec::new();
    for &(a, b) in edges {
        let mut before = vec![a];
        state.scan([None, Some(p), Some(a)], Seg::All, poll, &mut |t| {
            before.push(t[0])
        });
        before.extend(predecessors.get(&a).into_iter().flatten().copied());
        let mut after = vec![b];
        state.scan([Some(b), Some(p), None], Seg::All, poll, &mut |t| {
            after.push(t[2])
        });
        after.extend(successors.get(&b).into_iter().flatten().copied());
        before.sort_unstable();
        before.dedup();
        after.sort_unstable();
        after.dedup();
        for &x in &before {
            for &y in &after {
                if poll.tick() {
                    return out;
                }
                let fact = [x, p, y];
                if !added.contains(&(x, y)) && !state.contains(fact, poll) && !poll.stopped() {
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

/// Reads count on the store's ([`Source for Store`](Store)); a probe of the asserted set is
/// one unit.
impl VisitsBase for MemoryBase {
    fn visit(&self, pattern: [Option<u64>; 3], visit: &mut Visit<'_, '_>) {
        self.facts.visit(pattern, Seg::All, visit);
    }

    fn size(&self, pattern: [Option<u64>; 3], probe: Probe<'_, '_>) -> usize {
        self.facts.size(pattern, Seg::All, probe)
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> bool {
        self.facts.holds(fact, probe)
    }

    fn asserts(&self, fact: Triple, _: Probe<'_, '_>) -> bool {
        self.asserted.contains(&fact)
    }
}
