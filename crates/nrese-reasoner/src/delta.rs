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
//!    ([`super::eval::ground_delta`]), including list rules that read it as an operand.
//!    Those instances and rules whose list lost a structural fact are evaluated in full.
//! 2. **Rederive.** Each overdeleted fact (and each deleted asserted fact) that still has a
//!    one-step derivation from the remaining facts is put back
//!    ([`GroundProgram::derivable_with`], which finds the candidate rules by head).
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
use std::cell::OnceCell;

use hashbrown::HashSet;
use rayon::prelude::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nrese_exec::Budget;

use super::batch::{Equality, Store, check, is_replacement};
use super::eval::{
    AllFacts, GroundProgram, Job, ListRuleHeads, NEVER, Poller, Probe, RuleKey, ScannedFacts,
    Schema, Seg, Source, Stop, Stopped, Visit, Visits, WitnessIndex, instantiate_head, rule_key,
    run_jobs, run_jobs_acyclic, transitive_predicate,
};
use super::ir::Triple;
use super::ir::{Head, Rule};
use super::lists::{
    ListDiagnostic, ListRules, ListVocabulary, Predicates, RuleKind, WrittenLists, internal,
};
use super::violations::{Violations, unlimited};

/// The materialised state: asserted facts after the change, and inferred facts before it.
///
/// Every read takes the reading task's poller and counts on it as a [`Source`] read does
/// (the call, then each position it visits, the ones it filters out included); on a
/// stopped poller it does no more work and answers [`Stopped`], never a value.
/// As for [`Source`], the counting is the trait's: an implementation
/// provides [`VisitsBase`], and `Base` is implemented for every one of those and nothing
/// else.
pub trait Base: Sync + sealed::Read {
    /// Calls `f` with every fact matching `pattern` (`None` = any term). A fact may be
    /// reported more than once (asserted in several graphs). `Err` if its poller stopped it.
    fn scan(
        &self,
        pattern: [Option<u64>; 3],
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    ) -> Result<(), Stopped>;
    /// An upper bound on the matches of `pattern`; zero only if nothing matches.
    fn estimate(&self, pattern: [Option<u64>; 3], poll: &Poller<'_>) -> Result<usize, Stopped>;
    /// [`Base::estimate`], asked only up to `limit` ([`crate::eval::Source::estimate_up_to`]).
    fn estimate_up_to(
        &self,
        pattern: [Option<u64>; 3],
        limit: usize,
        poll: &Poller<'_>,
    ) -> Result<usize, Stopped>;
    fn contains(&self, fact: Triple, poll: &Poller<'_>) -> Result<bool, Stopped>;
    /// Whether `fact` is asserted (after the change).
    fn is_asserted(&self, fact: Triple, poll: &Poller<'_>) -> Result<bool, Stopped>;
    /// Calls `f` with every asserted fact matching `pattern` (after the change), as
    /// [`Base::scan`] does: a base whose asserted facts are apart reads them alone.
    fn scan_asserted(
        &self,
        pattern: [Option<u64>; 3],
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    ) -> Result<(), Stopped>;
}

/// What a [`Base`] implementation provides: its reads, each counted by the caller's side
/// before it runs, its scans delivering through a [`Visit`] ([`Visits`] for sources). Its
/// answers are `Err` once their probe's poller has stopped.
pub trait VisitsBase: Sync {
    /// Hands every position matching `pattern` to `visit` ([`Visits::visit`]).
    fn visit(&self, pattern: [Option<u64>; 3], visit: &mut Visit<'_, '_>);
    /// [`Base::estimate`], the call already counted.
    fn size(&self, pattern: [Option<u64>; 3], probe: Probe<'_, '_>) -> Result<usize, Stopped>;
    /// [`Base::estimate_up_to`], the call already counted. By default the whole estimate.
    fn size_up_to(
        &self,
        pattern: [Option<u64>; 3],
        _limit: usize,
        probe: Probe<'_, '_>,
    ) -> Result<usize, Stopped> {
        self.size(pattern, probe)
    }
    /// [`Base::contains`], the call already counted.
    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped>;
    /// [`Base::is_asserted`], the call already counted.
    fn asserts(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped>;
    /// [`Base::scan_asserted`]: by default the scan, each fact it delivers asked whether it
    /// is asserted (on the scan's poller); a stopped ask ends the scan.
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
    fn scan(
        &self,
        pattern: [Option<u64>; 3],
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    ) -> Result<(), Stopped> {
        if poll.tick() {
            return Err(Stopped);
        }
        self.visit(pattern, &mut Visit::new(poll, f));
        poll.ended()
    }

    #[inline]
    fn estimate(&self, pattern: [Option<u64>; 3], poll: &Poller<'_>) -> Result<usize, Stopped> {
        if poll.tick() {
            return Err(Stopped);
        }
        self.size(pattern, Probe::of(poll))
    }

    #[inline]
    fn estimate_up_to(
        &self,
        pattern: [Option<u64>; 3],
        limit: usize,
        poll: &Poller<'_>,
    ) -> Result<usize, Stopped> {
        if poll.tick() {
            return Err(Stopped);
        }
        self.size_up_to(pattern, limit, Probe::of(poll))
    }

    #[inline]
    fn contains(&self, fact: Triple, poll: &Poller<'_>) -> Result<bool, Stopped> {
        if poll.tick() {
            return Err(Stopped);
        }
        self.holds(fact, Probe::of(poll))
    }

    #[inline]
    fn is_asserted(&self, fact: Triple, poll: &Poller<'_>) -> Result<bool, Stopped> {
        if poll.tick() {
            return Err(Stopped);
        }
        self.asserts(fact, Probe::of(poll))
    }

    fn scan_asserted(
        &self,
        pattern: [Option<u64>; 3],
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    ) -> Result<(), Stopped> {
        if poll.tick() {
            return Err(Stopped);
        }
        self.visit_asserted(pattern, &mut Visit::new(poll, f));
        poll.ended()
    }
}

/// The rules an update runs, compiled against the vocabulary.
#[derive(Clone, Copy)]
pub struct Rules<'a> {
    pub rules: &'a [Rule],
    pub lists: Option<&'a ListVocabulary>,
    pub schema: &'a Schema,
    /// The `owl:sameAs` id the rules hold the equality rules over, if they do, asked once
    /// as they are compiled ([`super::equality_rules::of`]).
    pub equality: Option<u64>,
}

impl Rules<'_> {
    /// The ground program over `source`; its bodiless facts count as handed out. List
    /// rules carry the list facts they come from as premises.
    fn program<B: Base + ?Sized>(
        &self,
        source: &Overlay<'_, B>,
        predicates: Predicates,
        poll: &Poller<'_>,
        restored: &[Triple],
    ) -> GroundProgram {
        let mut program = GroundProgram::default();
        program.list_predicates = predicates;
        program.ground(source, self.schema, self.rules);
        if let Some((rules, premises, diagnostics)) =
            self.read_lists(source, &mut program.list_predicates, poll, restored)
        {
            program.ground_lists(source, self.schema, rules, premises);
            program.list_diagnostics = diagnostics;
        }
        program.take_facts();
        program
    }

    /// The inference list rules instantiated over `source` for overdeletion.
    fn list_rules<S: Source + ?Sized>(
        &self,
        source: &S,
        mut predicates: Predicates,
        poll: &Poller<'_>,
    ) -> Vec<Rule> {
        #[cfg(test)]
        crate::witness::add(crate::witness::Site::ListRules, 1);
        self.lists
            .map(|vocabulary| {
                super::lists::compile(
                    vocabulary,
                    &AllFacts(source, poll),
                    &mut predicates,
                    RuleKind::Inference,
                    None,
                )
                .0
            })
            .unwrap_or_default()
    }

    /// The list rules over `source`, their premises, and the lists left out.
    fn read_lists<B: Base + ?Sized>(
        &self,
        source: &Overlay<'_, B>,
        predicates: &mut Predicates,
        poll: &Poller<'_>,
        restored: &[Triple],
    ) -> Option<ListRules> {
        let vocabulary = self.lists?;
        let written = ScannedFacts(
            |pattern: [Option<u64>; 3], found: &mut dyn FnMut(Triple)| {
                source.base.scan_asserted(pattern, poll, &mut |fact| {
                    if !source.hidden.contains(&fact) {
                        found(fact);
                    }
                })?;
                // The pending base has already deleted these assertions.
                for fact in poll.counted(restored.iter().copied()) {
                    if pattern
                        .iter()
                        .zip(fact)
                        .all(|(want, n)| want.is_none_or(|want| want == n))
                    {
                        found(fact);
                    }
                }
                poll.ended()
            },
            poll,
        );
        Some(super::lists::compile(
            vocabulary,
            &AllFacts(source, poll),
            predicates,
            RuleKind::All,
            Some(WrittenLists {
                vocabulary,
                facts: &written,
                projection: None,
            }),
        ))
    }

    /// A list edit reads the lists again. A schema edit with the same lists grounds
    /// just the affected templates, through the delta and with their original premises.
    fn extend_lists<B: Base + ?Sized>(
        &self,
        state: &Overlay<'_, B>,
        program: &mut Cow<'_, GroundProgram>,
        delta: &[Triple],
        schema_changed: bool,
        poll: &Poller<'_>,
    ) -> Result<bool, Stopped> {
        let list_changed = self.lists.is_some() && delta.iter().any(|&f| self.is_list_fact(f));
        if list_changed {
            let program = program.to_mut();
            let (rules, premises, diagnostics) = self
                .read_lists(state, &mut program.list_predicates, poll, &[])
                .expect("list vocabulary");
            poll.ended()?;
            program.ground_lists(state, self.schema, rules, premises);
            program.list_diagnostics = diagnostics;
        } else if schema_changed && let Some(operands) = program.list_operands.clone() {
            // The schema delta already made the program owned.
            operands.ground_delta(
                program.to_mut(),
                state,
                self.schema,
                state.extra,
                poll,
                ListRuleHeads::All,
            )?;
        }
        Ok(list_changed)
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
    /// Whether any list goes between having an accepting walk and having none.
    pub lists_read_otherwise: bool,
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
            .field("lists_read_otherwise", &self.lists_read_otherwise)
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
    table: Option<&'a Store>,
}

impl<B: Base + ?Sized> Overlay<'_, B> {
    fn in_base(&self, fact: Triple, poll: &Poller<'_>) -> Result<bool, Stopped> {
        Ok(!self.hidden.contains(&fact) && !self.extra.contains(fact, poll)?)
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
            if pattern[1].is_some_and(internal) {
                if let Some(table) = self.table {
                    visit.kept(
                        |fact| self.in_base(fact, poll) == Ok(true),
                        |inner| table.visit(pattern, Seg::All, inner),
                    );
                }
                return;
            }
            // The base's scan counts each position it visits on this visit's poller; a check
            // its poller stopped delivers nothing, and the scan ends at its next position.
            // Stopped, the read around this visit answers `Stopped` (the poller is latched).
            let scanned = self.base.scan(pattern, poll, &mut |fact| {
                if self.in_base(fact, poll) == Ok(true) {
                    visit.counted(fact);
                }
            });
            debug_assert!(scanned.is_ok() || poll.stopped());
        }
    }

    fn size(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<usize, Stopped> {
        self.size_up_to(pattern, seg, usize::MAX, probe)
    }

    /// The extra store's estimate (exact, in memory) and the base's asked up to `limit`.
    fn size_up_to(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        limit: usize,
        probe: Probe<'_, '_>,
    ) -> Result<usize, Stopped> {
        let seg = seg.plain();
        let extra = self.extra.size(pattern, seg, probe)?;
        if pattern[1].is_some_and(internal) {
            let held = match self.table.filter(|_| seg != Seg::Delta) {
                Some(table) => table.size(pattern, Seg::All, probe)?,
                None => 0,
            };
            return Ok(extra.saturating_add(held));
        }
        Ok(match seg {
            // The base's estimate saturates (`Snapshot::count_in`), so the sum does too.
            Seg::Old | Seg::All => {
                extra.saturating_add(self.base.estimate_up_to(pattern, limit, probe.poller())?)
            }
            _ => extra,
        })
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(self.extra.holds(fact, probe)?
            || (!self.hidden.contains(&fact)
                && if internal(fact[1]) {
                    table_contains(self.table, fact, probe.poller())?
                } else {
                    self.base.contains(fact, probe.poller())?
                }))
    }
}

/// The ground program of the materialised state `base`: the cache [`update`] takes.
pub fn program<B: Base + ?Sized>(base: &B, rules: Rules<'_>) -> GroundProgram {
    program_until(base, rules, NEVER).expect("unbounded grounding")
}

pub(crate) fn program_until<B: Base + ?Sized>(
    base: &B,
    rules: Rules<'_>,
    stop: Stop<'_>,
) -> Result<GroundProgram, Stopped> {
    let (hidden, extra) = (HashSet::new(), Store::default());
    let source = Overlay {
        base,
        hidden: &hidden,
        extra: &extra,
        table: None,
    };
    let stage = Poller::new(stop).ok_or(Stopped)?;
    let mut program = rules.program(&source, Predicates::default(), &stage, &[]);
    stage.ended()?;
    program.table = rebuild_table(&program, &source, stop)?;
    if stop() {
        return Err(Stopped);
    }
    Ok(program)
}

fn table_contains(table: Option<&Store>, fact: Triple, poll: &Poller<'_>) -> Result<bool, Stopped> {
    table.map_or(Ok(false), |table| table.contains(fact, poll))
}

/// Ground rules can read private relations by their reserved predicate. A
/// variable-predicate read over RDF facts never ranges over them.
pub(crate) struct TableSource<'a, S: ?Sized> {
    pub(crate) source: &'a S,
    pub(crate) table: Option<&'a Store>,
}

impl<S: Source + ?Sized> Visits for TableSource<'_, S> {
    fn visit(&self, pattern: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>) {
        if pattern[1].is_some_and(internal) {
            if let Some(table) = self.table {
                table.visit(pattern, seg, visit);
            }
        } else {
            let read = self.source.scan(pattern, seg, visit.poller(), &mut |fact| {
                visit.counted(fact)
            });
            debug_assert!(read.is_ok() || visit.stopped());
        }
    }

    fn size(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<usize, Stopped> {
        if pattern[1].is_some_and(internal) {
            self.table
                .map_or(Ok(0), |table| table.size(pattern, seg, probe))
        } else {
            self.source.estimate(pattern, seg, probe.poller())
        }
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        if internal(fact[1]) {
            table_contains(self.table, fact, probe.poller())
        } else {
            self.source.contains(fact, probe.poller())
        }
    }
}

fn rebuild_table<S: Source + ?Sized>(
    program: &GroundProgram,
    source: &S,
    stop: Stop<'_>,
) -> Result<Option<Arc<Store>>, Stopped> {
    if program.table_rules.is_empty() {
        return Ok(None);
    }
    let stage = Poller::new(stop).ok_or(Stopped)?;
    let mut table = Store::default();
    let mut initial = true;
    loop {
        let state = TableSource {
            source,
            table: Some(&table),
        };
        let mut jobs = Vec::new();
        if initial {
            for &rule in &program.table_rules {
                jobs.extend(Job::full(&state, &program.rules[rule], &stage)?);
            }
            initial = false;
        } else {
            for (rule, atom) in program.variants(&table, &stage)? {
                if program.table_rules.binary_search(&rule).is_ok() {
                    jobs.extend(Job::variant(&state, &program.rules[rule], atom, &stage)?);
                }
            }
        }
        let candidates = run_jobs(
            &state,
            &jobs,
            &|fact, poll| Ok(!state.contains(fact, poll)?),
            stop,
        )?;
        stage.ended()?;
        if table.advance(candidates).is_empty() {
            return Ok(Some(Arc::new(table)));
        }
    }
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

impl std::fmt::Display for Interrupted {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use nrese_exec::memory::MemoryStopCause;
        out.write_str(match self.memory {
            None => "stopped by its caller (a stop or a deadline)",
            Some(MemoryStopCause::OverLimit) => "stopped: past the process's memory limit",
            Some(MemoryStopCause::Unread) => "stopped: the process's memory couldn't be read",
        })
    }
}

impl std::error::Error for Interrupted {}

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
    // A read its poller stopped ends the update here, as the stage's
    // own check does.
    let stopped = |Stopped| Interrupted::default();
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
    let same_as = rules.equality;
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
        )
        .map_err(stopped)?;
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
    let computed: Option<GroundProgram> = cache
        .is_none()
        .then(|| {
            let extra = Store::new(deleted.to_vec());
            let before = Overlay {
                base,
                hidden: &inserted_set,
                extra: &extra,
                table: None,
            };
            let mut program = rules.program(&before, Predicates::default(), &stage, deleted);
            stage.ended()?;
            program.table = rebuild_table(&program, &before, stop)?;
            Ok(program)
        })
        .transpose()
        .map_err(stopped)?;
    let old_program: &GroundProgram = cache.or(computed.as_ref()).expect("one of them");
    let table = old_program.table.as_deref();
    let predicates = || old_program.list_predicates.clone();
    // The ground rules that are non-recursive ([`super::recursion`]): a candidate for
    // overdeletion that one of them still derives in one step from what is left keeps
    // that derivation (its premises are below it: if one goes later in the commit, its
    // loss brings the candidate back), without a proof search. Bodiless instances aren't
    // among them: their schema facts may come from the candidate (scm-eqc1 and scm-eqc2
    // derive each other's). Its size follows the schema, so it is built when a candidate
    // first asks for it: a commit that deletes nothing never does. It reads the state
    // before the commit, which nothing in the update changes.
    let mut non_recursive: Option<Vec<bool>> = None;
    let build_non_recursive = || {
        let before = Overlay {
            base,
            hidden: &inserted_set,
            extra: &Store::new(deleted.to_vec()),
            table,
        };
        // A partner read a stop cut short ends the update: the keys asked about are the
        // grounded rules', which grow with the schema.
        let ground = &old_program.rules;
        let equality = &old_program.equality;
        super::recursion::non_recursive(ground, equality, schema.rdf_type(), same_as, &|term| {
            super::recursion::same_as_partners(&before, same_as, term, &stage)
        })
        .map_err(stopped)
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
        let capacity = PROOF_BUDGET;
        #[cfg(test)]
        let capacity = budget_tests::capacity(capacity);
        let proof_budget = AtomicUsize::new(capacity);
        overdeleted.clear();
        recloses.clear();
        // The region's facts, overdeleted at once: every inferred fact that mentions one
        // of its terms (the asserted ones are what phase 2 settles it from).
        region_asserted = asserted_mentioning(base, &region, &stage).map_err(stopped)?;
        region_facts = facts_mentioning(base, &region, &stage).map_err(stopped)?;
        region_facts.retain(|f| !region_asserted.contains(f) && !inserted_set.contains(f));
        if region_facts.iter().any(|&f| rules.is_program_fact(f)) {
            // Ground instances that bake those facts in are no proofs any more.
            backward = false;
        }
        overdeleted.extend(region_facts.iter().copied());
        let mut extra = Store::new(deleted.iter().chain(&region_facts).copied().collect());
        let fact_schema_rules: Vec<Rule> = rules
            .schema_rules()
            .into_iter()
            .filter(|r| r.head != Head::Inconsistent)
            .collect();
        // The list rules of the pass's old state, read only when the delta holds a list
        // fact: built then, over the state the pass started from (its deleted and region
        // facts), as they follow the store's lists.
        let mut old_lists: Option<Vec<Rule>> = None;
        let mut vanished_done: HashSet<RuleKey> = HashSet::new();
        loop {
            check_stop()?;
            result.rounds += 1;
            let old = Overlay {
                base,
                hidden: &inserted_set,
                extra: &extra,
                table,
            };
            // Instances through a deleted schema fact, known or not, in full.
            let mut scratch = GroundProgram::default();
            scratch.ground_delta(&old, schema, &fact_schema_rules);
            if !backward && let Some(operands) = old_program.list_operands.as_deref() {
                operands
                    .ground_delta(
                        &mut scratch,
                        &old,
                        schema,
                        &extra,
                        &stage,
                        ListRuleHeads::FactsOnly,
                    )
                    .map_err(stopped)?;
            }
            let mut fresh: Vec<Rule> = scratch.rules.clone();
            // Transitivity whose declaration is deleted: every inferred pair may lose its
            // support.
            let mut component: Vec<Triple> = Vec::new();
            for &p in &scratch.transitive {
                let filter = scratch.transitive_filter(p);
                old.scan([None, Some(p), None], Seg::All, &stage, &mut |t| {
                    if filter.is_none_or(|f| f.keeps(t[0], t[2], &stage)) {
                        component.push(t);
                    }
                })
                .map_err(stopped)?;
            }
            let mut bodiless = scratch.take_facts();
            // List rules whose list lost a fact: in full as well.
            let mut delta_facts = Vec::new();
            extra
                .scan([None, None, None], Seg::Delta, &stage, &mut |f| {
                    delta_facts.push(f)
                })
                .map_err(stopped)?;
            if delta_facts.iter().any(|&f| rules.is_list_fact(f)) {
                let mut gone: HashSet<Triple> = overdeleted.clone();
                gone.extend(delta_facts.iter().copied());
                gone.extend(deleted.iter().copied());
                gone.extend(inserted.iter().copied());
                let now = Overlay {
                    base,
                    hidden: &gone,
                    extra: &empty,
                    table,
                };
                let current: HashSet<RuleKey> = rules
                    .list_rules(&now, predicates(), &stage)
                    .iter()
                    .map(rule_key)
                    .collect();
                // A stop asked during that extraction is seen before the next one.
                check_stop()?;
                let old_lists = old_lists.get_or_insert_with(|| {
                    let at_start =
                        Store::new(deleted.iter().chain(&region_facts).copied().collect());
                    let old = Overlay {
                        base,
                        hidden: &inserted_set,
                        extra: &at_start,
                        table,
                    };
                    rules
                        .list_rules(&old, predicates(), &stage)
                        .into_iter()
                        .filter(|r| r.head != Head::Inconsistent)
                        .collect()
                });
                // And one asked during the second before its rules are walked.
                check_stop()?;
                for rule in old_lists.iter() {
                    let key = rule_key(rule);
                    if current.contains(&key) || !vanished_done.insert(key) {
                        continue;
                    }
                    match transitive_predicate(rule) {
                        Some(p) => {
                            old.scan([None, Some(p), None], Seg::All, &stage, &mut |t| {
                                component.push(t)
                            })
                            .map_err(stopped)?;
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
            // A stopped read ends the job list.
            for (r, i) in old_program.variants(&extra, &stage).map_err(stopped)? {
                let rule = &old_program.rules[r];
                // An equality rule, over the `same_as` the rules hold them over.
                if !region.is_empty() && same_as.is_some() && old_program.equality[r].is_some() {
                    equality_jobs.extend(Job::variant(&outside, rule, i, &stage).map_err(stopped)?);
                } else {
                    jobs.extend(Job::variant(&old, rule, i, &stage).map_err(stopped)?);
                }
            }
            // Transitive properties, by component: a lost edge (a, b) may support every pair
            // from a predecessor of a (or a) to a successor of b (or b). The relation is
            // closed, so one hop finds them; the pairs are re-closed in phase 3. Each scan
            // per endpoint and each probe per fact it visits counts on the stage's poller
            // (sweep M06, M07).
            for &p in &old_program.transitive {
                let filter = old_program.transitive_filter(p);
                let eligible = |[s, _, o]: Triple| filter.is_none_or(|f| f.keeps(s, o, &stage));
                let (mut sources, mut targets) = (Vec::new(), Vec::new());
                extra
                    .scan([None, Some(p), None], Seg::Delta, &stage, &mut |t| {
                        // The region's equalities are all overdeleted already.
                        if (Some(p) != same_as || !mentions(&region, t)) && eligible(t) {
                            sources.push(t[0]);
                            targets.push(t[2]);
                        }
                    })
                    .map_err(stopped)?;
                if sources.is_empty() {
                    continue;
                }
                let newly = recloses.insert(p);
                let mut before: HashSet<u64> = sources.iter().copied().collect();
                for &a in &sources {
                    old.scan([None, Some(p), Some(a)], Seg::All, &stage, &mut |t| {
                        if eligible(t) {
                            before.insert(t[0]);
                        }
                    })
                    .map_err(stopped)?;
                }
                let mut after: HashSet<u64> = targets.iter().copied().collect();
                for &b in &targets {
                    old.scan([Some(b), Some(p), None], Seg::All, &stage, &mut |t| {
                        if eligible(t) {
                            after.insert(t[2]);
                        }
                    })
                    .map_err(stopped)?;
                }
                // With B/F (instances valid), pairs still connected by base edges keep a
                // proof: base edges are still-asserted facts and their one-atom images
                // (symmetry, inverses, subproperties) around the component; their closure
                // is computed by SCC condensation.
                let connected: HashSet<(u64, u64)> = if backward {
                    let mut edges = Vec::new();
                    for &u in before.iter().chain(&after) {
                        // A probe its poller stopped skips its fact: the scan ends at its next
                        // position and answers `Stopped`.
                        old.scan([Some(u), None, None], Seg::All, &stage, &mut |t| {
                            if extra.contains(t, &stage) != Ok(false)
                                || base.is_asserted(t, &stage) != Ok(true)
                            {
                                return;
                            }
                            if t[1] == p && eligible(t) {
                                edges.push((t[0], t[2]));
                            }
                            // A fact's images grow with the grounded rules that match it: a
                            // stopped probe ends them, and the scan at its next
                            // position.
                            for image in old_program.one_step_images(t) {
                                if image[1] != p || !eligible(image) {
                                    continue;
                                }
                                match extra.contains(image, &stage) {
                                    Ok(false) => edges.push((image[0], image[2])),
                                    Ok(true) => {}
                                    Err(Stopped) => return,
                                }
                            }
                        })
                        .map_err(stopped)?;
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
                    })
                    .map_err(stopped)?;
                }
                if newly && component.len() == before_len {
                    recloses.remove(&p);
                }
            }
            for rule in &fresh {
                jobs.extend(Job::full(&old, rule, &stage).map_err(stopped)?);
            }
            // Only inferred facts of the old state are overdeleted; asserted ones stay.
            let overdeletable = |f: Triple, poll: &Poller<'_>| -> Result<bool, Stopped> {
                let inferred = if internal(f[1]) {
                    table_contains(table, f, poll)?
                } else {
                    base.contains(f, poll)? && !base.is_asserted(f, poll)?
                };
                Ok(inferred && !extra.contains(f, poll)? && !inserted_set.contains(&f))
            };
            let mut candidates =
                run_jobs_acyclic(&old, &jobs, &overdeletable, stop).map_err(stopped)?;
            candidates.extend(
                run_jobs_acyclic(&outside, &equality_jobs, &overdeletable, stop)
                    .map_err(stopped)?,
            );
            check_stop()?;
            // The first probe that finds the stage stopped ends the update (a stopped probe read as "keep" walked every candidate after
            // it).
            for f in bodiless.into_iter().chain(component) {
                if overdeletable(f, &stage).map_err(stopped)? {
                    candidates.push(f);
                }
            }
            // Only RDF facts are bulk-overdeleted with the equality region.
            // Table facts still need to follow their deleted premises.
            candidates.retain(|&f| internal(f[1]) || !mentions(&region, f));
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
                )
                .map_err(stopped)?;
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
                if !candidates.is_empty() && non_recursive.is_none() {
                    non_recursive = Some(build_non_recursive()?);
                }
                let non_recursive = non_recursive.as_deref().unwrap_or_default();
                let mut gone: HashSet<Triple> = inserted_set.clone();
                gone.extend(overdeleted.iter().copied());
                let left = Overlay {
                    base,
                    hidden: &gone,
                    extra: &empty,
                    table,
                };
                candidates.sort_unstable();
                candidates.dedup();
                // Independent per candidate: in parallel. The first answer its stop ended ends
                // them all (rayon's `Result` collect), never a "not derivable" kept.
                let kept: Vec<Option<Triple>> = candidates
                    .into_par_iter()
                    .map(|f| {
                        old_program
                            .derivable_by(&left, f, non_recursive, false, stop)
                            .map(|derivable| (!derivable).then_some(f))
                    })
                    .collect::<Result<_, Stopped>>()
                    .map_err(stopped)?;
                candidates = kept.into_iter().flatten().collect();
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
                // Each worker gets its own memo caches while sharing one pass budget.
                let fresh_prover = || Prover {
                    base,
                    program: old_program,
                    proved: HashSet::new(),
                    failed: HashSet::new(),
                    active: HashSet::new(),
                    budget: &proof_budget,
                    poll: super::eval::Poller::new(stop),
                };
                // The first proof a stop ended ends them all (a stopped
                // candidate was kept unproved, and every one after it asked the stop again).
                let kept: Vec<Option<Triple>> = candidates
                    .into_par_iter()
                    .map_init(
                        fresh_prover,
                        |prover, f| -> Result<Option<Triple>, Stopped> {
                            let (proved, _) = prover.prove(&old, &extra, f, 0)?;
                            Ok((!proved).then_some(f))
                        },
                    )
                    .collect::<Result<_, Stopped>>()
                    .map_err(stopped)?;
                candidates = kept.into_iter().flatten().collect();
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
    let mut lists_changed = deleted
        .iter()
        .chain(&overdeleted)
        .any(|&f| rules.is_list_fact(f));
    let settle = |unsettled: &HashSet<Triple>| -> Cow<'_, GroundProgram> {
        if program_changed {
            Cow::Owned(rules.program(
                &Overlay {
                    base,
                    hidden: unsettled,
                    extra: &empty,
                    table,
                },
                predicates(),
                &stage,
                &[],
            ))
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
                table,
            };
            let not_equality: Vec<bool> = program
                .equality
                .iter()
                .map(|rule| same_as.is_none() || rule.is_none())
                .collect();
            // Bodiless instances too: their schema facts are what remains, every open fact
            // hidden.
            // The first answer its stop ended ends them all, never a "not derivable" kept.
            let by_rules: Vec<Option<Triple>> = open
                .par_iter()
                .copied()
                .map(|f| {
                    program
                        .derivable_by(&remaining, f, &not_equality, true, stop)
                        .map(|derivable| derivable.then_some(f))
                })
                .collect::<Result<_, Stopped>>()
                .map_err(stopped)?;
            let by_rules: HashSet<Triple> = by_rules.into_iter().flatten().collect();
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
                    remaining
                        .scan(pattern, Seg::All, &stage, &mut |f| {
                            if f[0] != f[2] {
                                classes.union(f[0], f[2]);
                            }
                        })
                        .map_err(stopped)?;
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
        table,
    };
    let candidates: Vec<Triple> = overdeleted
        .iter()
        .copied()
        .filter(|f| internal(f[1]) || !mentions(&region, *f))
        .chain(deleted.iter().copied())
        .collect();
    let settled: &GroundProgram = &program;
    // Each task's probe of a candidate counts on its own poller, before the search makes
    // one of its own. The first answer a stop ended ends them all.
    let others: Vec<Option<Triple>> = candidates
        .par_iter()
        .copied()
        .map_init(
            || Poller::new(stop),
            |poll, fact| -> Result<Option<Triple>, Stopped> {
                let poll = poll.as_ref().ok_or(Stopped)?;
                let recloses = recloses.contains(&fact[1]);
                Ok(((internal(fact[1]) || !base.is_asserted(fact, poll)?)
                    && settled.derivable_with(&remaining, fact, !recloses, stop)?)
                .then_some(fact))
            },
        )
        .collect::<Result<_, Stopped>>()
        .map_err(stopped)?;
    rederived.extend(others.into_iter().flatten());
    check_stop()?;

    lap(2, &mut result);
    // 3. Insert: semi-naive from the rederived and inserted facts.
    let schema_rules = rules.schema_rules();
    result.policy.rederived = (rederived.len() + put_back.len()) as u64;
    let mut seeds = rederived;
    seeds.extend_from_slice(inserted);
    let mut extra = Store::new(seeds);
    let mut module_output: HashSet<Triple> = HashSet::new();
    // The equality module replaces eq-rep-s/p/o here, as in the batch executor.
    // Over `same_as`, the id the rules hold the equality rules over, if they do.
    let mut equality = same_as.map(Equality::over);
    let replaced = |rule: Option<&str>| same_as.is_some() && is_replacement(rule);
    let mut reclose = recloses;
    loop {
        check_stop()?;
        result.rounds += 1;
        let state = Overlay {
            base,
            hidden: &hidden,
            extra: &extra,
            table,
        };
        // Instances through new schema facts, and list rules from new list facts. They
        // are appended, so the new ones are `evaluated..`.
        let evaluated = program.rules.len();
        let transitive_before = program.transitive.clone();
        let mut delta_facts = Vec::new();
        extra
            .scan([None, None, None], Seg::Delta, &stage, &mut |f| {
                delta_facts.push(f)
            })
            .map_err(stopped)?;
        // The program grows from these: only from a delta read whole.
        check_stop()?;
        let schema_changed = delta_facts.iter().any(|&f| schema.is_schema_fact(f));
        if schema_changed {
            program.to_mut().ground_delta(&state, schema, &schema_rules);
        }
        lists_changed |= rules
            .extend_lists(&state, &mut program, &delta_facts, schema_changed, &stage)
            .map_err(stopped)?;
        let mut candidates = if program.has_pending_facts() {
            program.to_mut().take_facts()
        } else {
            Vec::new()
        };
        candidates = new_in(&state, candidates, &stage).map_err(stopped)?;
        let mut jobs = Vec::new();
        // A stopped read ends the job list.
        for (r, i) in program.variants(&extra, &stage).map_err(stopped)? {
            if r < evaluated && !replaced(program.equality[r]) {
                jobs.extend(Job::variant(&state, &program.rules[r], i, &stage).map_err(stopped)?);
            }
        }
        for (r, rule) in program.rules.iter().enumerate().skip(evaluated) {
            if !replaced(program.equality[r]) {
                jobs.extend(Job::full(&state, rule, &stage).map_err(stopped)?);
            }
        }
        let keep = |fact: Triple, poll: &Poller<'_>| Ok(!state.contains(fact, poll)?);
        candidates.extend(run_jobs(&state, &jobs, &keep, stop).map_err(stopped)?);
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
            let filter = program.transitive_filter(p);
            let eligible = |[s, _, o]: Triple| filter.is_none_or(|f| f.keeps(s, o, &stage));
            if !transitive_before.contains(&p) || reclose.remove(&p) {
                let mut all = Vec::new();
                state
                    .scan([None, Some(p), None], Seg::All, &stage, &mut |t| {
                        if eligible(t) {
                            all.push((t[0], t[2]));
                        }
                    })
                    .map_err(stopped)?;
                let closure: Vec<Triple> = nrese_exec::graph::transitive_closure_until(&all, stop)
                    .ok_or_else(Interrupted::default)?
                    .into_iter()
                    .map(|(s, o)| [s, p, o])
                    .collect();
                produced.extend(new_in(&state, closure, &stage).map_err(stopped)?);
                continue;
            }
            let mut edges = Vec::new();
            state
                .scan([None, Some(p), None], Seg::Delta, &stage, &mut |t| {
                    if !module_output.contains(&t) && eligible(t) {
                        edges.push((t[0], t[2]));
                    }
                })
                .map_err(stopped)?;
            produced.extend(close_edges(&state, p, &edges, eligible, &stage).map_err(stopped)?);
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
    extra
        .scan([None, None, None], Seg::All, &stage, &mut |f| added.push(f))
        .map_err(stopped)?;
    let mut internal_added = Vec::new();
    if !program.table_rules.is_empty() {
        extra
            .scan_table(&stage, |fact| internal_added.push(fact))
            .map_err(stopped)?;
    }
    added.sort_unstable();
    added.dedup();
    // Two probes per added fact, on the stage's poller; the first that finds it stopped ends
    // the update.
    let visible_before = |f: Triple| -> Result<bool, Stopped> {
        Ok(base.contains(f, &stage)? && !hidden.contains(&f) && !inserted_set.contains(&f))
    };
    // Each added fact's visibility before, asked once (the insert and the split below).
    let mut before: Vec<bool> = Vec::with_capacity(added.len());
    for &f in &added {
        before.push(visible_before(f).map_err(stopped)?);
    }
    let mut insert = Vec::new();
    for (&f, &visible) in added.iter().zip(&before) {
        if !base.is_asserted(f, &stage).map_err(stopped)? && !(visible || overdeleted.contains(&f))
        {
            insert.push(f);
        }
    }
    result.insert = insert;
    let kept: HashSet<Triple> = added.iter().copied().collect();
    result.remove = overdeleted
        .iter()
        .copied()
        .filter(|f| !internal(f[1]) && !kept.contains(f) && !put_back.contains(f))
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
    let (mut new_facts, mut restored): (Vec<Triple>, Vec<Triple>) = (Vec::new(), Vec::new());
    for (&f, &visible) in added.iter().zip(&before) {
        match inserted_set.contains(&f) || (!visible && !overdeleted.contains(&f)) {
            true => new_facts.push(f),
            false => restored.push(f),
        }
    }
    let mut fresh = Store::new(restored);
    fresh.advance(new_facts);
    let state = Overlay {
        base,
        hidden: &hidden,
        extra: &fresh,
        table,
    };
    // When phase 2 borrows the old program, phase 3 only appends instances.
    let first_full = if program_changed {
        0
    } else {
        old_program.consistency.len()
    };
    let rebuilt = if program_changed {
        let mut full = Vec::with_capacity(program.consistency.len());
        for (rule, grounded) in &program.consistency {
            if stage.tick() {
                return Err(Interrupted::default());
            }
            full.push(!old_program.has_consistency(rule, grounded));
        }
        Some(full)
    } else {
        None
    };
    let is_full = |c| rebuilt.as_ref().map_or(c >= first_full, |full| full[c]);
    let full_consistency = (first_full..program.consistency.len()).filter(|&c| is_full(c));
    let variants: Vec<(usize, usize)> = program
        .consistency_variants(&fresh, &stage)
        .map_err(|Stopped| Interrupted::default())?
        .into_iter()
        .filter(|&(c, _)| !is_full(c))
        .collect();
    let deleted_facts = OnceCell::new();
    let was_present = old_witness_filter(
        old_program,
        || Overlay {
            base,
            hidden: &inserted_set,
            extra: deleted_facts.get_or_init(|| Store::new(deleted.to_vec())),
            table,
        },
        memory,
        operation,
    );
    // An instance evaluated in full can report an old witness even on an insert,
    // for example when adding a list member changes its guard.
    let exclude_old = (!old_program.consistency.is_empty()
        && (!deleted.is_empty() || first_full < program.consistency.len()))
    .then_some(&was_present as &super::batch::WitnessFilter<'_>);
    let mut found = Violations::new(Arc::clone(memory));
    check(
        &state,
        &program,
        full_consistency,
        &variants,
        &mut found,
        stop,
        exclude_old,
    )?;
    // The index borrows the old program, which may be moved into the result below.
    drop(was_present);
    check_stop()?;
    result.violations = found;
    lap(4, &mut result);
    result.diagnostics = program
        .list_diagnostics
        .iter()
        .filter(|d| !old_program.list_diagnostics.contains(d))
        .copied()
        .collect();
    let reading = |p: &GroundProgram| -> HashSet<(u64, u64, u64)> {
        (p.list_diagnostics.iter())
            .map(|d| (d.subject, d.predicate, d.head))
            .collect()
    };
    result.lists_read_otherwise =
        matches!(program, Cow::Owned(_)) && reading(&program) != reading(old_program);
    if lists_changed && (!old_program.table_rules.is_empty() || !program.table_rules.is_empty()) {
        // Temporary programs grow monotonically through insertion rounds.
        // Reground where a per-cell rule can need retiring or its table can
        // change. Enumerated additions already grew the program above.
        let state = Overlay {
            base,
            hidden: &hidden,
            extra: &extra,
            table: None,
        };
        let mut current = rules.program(&state, program.list_predicates.clone(), &stage, &[]);
        stage.ended().map_err(stopped)?;
        current.table = rebuild_table(&current, &state, stop).map_err(stopped)?;
        program = Cow::Owned(current);
    } else if !internal_added.is_empty() || overdeleted.iter().any(|fact| internal(fact[1])) {
        if let Some(table) = table {
            table
                .scan_table(&stage, |fact| {
                    if !overdeleted.contains(&fact) || put_back.contains(&fact) {
                        internal_added.push(fact);
                    }
                })
                .map_err(stopped)?;
        }
        check_stop()?;
        program.to_mut().table = Some(Arc::new(Store::new(internal_added)));
    } else if matches!(program, Cow::Owned(_)) {
        program.to_mut().table = old_program.table.clone();
    }
    result.program = match program {
        Cow::Owned(program) => Some(program),
        Cow::Borrowed(_) => None,
    }
    .or(computed);
    Ok(result)
}

/// Probe the before state only when there is a candidate witness to report.
/// An insert adding consistency instances can be refused if the old-witness index exceeds the memory budget.
fn old_witness_filter<'a, S: Source>(
    program: &'a GroundProgram,
    before: impl Fn() -> S + 'a,
    memory: &'a Budget,
    operation: &'a super::eval::OperationStop<'_>,
) -> impl Fn(&Rule, &[Option<u64>], &Poller<'_>) -> Result<bool, Stopped> + 'a {
    let old_witnesses = OnceCell::new();
    move |rule, bindings, poll| {
        let index = old_witnesses
            .get_or_init(|| WitnessIndex::new(program, memory, operation, poll).ok())
            .as_ref()
            .ok_or(Stopped)?;
        index.contains(&before(), rule, bindings, poll)
    }
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
    introduced_violations_body(base, inserted, derived, rules, cache, operation, memory)
        .map_err(|_| operation.interrupted())
}

fn introduced_violations_body<B: Base + ?Sized>(
    base: &B,
    inserted: &[Triple],
    derived: &[Triple],
    rules: Rules<'_>,
    cache: Option<&GroundProgram>,
    operation: &super::eval::OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<Violations, Interrupted> {
    let stop: Stop<'_> = &|| operation.fired();
    let stage = Poller::new(stop).ok_or_else(Interrupted::default)?;
    let inserted_set: HashSet<Triple> = inserted.iter().copied().collect();
    let empty = Store::default();
    let before = Overlay {
        base,
        hidden: &inserted_set,
        extra: &empty,
        table: None,
    };
    let computed = if cache.is_none() {
        let mut program = rules.program(&before, Predicates::default(), &stage, &[]);
        stage.ended().map_err(|Stopped| Interrupted::default())?;
        program.table =
            rebuild_table(&program, &before, stop).map_err(|Stopped| Interrupted::default())?;
        Some(program)
    } else {
        None
    };
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
        table: old_program.table.as_deref(),
    };
    let mut program: Cow<'_, GroundProgram> = Cow::Borrowed(old_program);
    let mut delta_facts = Vec::new();
    fresh
        .scan([None, None, None], Seg::Delta, &stage, &mut |f| {
            delta_facts.push(f)
        })
        .map_err(|Stopped| Interrupted::default())?;
    if stop() {
        return Err(Interrupted::default());
    }
    let schema_changed = delta_facts.iter().any(|&f| rules.schema.is_schema_fact(f));
    if schema_changed {
        program
            .to_mut()
            .ground_delta(&state, rules.schema, &rules.schema_rules());
    }
    rules
        .extend_lists(&state, &mut program, &delta_facts, schema_changed, &stage)
        .map_err(|Stopped| Interrupted::default())?;
    let variants: Vec<(usize, usize)> = program
        .consistency_variants(&fresh, &stage)
        .map_err(|Stopped| Interrupted::default())?
        .into_iter()
        .filter(|&(c, _)| c < settled_consistency)
        .collect();
    let was_present = old_witness_filter(
        old_program,
        || Overlay {
            table: old_program.table.as_deref(),
            ..before
        },
        memory,
        operation,
    );
    let exclude_old = (settled_consistency > 0 && settled_consistency < program.consistency.len())
        .then_some(&was_present as &super::batch::WitnessFilter<'_>);
    let mut found = Violations::new(Arc::clone(memory));
    // It asks the latched stop again before its report is made: the variants above too.
    check(
        &state,
        &program,
        settled_consistency..program.consistency.len(),
        &variants,
        &mut found,
        stop,
        exclude_old,
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
) -> Result<Vec<Triple>, Stopped> {
    let mut facts: Vec<Triple> = Vec::new();
    for &term in terms {
        for position in 0..3 {
            let mut pattern = [None; 3];
            pattern[position] = Some(term);
            base.scan(pattern, poll, &mut |t| {
                if !t[..position].iter().any(|earlier| terms.contains(earlier)) {
                    facts.push(t);
                }
            })?;
        }
    }
    Ok(facts)
}

/// The asserted facts of `base` that mention one of `terms`.
fn asserted_mentioning<B: Base + ?Sized>(
    base: &B,
    terms: &HashSet<u64>,
    poll: &Poller<'_>,
) -> Result<HashSet<Triple>, Stopped> {
    let mut facts = HashSet::new();
    for &term in terms {
        for pattern in [
            [Some(term), None, None],
            [None, Some(term), None],
            [None, None, Some(term)],
        ] {
            base.scan_asserted(pattern, poll, &mut |t| {
                facts.insert(t);
            })?;
        }
    }
    Ok(facts)
}

/// The facts of `facts` that `state` doesn't hold, in order; [`Stopped`] at the first probe
/// its poller stopped, the partial list dropped (a stopped probe read
/// as "not held" kept every fact after it).
fn new_in<S: Source + ?Sized>(
    state: &S,
    facts: Vec<Triple>,
    poll: &Poller<'_>,
) -> Result<Vec<Triple>, Stopped> {
    let mut out = Vec::with_capacity(facts.len());
    for fact in facts {
        if !state.contains(fact, poll)? {
            out.push(fact);
        }
    }
    Ok(out)
}

/// Whether `fact` mentions one of `terms`.
fn mentions(terms: &HashSet<u64>, fact: Triple) -> bool {
    !terms.is_empty() && fact.iter().any(|term| terms.contains(term))
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
        // Stopped, the read around this visit answers `Stopped` (the poller is latched).
        let delta = seg.plain() == Seg::Delta;
        let scanned = self.source.scan(pattern, seg, visit.poller(), &mut |fact| {
            if !(delta && mentions(self.region, fact)) {
                visit.counted(fact);
            }
        });
        debug_assert!(scanned.is_ok() || visit.stopped());
    }

    fn size(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<usize, Stopped> {
        self.source.estimate(pattern, seg, probe.poller())
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
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
    budget: &'a AtomicUsize,
    /// The update's stop, one poller per parallel task: each alternative, producer, search
    /// and scanned fact of a proof is attempted work. `None`: stopped before
    /// the task began.
    poll: Option<super::eval::Poller<'p>>,
}

/// Proof expansions shared by all provers in one overdelete pass (when exhausted, remaining
/// candidates are overdeleted and rederived if still entailed).
const PROOF_BUDGET: usize = 10_000;

/// Derivations tried per fact, and the proof depth.
const PROOF_BRANCHES: usize = 32;
const PROOF_DEPTH: usize = 64;

impl<B: Base + ?Sized> Prover<'_, '_, B> {
    /// Whether `fact` is provable over `state` without the facts in `gone`; the second
    /// value says whether a cycle (or the budget) cut the search. [`Stopped`] at the first
    /// probe its poller stopped: nothing of that search is memoised.
    fn prove<S: Source + ?Sized>(
        &mut self,
        state: &S,
        gone: &Store,
        fact: Triple,
        depth: usize,
    ) -> Result<(bool, bool), Stopped> {
        if self.proved.contains(&fact) {
            return Ok((true, false));
        }
        // The probes below count on the task's poller too (REAS-2).
        let Some(poll) = self.poll.as_ref() else {
            return Err(Stopped);
        };
        if self.failed.contains(&fact) || gone.contains(fact, poll)? {
            return Ok((false, false));
        }
        if self.base.is_asserted(fact, poll)? {
            self.proved.insert(fact);
            return Ok((true, false));
        }
        // Transitive properties go through the component path of overdeletion instead (a
        // proof search through a k-clique is O(k^3)): never proved here, so it's a failure
        // that can be memoised.
        if self.program.transitive.contains(&fact[1]) {
            return Ok((false, false));
        }
        if self.active.contains(&fact) || depth > PROOF_DEPTH {
            return Ok((false, true));
        }
        if self
            .budget
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_err()
        {
            #[cfg(test)]
            budget_tests::record_exhaustion();
            return Ok((false, true));
        }
        #[cfg(test)]
        budget_tests::record_expansion();
        // Stopped: the bodies are a part of them, so the search ends here.
        let bodies = self
            .program
            .derivation_bodies(state, fact, PROOF_BRANCHES, poll)?;
        let mut cut = false;
        self.active.insert(fact);
        let found = self.prove_bodies(state, gone, bodies, depth, &mut cut);
        self.active.remove(&fact);
        if found? {
            self.proved.insert(fact);
            return Ok((true, false));
        }
        if !cut {
            self.failed.insert(fact);
        }
        Ok((false, cut))
    }

    /// Whether one of `bodies` has every premise proved ([`Self::prove`]), `cut` set where a
    /// cycle or the budget cut a premise's search.
    fn prove_bodies<S: Source + ?Sized>(
        &mut self,
        state: &S,
        gone: &Store,
        bodies: Vec<Vec<Triple>>,
        depth: usize,
        cut: &mut bool,
    ) -> Result<bool, Stopped> {
        'bodies: for body in bodies {
            let poll = self.poll.as_ref().ok_or(Stopped)?;
            for &g in &body {
                if gone.contains(g, poll)? {
                    continue 'bodies;
                }
            }
            for &g in &body {
                let (proved, cycle) = self.prove(state, gone, g, depth + 1)?;
                *cut |= cycle;
                if !proved {
                    continue 'bodies;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }
}

/// Closes `p` after adding `edges` to a relation that is transitively closed apart from
/// them: for each edge `(a, b)`, every predecessor of `a` (and `a`) gets every successor
/// of `b` (and `b`). Edges are processed one at a time over the relation including the
/// pairs added so far, which keeps it closed. Returns the new pairs. Each scan, each pair
/// visited and each probe counts on `poll`; once that has stopped it answers [`Stopped`],
/// its pairs so far dropped (sweep M11: a predecessor × successor product per
/// edge).
fn close_edges<S: Source + ?Sized>(
    state: &S,
    p: u64,
    edges: &[(u64, u64)],
    eligible: impl Fn(Triple) -> bool,
    poll: &Poller<'_>,
) -> Result<Vec<Triple>, Stopped> {
    use hashbrown::HashMap;
    let mut added: HashSet<(u64, u64)> = HashSet::new();
    let mut successors: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut predecessors: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut out = Vec::new();
    for &(a, b) in edges {
        let mut before = vec![a];
        state.scan([None, Some(p), Some(a)], Seg::All, poll, &mut |t| {
            if eligible(t) {
                before.push(t[0]);
            }
        })?;
        before.extend(predecessors.get(&a).into_iter().flatten().copied());
        let mut after = vec![b];
        state.scan([Some(b), Some(p), None], Seg::All, poll, &mut |t| {
            if eligible(t) {
                after.push(t[2]);
            }
        })?;
        after.extend(successors.get(&b).into_iter().flatten().copied());
        before.sort_unstable();
        before.dedup();
        after.sort_unstable();
        after.dedup();
        for &x in &before {
            for &y in &after {
                if poll.tick() {
                    return Err(Stopped);
                }
                let fact = [x, p, y];
                if !added.contains(&(x, y)) && !state.contains(fact, poll)? {
                    added.insert((x, y));
                    successors.entry(x).or_default().push(y);
                    predecessors.entry(y).or_default().push(x);
                    out.push(fact);
                }
            }
        }
    }
    Ok(out)
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

    pub(crate) fn add_inferred(&mut self, facts: Vec<Triple>) {
        self.facts.advance(facts);
    }
}

/// Reads count on the store's (`Source for Store`); a probe of the asserted set is
/// one unit.
impl VisitsBase for MemoryBase {
    fn visit(&self, pattern: [Option<u64>; 3], visit: &mut Visit<'_, '_>) {
        self.facts.visit(pattern, Seg::All, visit);
    }

    fn size(&self, pattern: [Option<u64>; 3], probe: Probe<'_, '_>) -> Result<usize, Stopped> {
        self.facts.size(pattern, Seg::All, probe)
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        self.facts.holds(fact, probe)
    }

    fn asserts(&self, fact: Triple, _: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(self.asserted.contains(&fact))
    }
}

#[cfg(test)]
mod budget_tests {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::batch::{self, Schema};
    use crate::delta::policy::{Plan, Policy};
    use crate::ir::{Atom, Term, Violation, Vocabulary};
    use crate::lists::{ListVocabulary, PAIRWISE_MEMBERS};
    use crate::vocabulary::LocalVocabulary;

    fn implication(name: &str, body: Vec<Atom>, head: Atom) -> Rule {
        Rule {
            name: name.into(),
            body,
            guards: Vec::new(),
            head: Head::Facts(vec![head]),
        }
    }

    const PROOF_CELLS: [usize; 3] = [0, 2, usize::MAX];
    const POOL_WIDTHS: [usize; 3] = [1, 4, 8];

    struct ProofWork {
        capacity: usize,
        expanded: AtomicUsize,
        exhausted: AtomicUsize,
    }

    thread_local! {
        static POOL_PROOF: RefCell<Option<Arc<ProofWork>>> = const { RefCell::new(None) };
    }

    pub(super) fn capacity(default: usize) -> usize {
        POOL_PROOF.with(|cell| cell.borrow().as_ref().map_or(default, |work| work.capacity))
    }

    pub(super) fn record_expansion() {
        POOL_PROOF.with(|cell| {
            if let Some(work) = cell.borrow().as_ref() {
                work.expanded.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    pub(super) fn record_exhaustion() {
        POOL_PROOF.with(|cell| {
            if let Some(work) = cell.borrow().as_ref() {
                work.exhausted.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    fn run_delete(
        base: &MemoryBase,
        inserted: &[Triple],
        deleted: &[Triple],
        rules: Rules<'_>,
        threads: usize,
        capacity: usize,
    ) -> (Update, usize, usize) {
        let work = Arc::new(ProofWork {
            capacity,
            expanded: AtomicUsize::new(0),
            exhausted: AtomicUsize::new(0),
        });
        let worker = Arc::clone(&work);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .start_handler(move |_| {
                POOL_PROOF.with(|cell| *cell.borrow_mut() = Some(Arc::clone(&worker)));
            })
            .build()
            .unwrap();
        let policy = Policy {
            force: Some(Plan::Dred),
            ..Policy::default()
        };
        let result = pool
            .install(|| {
                update_with(
                    base,
                    inserted,
                    deleted,
                    rules,
                    None,
                    crate::eval::NEVER,
                    policy,
                )
            })
            .expect("never stopped");
        (
            result,
            work.expanded.load(Ordering::Relaxed),
            work.exhausted.load(Ordering::Relaxed),
        )
    }

    fn maintained(stack: &[Triple], update: &Update) -> Vec<Triple> {
        let mut closure: Vec<Triple> = stack
            .iter()
            .copied()
            .filter(|f| update.remove.binary_search(f).is_err())
            .chain(update.insert.iter().copied())
            .collect();
        closure.sort_unstable();
        closure.dedup();
        closure
    }

    fn reach(e: u64, p: u64) -> Rule {
        let [x, y, z] = [0, 1, 2].map(Term::Var);
        implication(
            "reach",
            vec![Atom([x, Term::Const(e), y]), Atom([y, Term::Const(p), z])],
            Atom([x, Term::Const(p), z]),
        )
    }

    fn assert_budget(capacity: usize, used: usize, exhausted: usize, cell: &str) {
        if capacity == usize::MAX {
            assert!(
                used > PROOF_CELLS[1] && exhausted == 0,
                "{cell}: expected a completed proof search, used={used}, denied={exhausted}"
            );
        } else {
            assert_eq!(
                used, capacity,
                "{cell}: proof expansions exceed the shared pass budget or did not spend it"
            );
            assert!(exhausted > 0, "{cell}: no proof was denied by the budget");
        }
    }

    /// The shared cap bounds total proof expansions in a pass, irrespective of pool width.
    /// The closure guard separately checks equality with rematerialisation at every budget.
    #[test]
    fn shared_proof_budget_caps_total_expansions_and_matches_rematerialisation() {
        let mut vocab = LocalVocabulary::default();
        // A recursive reachability rule and its seeds.
        let e = vocab.iri("http://example.com/e");
        let p = vocab.iri("http://example.com/p");
        let seed = vocab.iri("http://example.com/seed");
        let [x, y] = [0, 1].map(Term::Var);
        let rules = [
            reach(e, p),
            implication(
                "r2",
                vec![Atom([x, Term::Const(seed), y])],
                Atom([x, Term::Const(p), y]),
            ),
        ];
        let schema = Schema::owl(&mut vocab);
        let nodes = 8usize;
        let targets = 4usize;
        let mut asserted = Vec::new();
        // Dense small cycle among nodes so proof searches branch heavily.
        for i in 0..nodes {
            let a = vocab.iri(&format!("http://example.com/n{i}"));
            for step in [1usize, 2, 3, 5] {
                let b = vocab.iri(&format!("http://example.com/n{}", (i + step) % nodes));
                asserted.push([a, e, b]);
            }
        }
        let root = vocab.iri("http://example.com/n0");
        let mut deleted = Vec::new();
        for t in 0..targets {
            let target = vocab.iri(&format!("http://example.com/t{t}"));
            let fact = [root, seed, target];
            asserted.push(fact);
            deleted.push(fact);
        }
        asserted.sort_unstable();
        asserted.dedup();

        let before = batch::materialise(&asserted, &rules, None, &schema).derived;
        let after_asserted: Vec<Triple> = asserted
            .iter()
            .copied()
            .filter(|f| !deleted.contains(f))
            .collect();
        let expected = batch::materialise(&after_asserted, &rules, None, &schema).derived;
        let stack: Vec<Triple> = before
            .iter()
            .copied()
            .filter(|f| after_asserted.binary_search(f).is_err())
            .collect();

        let base = MemoryBase::new(&after_asserted, &stack);
        let compiled = Rules {
            rules: &rules,
            lists: None,
            schema: &schema,
            equality: None,
        };
        for threads in POOL_WIDTHS {
            for capacity in PROOF_CELLS {
                let cell = format!("closure threads={threads} budget={capacity}");
                let (result, used, exhausted) =
                    run_delete(&base, &[], &deleted, compiled, threads, capacity);
                assert_eq!(maintained(&stack, &result), expected, "{cell}");
                assert_budget(capacity, used, exhausted, &cell);
                println!("{cell}: expanded={used} denied={exhausted}");
            }
        }
    }

    fn check_witnesses(
        scenario: &str,
        asserted: &[Triple],
        inserted: &[Triple],
        deleted: Triple,
        rules: Rules<'_>,
        witness_counts: (usize, usize, usize),
    ) -> Vec<String> {
        let full = |facts| batch::materialise(facts, rules.rules, rules.lists, rules.schema);
        let before = full(asserted);
        let mut after_asserted: Vec<_> = asserted
            .iter()
            .copied()
            .filter(|&f| f != deleted)
            .chain(inserted.iter().copied())
            .collect();
        after_asserted.sort_unstable();
        let after = full(&after_asserted);
        let old: HashSet<Violation> = before.violations.to_vec().into_iter().collect();
        let now: HashSet<Violation> = after.violations.to_vec().into_iter().collect();
        let expected: HashSet<_> = now.difference(&old).cloned().collect();
        assert_eq!(
            (old.len(), now.len(), expected.len()),
            witness_counts,
            "{scenario}: before={old:?} after={now:?}"
        );
        let stack: Vec<_> = before
            .derived
            .into_iter()
            .filter(|f| after_asserted.binary_search(f).is_err())
            .collect();
        let base = MemoryBase::new(&after_asserted, &stack);
        let mut mismatches = Vec::new();
        for threads in POOL_WIDTHS {
            for capacity in PROOF_CELLS {
                let cell = format!("{scenario} threads={threads} budget={capacity}");
                let (result, used, exhausted) =
                    run_delete(&base, inserted, &[deleted], rules, threads, capacity);
                assert_eq!(maintained(&stack, &result), after.derived, "{cell}");
                let reported: HashSet<_> = result.violations.to_vec().into_iter().collect();
                if reported != expected {
                    mismatches.push(format!("{cell}: expected {expected:?}, got {reported:?}"));
                }
                assert_budget(capacity, used, exhausted, &cell);
                println!(
                    "{cell}: expanded={used} denied={exhausted}, expected={} reported={}",
                    expected.len(),
                    reported.len()
                );
            }
        }
        mismatches
    }

    /// When shared proof-budget exhaustion forces a still-supported inferred schema fact
    /// through overdeletion and phase-3 rederivation, consistency instances that already
    /// existed in the pre-update program must not be re-evaluated in full and reported as
    /// newly introduced violations, while a genuinely new clash against the same restored
    /// consistency instance must still be reported.
    #[test]
    fn budget_exhaustion_does_not_report_preexisting_clash_as_introduced() {
        let mut vocab = LocalVocabulary::default();
        let e = vocab.iri("http://example.com/e");
        let p = vocab.iri("http://example.com/p");
        let mark = vocab.iri("http://example.com/mark");
        let disjoint = vocab.iri("http://www.w3.org/2002/07/owl#disjointWith");
        let [x, y] = [0, 1].map(Term::Var);
        let mut rules = crate::ir::parse_rules(
            "cax-dw: (?c owl:disjointWith ?d), (?x rdf:type ?c), (?x rdf:type ?d) -> false",
            &mut vocab,
        )
        .unwrap();
        rules.extend([
            reach(e, p),
            implication(
                "disjoint",
                vec![
                    Atom([x, Term::Const(p), y]),
                    Atom([x, Term::Const(mark), y]),
                ],
                Atom([x, Term::Const(disjoint), y]),
            ),
        ]);
        let schema = Schema::owl(&mut vocab);
        let rdf_type = vocab.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");

        let root = vocab.iri("http://example.com/root");
        let anchor = vocab.iri("http://example.com/anchor");
        let target = vocab.iri("http://example.com/t");
        let last = vocab.iri("http://example.com/last");
        let ind = vocab.iri("http://example.com/ind");
        let ind2 = vocab.iri("http://example.com/ind2");

        let deleted_fact = [root, p, target];
        let mut asserted = vec![
            deleted_fact,
            [anchor, p, target],
            [last, mark, target],
            [last, e, anchor],
            [ind, rdf_type, last],
            [ind, rdf_type, target],
        ];
        let l1 = vocab.iri("http://example.com/l1");
        let l2 = vocab.iri("http://example.com/l2");
        asserted.extend([[l1, e, root], [l2, e, l1], [last, e, l2]]);
        for i in 0..4 {
            let branch = vocab.iri(&format!("http://example.com/branch{i}"));
            asserted.push([branch, e, root]);
        }
        asserted.sort_unstable();
        let compiled = Rules {
            rules: &rules,
            lists: None,
            schema: &schema,
            equality: None,
        };
        let mut mismatches = check_witnesses(
            "restored axiom",
            &asserted,
            &[],
            deleted_fact,
            compiled,
            (1, 1, 0),
        );
        // Control: when the same update also inserts a genuinely new clashing individual,
        // the restored instance must report only the new clash.
        mismatches.extend(check_witnesses(
            "restored axiom with new clash",
            &asserted,
            &[[ind2, rdf_type, last], [ind2, rdf_type, target]],
            deleted_fact,
            compiled,
            (1, 2, 1),
        ));
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// Deletes can both enable a list check and change a long list's guard. Report the
    /// witnesses introduced by the edit, even when the list's program is rebuilt.
    /// The short case adds the only exit while removing the inferred back edge.
    /// With an earlier exit, the accepting walk would already trigger Table 5 prp-adp.
    #[test]
    fn list_updates_report_only_introduced_witnesses() {
        let mismatches = list_update_witnesses(&[
            ("phase-2 list check", 2, false, (0, 1, 1)),
            (
                "changed long-list guard",
                PAIRWISE_MEMBERS + 1,
                true,
                (1, 1, 0),
            ),
        ]);
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// The two-position exit of an accepting cycle already gives a prp-adp clash.
    /// Removing the back edge or adding a separate plain list with the same pair
    /// can add a short rule instance without introducing that clash.
    #[test]
    fn an_accepting_cycle_does_not_make_a_surviving_clash_new() {
        let mismatches = list_update_witnesses(&[("old accepting cycle", 2, true, (2, 1, 0))]);
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// Table 5 prp-adp still has the same two property assertions after adding an
    /// unused first. Neither the bulk insert report nor the bulk filter checking
    /// the reverse, precomputed closure may count the pair as introduced.
    #[test]
    fn list_form_changes_keep_the_same_pair_in_the_bulk_witness_check() {
        let mut vocab = LocalVocabulary::default();
        let schema = Schema::owl(&mut vocab);
        let lists = ListVocabulary::new(&mut vocab);
        let [list, head, tail, q, a, b, m0] = ["list", "h", "t", "q", "a", "b", "m0"]
            .map(|local| vocab.iri(&format!("http://example.org/{local}")));
        let [first, rest, nil] =
            ["first", "rest", "nil"].map(|local| vocab.iri(&format!("{}{local}", crate::ir::RDF)));
        let [kind, members] = ["AllDisjointProperties", "members"]
            .map(|local| vocab.iri(&format!("{}{local}", crate::ir::OWL)));
        let mut asserted = vec![
            [list, schema.rdf_type(), kind],
            [list, members, head],
            [head, rest, tail],
            [tail, first, q],
            [tail, rest, nil],
            [a, m0, b],
            [a, q, b],
        ];
        asserted
            .extend((0..64).map(|i| [head, first, vocab.iri(&format!("http://example.org/m{i}"))]));
        let extra = [head, first, vocab.iri("http://example.org/m64")];
        let mut larger = asserted.clone();
        larger.push(extra);
        let rules = Rules {
            rules: &[],
            lists: Some(&lists),
            schema: &schema,
            equality: None,
        };
        let small = MemoryBase::new(&asserted, &[]);
        let large = MemoryBase::new(&larger, &[]);
        let stop = crate::eval::OperationStop::new(crate::eval::NEVER);
        let memory = Arc::new(Budget::unlimited());
        let mut errors = Vec::new();
        for warm in [true, false] {
            let cache = warm.then(|| program(&small, rules));
            let grown =
                introduced_violations(&large, &[extra], &[], rules, cache.as_ref(), &stop, &memory)
                    .expect("the bulk insert witness check ran");
            if !grown.consistent() {
                errors.push(format!("bulk 64 to 65 warm={warm}: {:?}", grown.to_vec()));
            }

            // The 65th property can also clash with q on the same subject and
            // object. The distinct pair is new even though the first pair is old.
            let new_property_fact = [a, extra[2], b];
            let mut additional = larger.clone();
            additional.push(new_property_fact);
            let new_pair = introduced_violations(
                &MemoryBase::new(&additional, &[]),
                &[extra, new_property_fact],
                &[],
                rules,
                cache.as_ref(),
                &stop,
                &memory,
            )
            .expect("the bulk insert witness check ran");
            if !new_pair.complete() || new_pair.at_least() != 1 {
                errors.push(format!(
                    "distinct new pair warm={warm}: {:?}",
                    new_pair.to_vec()
                ));
            }

            // The public bulk entry point accepts only inserts. For the reverse
            // edit, run its consistency filter over the precomputed after state.
            let old = match cache.as_ref() {
                Some(small_program) => update(&large, &[extra], &[], rules, Some(small_program))
                    .program
                    .expect("the growth edit refreshed the ground program"),
                None => program(&large, rules),
            };
            let after = program(&small, rules);
            let empty = Store::default();
            let hidden = HashSet::new();
            let before = || Overlay {
                base: &large,
                hidden: &hidden,
                extra: &empty,
                table: old.table.as_deref(),
            };
            let filter = old_witness_filter(&old, before, &memory, &stop);
            let state = Overlay {
                base: &small,
                hidden: &hidden,
                extra: &empty,
                table: after.table.as_deref(),
            };
            let mut found = Violations::new(Arc::clone(&memory));
            check(
                &state,
                &after,
                0..after.consistency.len(),
                &[],
                &mut found,
                crate::eval::NEVER,
                Some(&filter),
            )
            .unwrap();
            if !found.consistent() {
                errors.push(format!(
                    "bulk filter 65 to 64 warm={warm}: {:?}",
                    found.to_vec()
                ));
            }
        }
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }

    type ListWitnessCase<'a> = (&'a str, usize, bool, (usize, usize, usize));

    fn list_update_witnesses(cases: &[ListWitnessCase<'_>]) -> Vec<String> {
        let mut vocab = LocalVocabulary::default();
        let schema = Schema::owl(&mut vocab);
        let lists = ListVocabulary::new(&mut vocab);
        let e = vocab.iri("http://example.com/e");
        let p = vocab.iri("http://example.com/p");
        let source = vocab.iri("http://example.com/source");
        let root = vocab.iri("http://example.com/root");
        let marker = vocab.iri("http://example.com/marker");
        let list = vocab.iri("http://example.com/list");
        let subject = vocab.iri("http://example.com/s");
        let object = vocab.iri("http://example.com/o");
        let other_member = vocab.iri("http://example.com/other-member");
        let first = vocab.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#first");
        let rest = vocab.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#rest");
        let nil = vocab.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil");
        let adp = vocab.iri("http://www.w3.org/2002/07/owl#AllDisjointProperties");
        let members = vocab.iri("http://www.w3.org/2002/07/owl#members");
        let deleted = [root, p, marker];
        let mut mismatches = Vec::new();
        for &(scenario, len, exit_before, witness_counts) in cases {
            let nodes: Vec<_> = (0..len)
                .map(|i| vocab.iri(&format!("http://example.com/cell{i}")))
                .collect();
            let mut asserted = vec![
                deleted,
                [source, e, root],
                [list, schema.rdf_type(), adp],
                [list, members, nodes[0]],
            ];
            for i in 0..4 {
                let branch = vocab.iri(&format!("http://example.com/branch{i}"));
                asserted.push([branch, e, root]);
            }
            let mut inserted = Vec::new();
            let mut member_properties = Vec::new();
            for (i, &node) in nodes.iter().enumerate() {
                let member = vocab.iri(&format!("http://example.com/member{i}"));
                asserted.push([node, first, member]);
                let edge = [node, rest, nodes.get(i + 1).copied().unwrap_or(nil)];
                if !exit_before && i + 1 == len {
                    inserted.push(edge);
                } else {
                    asserted.push(edge);
                }
                if i < 2 {
                    asserted.push([subject, member, object]);
                    member_properties.push(member);
                }
            }
            asserted.sort_unstable();
            // Overdeleting the recursive p fact reaches a list fact in the next wave:
            // a cycle in the short list, or an alternative first member of the long one.
            let depends_on_proof = if len == 2 {
                [nodes[1], rest, nodes[0]]
            } else {
                [nodes[0], first, other_member]
            };
            let rules = [
                reach(e, p),
                implication(
                    "list-edge",
                    vec![Atom([source, p, marker].map(Term::Const))],
                    Atom(depends_on_proof.map(Term::Const)),
                ),
            ];
            let compiled = Rules {
                rules: &rules,
                lists: Some(&lists),
                schema: &schema,
                equality: None,
            };
            mismatches.extend(check_witnesses(
                scenario,
                &asserted,
                &inserted,
                deleted,
                compiled,
                witness_counts,
            ));
            if len == 2 && exit_before {
                // The bulk witness entry point only accepts insertions. Keep the cycle and
                // add the same disjoint pair in the enumerated form as another list axiom.
                let [plain, a, b] = ["plain", "plain0", "plain1"]
                    .map(|local| vocab.iri(&format!("http://example.com/{local}")));
                let extra = [
                    [plain, schema.rdf_type(), adp],
                    [plain, members, a],
                    [a, first, member_properties[0]],
                    [a, rest, b],
                    [b, first, member_properties[1]],
                    [b, rest, nil],
                ];
                let before = batch::materialise(&asserted, &rules, Some(&lists), &schema);
                let mut after = asserted.clone();
                after.extend(extra);
                let base = MemoryBase::new(&after, &before.derived);
                for threads in POOL_WIDTHS {
                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .build()
                        .unwrap();
                    let reported = pool.install(|| {
                        introduced_violations(
                            &base,
                            &extra,
                            &[],
                            compiled,
                            None,
                            &crate::eval::OperationStop::new(crate::eval::NEVER),
                            &Arc::new(Budget::unlimited()),
                        )
                        .expect("never stopped")
                    });
                    if !reported.consistent() {
                        mismatches.push(format!(
                            "bulk form addition, threads={threads}: {:?}",
                            reported.to_vec()
                        ));
                    }
                }
            }
        }
        mismatches
    }
}
