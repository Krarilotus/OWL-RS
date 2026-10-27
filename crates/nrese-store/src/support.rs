//! Inferred statements under graph access (`inferred = "supported"` in the access
//! policy; docs/design/reasoner-provenance.md). A user who may read the graphs `R` sees
//! an inferred statement exactly when the statements in `R` derive it: when one of its
//! support graph sets lies in `R`, or, where the cap on sets left some out, when the
//! closure of the statements in `R` holds it.
//!
//! - **The sets** ([`SupportSets`]) are computed for a committed revision on the first
//!   read that needs them, from the stack and the ground program
//!   ([`nrese_reasoner::graph_sets`]); readers of that revision wait for one
//!   computation. Once there are sets, the mutation pipeline reports what each commit
//!   touched, and the next revision's sets are the last ones updated for the facts that
//!   can follow from those ([`SupportSets::updated`]): a base shared between revisions,
//!   with the recomputed facts on top until they are folded in. A write it didn't report
//!   (a bulk load, a rematerialisation), a change of the schema, or a change reaching too
//!   many facts computes them afresh.
//! - **The cap** (`reasoner.support_sets`) bounds the sets kept per statement, not what a
//!   reader sees. A statement the cap left sets out of (here or in a premise's) is marked;
//!   a reader none of its kept sets fits is decided by the closure of the statements it
//!   may read ([`nrese_reasoner::graph_sets::closure_within`]), computed once for the
//!   view, and only where such a statement is open for it. Graphs are numbered in IRI
//!   order, so which sets the cap keeps doesn't depend on the order terms were stored in.
//! - **A reader's view** is the snapshot with its invisible inferred statements removed
//!   ([`nrese_engine::Snapshot::with_inferred_only`], a mask over the stack where it sees
//!   most): every read path (counts, sorted scans, equality, the query cache keyed by
//!   access) sees the subset alone. Views are kept per revision and access set; the next
//!   revision's view is the last one's list corrected for the facts the update recomputed.
//! - **Not computable** (no reasoner v2 rules registered, or the stack over
//!   representatives of `reasoner.equality = "compact"`): the reader sees no inferred
//!   statement, the safe side.
//! - **Memory:** what the sets and views keep is charged before it is allocated to the
//!   store's support budget (the memory the queries draw on, as the canonical views'),
//!   and stays charged while it lives: a view's by the view itself, released when its
//!   last reader lets it go; a part shared between revisions, readers and the cells they
//!   are computed once in, with its box ([`Shared`]). What computing them reads and builds
//!   is charged to the reading query's envelope; the evaluation's own working set reports
//!   its size at its rounds and is charged there as it goes (an exemption until the
//!   reasoner's working set is charged before it is allocated:
//!   notes/2026-10-20-executor-admission-design.md, W1 site "reasoner fixpoint"). A read
//!   that finds no room fails with the error, never with a partial view; nothing failed is
//!   kept, so the next read tries again.

use std::sync::{Arc, Mutex, OnceLock};

use hashbrown::{HashMap, HashSet};
use nrese_engine::quad::Permutation;
use nrese_engine::{
    EncodedQuad, GraphSelector, InferredMask, QuadPattern, ReadModel, Snapshot, TermId,
};
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Envelope, Reservation};
use nrese_reasoner::RuleProgram;
use nrese_reasoner::eval::{Seg, Source};
use nrese_reasoner::graph_sets::{
    Annotation, Change, GraphSet, Stopped, closure_within, initial_sets, rederived,
    rederived_candidates, support_sets, update_support_sets,
};
use nrese_reasoner::ir::Triple;
use nrese_sparql::{CancellationToken, GraphAccess};

use crate::reasoning::Program;

#[cfg(test)]
#[path = "support_tests.rs"]
mod tests;

/// Restricted views kept per revision.
const VIEWS: usize = 32;

/// Touched facts kept for updates; past this many, the next sets are computed afresh.
const MAX_TOUCHED: usize = 1 << 20;

/// Recomputed facts kept on top of the base before they are folded into it.
const MAX_OVERLAY: usize = 1 << 16;

/// The set index of an overlay entry whose fact has only the sets it starts with.
const OWN: u32 = u32::MAX;

/// A list kept past a read, or built while reading, charged before it grows.
type Kept<T> = ChargedVec<T, Arc<Budget>>;

/// A part kept for readers and shared (between revisions' sets, readers' views and the
/// cells they are computed once in), its box charged with it. The last holder takes it out
/// of its box (freeing the box), drops it, and releases the box's charge last: nothing of
/// it is still there when its charge goes.
#[derive(Debug)]
struct Shared<T>(Option<Arc<Boxed<T>>>);

#[derive(Debug)]
struct Boxed<T> {
    value: T,
    /// Its box. Last: released after the value is freed.
    _held: Reservation<Arc<Budget>>,
}

impl<T> Shared<T> {
    /// `value` in a box charged to `kept` first.
    fn new(value: T, kept: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let mut held = Reservation::new(Arc::clone(kept));
        held.charge(size_of::<Boxed<T>>() + 2 * size_of::<usize>())?;
        Ok(Self(Some(Arc::new(Boxed { value, _held: held }))))
    }

    /// Whether both are the same part.
    fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(one), Some(other)) => Arc::ptr_eq(one, other),
            _ => false,
        }
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0.as_ref().expect("held until dropped").value
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // The last holder: its box freed by `into_inner`, then the value, then the charge.
        if let Some(boxed) = self.0.take().and_then(Arc::into_inner) {
            drop(boxed);
        }
    }
}

/// Why a reader's view can't be given: the read fails with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The memory it needs is past a budget: the store's support budget (what is kept)
    /// or the reading query's (what computing it reads and builds).
    Memory(BudgetExceeded),
    /// The process held more than its memory limit.
    ProcessMemory,
    /// The reading query was cancelled, or its time ran out, while the view was computed.
    Cancelled,
}

impl From<BudgetExceeded> for Unavailable {
    fn from(exceeded: BudgetExceeded) -> Self {
        Self::Memory(exceeded)
    }
}

impl From<Unavailable> for crate::StoreError {
    fn from(unavailable: Unavailable) -> Self {
        match unavailable {
            Unavailable::Memory(exceeded) => exceeded.into(),
            Unavailable::ProcessMemory => crate::StoreError::ProcessMemoryLimit {
                limit: nrese_exec::memory::process_limit().unwrap_or(0),
            },
            Unavailable::Cancelled => nrese_sparql::QueryEvaluationError::Cancelled.into(),
        }
    }
}

/// What computing a view reads and builds is charged to (a part of the reading query's
/// envelope), and the reading query's cancellation (its time running out cancels it too),
/// which the evaluations poll at their rounds ([`Following`]).
struct Work {
    budget: Arc<Budget>,
    cancellation: Option<CancellationToken>,
}

impl Work {
    /// Fails once the reading query is cancelled: what polls it stops there, before it
    /// charges or keeps more.
    fn go_on(&self) -> Result<(), Unavailable> {
        match self
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            true => Err(Unavailable::Cancelled),
            false => Ok(()),
        }
    }
}

impl std::ops::Deref for Work {
    type Target = Arc<Budget>;

    fn deref(&self) -> &Arc<Budget> {
        &self.budget
    }
}

/// An evaluation's working set, followed on the reading query's budget at its rounds
/// ([`nrese_reasoner::graph_sets::Watch`]): what it reports is charged, a refusal kept for
/// the read to fail with; and the query's cancellation, which stops it there as well. The
/// charge is released when it drops.
struct Following {
    held: Mutex<Reservation<Arc<Budget>>>,
    refused: Mutex<Option<BudgetExceeded>>,
    cancellation: Option<CancellationToken>,
}

impl Following {
    fn new(working: &Work) -> Self {
        Self {
            held: Mutex::new(Reservation::new(Arc::clone(&working.budget))),
            refused: Mutex::new(None),
            cancellation: working.cancellation.clone(),
        }
    }

    fn watch(&self, bytes: usize) -> bool {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return false;
        }
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        match held.resize(bytes) {
            Ok(()) => true,
            Err(exceeded) => {
                *self.refused.lock().unwrap_or_else(|p| p.into_inner()) = Some(exceeded);
                false
            }
        }
    }

    fn failure(&self, stopped: Stopped) -> Unavailable {
        match stopped {
            Stopped::ProcessMemory => Unavailable::ProcessMemory,
            // The watch refused: the budget, or else the cancellation.
            Stopped::Memory { .. } => self
                .refused
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .map_or(Unavailable::Cancelled, Unavailable::Memory),
        }
    }
}

/// How a reader's kept sets decide a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Shown,
    Hidden,
    /// No kept set fits, but the cap left some out: the reader's closure decides.
    Open,
}

/// The distinct sets, by index, and the indices in the sets' order to find one by.
#[derive(Debug)]
struct Interned {
    sets: Kept<GraphSet>,
    order: Kept<u32>,
    /// The words of sets past 128 graphs. Last: released after the sets are freed.
    words: Reservation<Arc<Budget>>,
}

impl Interned {
    /// `distinct` (sorted, each once), indexed in that order.
    fn of(distinct: &[&GraphSet], kept: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let mut words = Reservation::new(Arc::clone(kept));
        words.charge(distinct.iter().map(|set| set.heap_bytes()).sum())?;
        let mut sets = ChargedVec::with_capacity(Arc::clone(kept), distinct.len())?;
        let mut order = ChargedVec::with_capacity(Arc::clone(kept), distinct.len())?;
        for (i, &set) in distinct.iter().enumerate() {
            sets.push(set.clone())?;
            order.push(i as u32)?;
        }
        Ok(Self { sets, order, words })
    }

    fn find(&self, set: &GraphSet) -> Option<u32> {
        self.order
            .binary_search_by(|&i| self.sets[i as usize].cmp(set))
            .ok()
            .map(|k| self.order[k])
    }

    /// These sets and `new` (sorted, each once, none of them held), the new ones indexed
    /// after these.
    fn with(&self, new: &[&GraphSet], kept: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let mut words = Reservation::new(Arc::clone(kept));
        words.charge(self.words.bytes() + new.iter().map(|set| set.heap_bytes()).sum::<usize>())?;
        let len = self.sets.len() + new.len();
        let mut sets = ChargedVec::with_capacity(Arc::clone(kept), len)?;
        sets.extend_from_slice(&self.sets)?;
        for &set in new {
            sets.push(set.clone())?;
        }
        // The old order and the new indices, merged by set.
        let mut order = ChargedVec::with_capacity(Arc::clone(kept), len)?;
        let (mut old, mut added) = (self.order.iter().copied().peekable(), 0usize);
        while old.peek().is_some() || added < new.len() {
            let take_new = match old.peek() {
                Some(&i) => added < new.len() && *new[added] < self.sets[i as usize],
                None => true,
            };
            match take_new {
                true => {
                    order.push((self.sets.len() + added) as u32)?;
                    added += 1;
                }
                false => order.push(old.next().expect("peeked"))?,
            }
        }
        Ok(Self { sets, order, words })
    }
}

/// Facts recomputed since the base was built, sorted, each with its set indices ([`OWN`]
/// alone: it has the sets it starts with); they replace their entries in the base.
#[derive(Debug)]
struct Overlay {
    entries: Kept<(Triple, u32)>,
    /// Its facts whose sets the cap cut, sorted.
    cut: Kept<Triple>,
    /// Its facts also asserted, sorted ([`SupportSets::elsewhere`]).
    elsewhere: Kept<Triple>,
}

impl Overlay {
    fn empty(kept: &Arc<Budget>) -> Self {
        Self {
            entries: ChargedVec::new(Arc::clone(kept)),
            cut: ChargedVec::new(Arc::clone(kept)),
            elsewhere: ChargedVec::new(Arc::clone(kept)),
        }
    }
}

/// What a fact's sets are recorded as.
enum Record<'a> {
    /// Those it starts with, from its graphs.
    Own,
    /// These entries (its set indices), and whether the cap cut its sets.
    Kept(&'a [(Triple, u32)], bool),
}

/// The support graph sets of one revision's facts.
#[derive(Debug)]
pub struct SupportSets {
    /// The graph of each set member: the default graph, then the named graphs in IRI
    /// order (later ones appended as they appear).
    graphs: Shared<Kept<TermId>>,
    /// The distinct sets.
    sets: Shared<Interned>,
    /// The facts whose sets aren't just those they start with (an inferred statement's,
    /// an asserted one's also derived from other graphs), each with one of its sets (an
    /// index into `sets`), sorted: a fact's sets are adjacent.
    base: Shared<Kept<(Triple, u32)>>,
    /// The facts of `base` whose sets the cap cut, sorted.
    cut: Shared<Kept<Triple>>,
    /// The facts of `base` also asserted, in some graph, sorted. The stack holds no
    /// inferred copy of them (an inferred statement is suppressed while it is asserted
    /// anywhere), so a reader who may read none of the graphs that assert one sees it only
    /// where its view adds it ([`Self::split`]).
    elsewhere: Shared<Kept<Triple>>,
    /// Facts recomputed since `base` was built.
    overlay: Shared<Overlay>,
    /// The ruleset's axioms, sorted: their set is the empty one.
    axioms: Shared<Kept<Triple>>,
    /// The schema facts the ground program was grounded on, sorted: a change of them
    /// changes the program, and the sets are computed afresh.
    schema: Shared<Kept<Triple>>,
    /// Where these sets were updated from: that revision, and the facts (sorted) whose
    /// sets or presence may differ from it.
    since: Option<(u64, Shared<Kept<Triple>>)>,
}

/// The facts of a revision's closure, read for an evaluation, charged to the reading
/// query's budget.
struct Closure {
    /// Each asserted statement with the index of a graph that holds it, sorted.
    asserted: Kept<(Triple, u32)>,
    /// The asserted, inferred and axiomatic facts, sorted, each once.
    facts: Kept<Triple>,
}

impl Closure {
    /// `snapshot`'s facts, graphs numbered by `graphs`, with the `axioms` (sorted).
    fn of(
        snapshot: &Snapshot,
        graphs: &[TermId],
        axioms: &[Triple],
        working: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let index = index_of(graphs, working)?;
        let quads = sorted_quads(snapshot, ReadModel::Asserted, working)?;
        let mut asserted: Kept<(Triple, u32)> =
            ChargedVec::with_capacity(Arc::clone(working), quads.len())?;
        for quad in quads.iter() {
            asserted.push((triple(*quad), graph_index(&index, quad.graph)))?;
        }
        drop((quads, index));
        asserted.sort_unstable();
        let inferred = sorted_quads(snapshot, ReadModel::Inferred, working)?;
        let mut facts: Kept<Triple> = ChargedVec::with_capacity(
            Arc::clone(working),
            asserted.len() + inferred.len() + axioms.len(),
        )?;
        for &(fact, _) in asserted.iter() {
            facts.push(fact)?;
        }
        for quad in inferred.iter() {
            facts.push(triple(*quad))?;
        }
        drop(inferred);
        facts.extend_from_slice(axioms)?;
        facts.sort_unstable();
        dedup(&mut facts);
        Ok(Self { asserted, facts })
    }

    /// The graphs that hold `fact`, ascending.
    fn graphs_of(&self, fact: Triple) -> &[(Triple, u32)] {
        in_order(&self.asserted, fact)
    }
}

impl SupportSets {
    /// The sets of `snapshot`'s facts under `program`, at most `cap` per fact, kept on
    /// `kept`; what computing them reads and builds charged to `working`.
    fn compute(
        program: &Program,
        snapshot: &Snapshot,
        cap: usize,
        kept: &Arc<Budget>,
        working: &Work,
    ) -> Result<Self, Unavailable> {
        let graphs = graphs_in_order(snapshot, &[TermId::DEFAULT_GRAPH], kept, working)?;
        let mut axioms: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(working), program.axioms().len())?;
        axioms.extend_from_slice(program.axioms())?;
        axioms.sort_unstable();
        let closure = Closure::of(snapshot, &graphs, &axioms, working)?;
        let graphs_of = |fact: Triple| -> Vec<u32> {
            closure.graphs_of(fact).iter().map(|&(_, g)| g).collect()
        };
        let ground = program.ground_program(snapshot);
        let following = Following::new(working);
        let by_fact = support_sets(
            &closure.facts,
            &ground,
            &axioms,
            &graphs_of,
            cap,
            &|bytes| following.watch(bytes),
        )
        .map_err(|stopped| following.failure(stopped))?;
        let schema = ground.schema_premises();
        drop(ground);
        Ok(Self::retain(
            graphs,
            &by_fact,
            &closure.asserted,
            &axioms,
            &schema,
            cap,
            kept,
        )?)
    }

    /// The sets of an evaluation (`by_fact`, every fact of a closure under a program
    /// grounded on `schema`, at most `cap` per fact) kept for readers: those of the facts
    /// whose sets aren't just those they start with, from their graphs in `asserted`
    /// (each asserted statement with a graph index of `graphs`, sorted). Everything kept is
    /// charged to `kept` before it is allocated.
    pub fn retain(
        graphs: Kept<TermId>,
        by_fact: &HashMap<Triple, Annotation>,
        asserted: &[(Triple, u32)],
        axioms: &[Triple],
        schema: &HashSet<Triple>,
        cap: usize,
        kept: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let starts = |fact: Triple, found: &Annotation| {
            let graphs = in_order(asserted, fact);
            let own = |graph: u32| graphs.binary_search_by_key(&graph, |&(_, g)| g).is_ok();
            starts_with(
                found,
                axioms.binary_search(&fact).is_ok(),
                graphs.len(),
                &own,
                cap,
            )
        };
        let asserts = |fact: Triple| !in_order(asserted, fact).is_empty();
        // Each fact's record decided once, in the map's order (the map is unchanged by the
        // passes below, so they meet its facts in the same order): 0 not kept (its sets are
        // those it starts with), 1 kept, 2 kept and asserted.
        let mut records: Kept<u8> = ChargedVec::with_capacity(Arc::clone(kept), by_fact.len())?;
        let (mut entries, mut marked, mut also) = (0, 0, 0);
        for (&fact, found) in by_fact {
            let record = match !found.is_empty() && !starts(fact, found) {
                true => 1 + u8::from(asserts(fact)),
                false => 0,
            };
            if record > 0 {
                entries += found.sets.len();
                marked += usize::from(found.truncated);
                also += usize::from(record == 2);
            }
            records.push(record)?;
        }
        let recorded = || {
            by_fact
                .iter()
                .zip(records.iter())
                .filter(|&(_, &record)| record > 0)
                .map(|((fact, found), &record)| (*fact, found, record == 2))
        };
        // The distinct sets, in their order.
        let mut distinct: ChargedVec<&GraphSet, Arc<Budget>> =
            ChargedVec::with_capacity(Arc::clone(kept), entries)?;
        for (_, found, _) in recorded() {
            for set in &found.sets {
                distinct.push(set)?;
            }
        }
        distinct.sort_unstable();
        dedup(&mut distinct);
        let sets = Interned::of(&distinct, kept)?;
        drop(distinct);
        let mut base: Kept<(Triple, u32)> = ChargedVec::with_capacity(Arc::clone(kept), entries)?;
        let mut cut: Kept<Triple> = ChargedVec::with_capacity(Arc::clone(kept), marked)?;
        let mut elsewhere: Kept<Triple> = ChargedVec::with_capacity(Arc::clone(kept), also)?;
        for (fact, found, asserted) in recorded() {
            for set in &found.sets {
                base.push((fact, sets.find(set).expect("interned")))?;
            }
            if found.truncated {
                cut.push(fact)?;
            }
            if asserted {
                elsewhere.push(fact)?;
            }
        }
        drop(records);
        base.sort_unstable();
        cut.sort_unstable();
        elsewhere.sort_unstable();
        let mut kept_axioms: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(kept), axioms.len())?;
        kept_axioms.extend_from_slice(axioms)?;
        let mut kept_schema: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(kept), schema.len())?;
        for &fact in schema {
            kept_schema.push(fact)?;
        }
        kept_schema.sort_unstable();
        Ok(Self {
            graphs: Shared::new(graphs, kept)?,
            sets: Shared::new(sets, kept)?,
            base: Shared::new(base, kept)?,
            cut: Shared::new(cut, kept)?,
            elsewhere: Shared::new(elsewhere, kept)?,
            overlay: Shared::new(Overlay::empty(kept), kept)?,
            axioms: Shared::new(kept_axioms, kept)?,
            schema: Shared::new(kept_schema, kept)?,
            since: None,
        })
    }

    /// These sets, of revision `from`, brought to `snapshot`, whose changes since
    /// touched the facts `touched` (sorted: their graphs, or whether they are in the
    /// closure). `Ok(None)` where the update can't serve: the schema changed, or the change
    /// reaches too many facts.
    #[allow(clippy::too_many_arguments)]
    fn updated(
        &self,
        program: &Program,
        snapshot: &Snapshot,
        from: u64,
        touched: &[Triple],
        cap: usize,
        kept: &Arc<Budget>,
        working: &Work,
    ) -> Result<Option<Self>, Unavailable> {
        let ground = program.ground_program(snapshot);
        let schema = ground.schema_premises();
        if schema.len() != self.schema.len()
            || !self.schema.iter().all(|fact| schema.contains(fact))
            || program.axioms().len() != self.axioms.len()
            || !program
                .axioms()
                .iter()
                .all(|fact| self.axioms.binary_search(fact).is_ok())
        {
            return Ok(None);
        }
        let graphs = graphs_in_order(snapshot, &self.graphs, kept, working)?;
        let index = index_of(&graphs, working)?;
        let graphs_of = |fact: Triple| -> Vec<u32> {
            let mut found: Vec<u32> = snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &pattern(fact, GraphSelector::Any))
                .map(|quad| graph_index(&index, quad.graph))
                .collect();
            found.sort_unstable();
            found.dedup();
            found
        };
        let in_closure = |fact: Triple| {
            snapshot.exists_in(ReadModel::Materialised, &pattern(fact, GraphSelector::Any))
                || self.axioms.binary_search(&fact).is_ok()
        };
        let mut removed: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(working), touched.len())?;
        for &fact in touched {
            if !in_closure(fact) {
                removed.push(fact)?;
            }
        }
        let source = After {
            snapshot,
            removed: &removed,
        };
        let previous = |fact: Triple| -> Annotation {
            match self.record(fact) {
                // The record keeps one mark: the second taken to be as wide.
                Record::Kept(entries, truncated) => Annotation {
                    sets: entries
                        .iter()
                        .map(|&(_, id)| self.sets.sets[id as usize].clone())
                        .collect(),
                    truncated,
                    beyond_own: truncated,
                },
                Record::Own => self.initial(fact, graphs_of(fact), cap),
            }
        };
        let change = Change {
            source: &source,
            in_closure: &in_closure,
            touched,
            previous: &previous,
            limit: self.base.len() / 4 + 10_000,
        };
        let following = Following::new(working);
        let recomputed =
            match update_support_sets(&change, &ground, &self.axioms, &graphs_of, cap, &|bytes| {
                following.watch(bytes)
            }) {
                Ok(Some(recomputed)) => recomputed,
                Ok(None) => return Ok(None),
                Err(stopped) => return Err(following.failure(stopped)),
            };
        drop(ground);
        // Whether a recomputed fact has only the sets it starts with, from its graphs as
        // `snapshot` holds them (one read at a time, its working memory reserved).
        let mut scanning = Reservation::new(Arc::clone(working));
        scanning.charge(snapshot.scan_bytes(ReadModel::Asserted))?;
        let starts = |fact: Triple, found: &Annotation| -> (bool, bool) {
            let [s, p, o] = fact.map(TermId::from_raw);
            let holds = snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &pattern(fact, GraphSelector::Any))
                .count();
            if found.is_empty() {
                return (true, holds > 0);
            }
            let own = |graph: u32| {
                graphs.get(graph as usize).is_some_and(|&graph| {
                    snapshot.contains_in(ReadModel::Asserted, &EncodedQuad::new(s, p, o, graph))
                })
            };
            let own_sets = starts_with(
                found,
                self.axioms.binary_search(&fact).is_ok(),
                holds,
                &own,
                cap,
            );
            (own_sets, holds > 0)
        };
        // The facts the overlay now gives, sorted, each with whether it has only its own
        // sets (a removed fact has none) and whether it is asserted.
        let mut fresh: Kept<(Triple, (bool, bool))> =
            ChargedVec::with_capacity(Arc::clone(working), recomputed.len() + removed.len())?;
        for (&fact, found) in &recomputed {
            fresh.push((fact, starts(fact, found)))?;
        }
        for &fact in removed.iter() {
            if !recomputed.contains_key(&fact) {
                fresh.push((fact, (true, false)))?;
            }
        }
        drop(scanning);
        fresh.sort_unstable();
        let own = |fact: &Triple| {
            in_order(&fresh, *fact)
                .first()
                .is_some_and(|&(_, (own, _))| own)
        };
        // New sets.
        let mut new: ChargedVec<&GraphSet, Arc<Budget>> = ChargedVec::new(Arc::clone(working));
        for (fact, found) in &recomputed {
            if own(fact) {
                continue;
            }
            for set in &found.sets {
                if self.sets.find(set).is_none() {
                    new.push(set)?;
                }
            }
        }
        new.sort_unstable();
        dedup(&mut new);
        let sets = match new.is_empty() {
            true => self.sets.clone(),
            false => Shared::new(self.sets.with(&new, kept)?, kept)?,
        };
        drop(new);
        // The overlay: the old one's entries for facts not given again, then the new ones.
        let given = |fact: &Triple| !in_order(&fresh, *fact).is_empty();
        let sets_of = |&(fact, (own, _)): &(Triple, (bool, bool))| match own {
            true => None,
            false => recomputed.get(&fact),
        };
        let entries_len = self
            .overlay
            .entries
            .iter()
            .filter(|(fact, _)| !given(fact))
            .count()
            + fresh
                .iter()
                .map(|entry| sets_of(entry).map_or(1, |found| found.sets.len()))
                .sum::<usize>();
        let cut_len = self.overlay.cut.iter().filter(|fact| !given(fact)).count()
            + fresh
                .iter()
                .filter(|entry| sets_of(entry).is_some_and(|found| found.truncated))
                .count();
        let mut entries: Kept<(Triple, u32)> =
            ChargedVec::with_capacity(Arc::clone(kept), entries_len)?;
        let mut cut: Kept<Triple> = ChargedVec::with_capacity(Arc::clone(kept), cut_len)?;
        let elsewhere_len = self
            .overlay
            .elsewhere
            .iter()
            .filter(|fact| !given(fact))
            .count()
            + fresh
                .iter()
                .filter(|entry| sets_of(entry).is_some() && entry.1.1)
                .count();
        let mut elsewhere: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(kept), elsewhere_len)?;
        for &fact in self.overlay.elsewhere.iter() {
            if !given(&fact) {
                elsewhere.push(fact)?;
            }
        }
        for &(fact, id) in self.overlay.entries.iter() {
            if !given(&fact) {
                entries.push((fact, id))?;
            }
        }
        for &fact in self.overlay.cut.iter() {
            if !given(&fact) {
                cut.push(fact)?;
            }
        }
        for entry in fresh.iter() {
            let fact = entry.0;
            match sets_of(entry) {
                Some(found) => {
                    for set in &found.sets {
                        entries.push((fact, sets.find(set).expect("interned")))?;
                    }
                    if found.truncated {
                        cut.push(fact)?;
                    }
                    if entry.1.1 {
                        elsewhere.push(fact)?;
                    }
                }
                None => entries.push((fact, OWN))?,
            }
        }
        entries.sort_unstable();
        cut.sort_unstable();
        elsewhere.sort_unstable();
        let mut changed: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(kept), touched.len() + fresh.len())?;
        changed.extend_from_slice(touched)?;
        for &(fact, _) in fresh.iter() {
            changed.push(fact)?;
        }
        changed.sort_unstable();
        dedup(&mut changed);
        let mut updated = Self {
            graphs: Shared::new(graphs, kept)?,
            sets,
            base: self.base.clone(),
            cut: self.cut.clone(),
            elsewhere: self.elsewhere.clone(),
            overlay: Shared::new(
                Overlay {
                    entries,
                    cut,
                    elsewhere,
                },
                kept,
            )?,
            axioms: self.axioms.clone(),
            schema: self.schema.clone(),
            since: Some((from, Shared::new(changed, kept)?)),
        };
        if updated.overlay.entries.len() > MAX_OVERLAY.max(updated.base.len() / 16) {
            updated.fold(kept)?;
        }
        Ok(Some(updated))
    }

    /// The overlay folded into the base: both merged in order, the base's entries of the
    /// overlay's facts left out.
    fn fold(&mut self, kept: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        let overlay = &self.overlay;
        let mut overlaid = overlay.entries.iter().map(|&(fact, _)| fact).peekable();
        let mut in_overlay = |fact: &Triple| {
            while overlaid.next_if(|f| f < fact).is_some() {}
            overlaid.peek() == Some(fact)
        };
        let kept_base = self
            .base
            .iter()
            .copied()
            .filter(|(fact, _)| !in_overlay(fact));
        let on_top = overlay.entries.iter().copied().filter(|&(_, id)| id != OWN);
        let mut base: Kept<(Triple, u32)> =
            ChargedVec::with_capacity(Arc::clone(kept), self.base.len() + overlay.entries.len())?;
        merge_sorted(kept_base, on_top, &mut base)?;
        let mut overlaid = overlay.entries.iter().map(|&(fact, _)| fact).peekable();
        let mut in_overlay = |fact: &Triple| {
            while overlaid.next_if(|f| f < fact).is_some() {}
            overlaid.peek() == Some(fact)
        };
        let kept_cut = self.cut.iter().copied().filter(|fact| !in_overlay(fact));
        let mut cut: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(kept), self.cut.len() + overlay.cut.len())?;
        merge_sorted(kept_cut, overlay.cut.iter().copied(), &mut cut)?;
        let mut overlaid = overlay.entries.iter().map(|&(fact, _)| fact).peekable();
        let mut in_overlay = |fact: &Triple| {
            while overlaid.next_if(|f| f < fact).is_some() {}
            overlaid.peek() == Some(fact)
        };
        let kept_elsewhere = self
            .elsewhere
            .iter()
            .copied()
            .filter(|fact| !in_overlay(fact));
        let mut elsewhere: Kept<Triple> = ChargedVec::with_capacity(
            Arc::clone(kept),
            self.elsewhere.len() + overlay.elsewhere.len(),
        )?;
        merge_sorted(
            kept_elsewhere,
            overlay.elsewhere.iter().copied(),
            &mut elsewhere,
        )?;
        self.base = Shared::new(base, kept)?;
        self.cut = Shared::new(cut, kept)?;
        self.elsewhere = Shared::new(elsewhere, kept)?;
        self.overlay = Shared::new(Overlay::empty(kept), kept)?;
        Ok(())
    }

    /// The sets `fact` starts with, from its graphs.
    fn initial(&self, fact: Triple, graphs: Vec<u32>, cap: usize) -> Annotation {
        let axiom = self.axioms.binary_search(&fact).is_ok();
        initial_sets(if axiom { Vec::new() } else { graphs }, axiom, cap)
    }

    /// What `fact`'s sets are recorded as.
    fn record(&self, fact: Triple) -> Record<'_> {
        let overlaid = in_order(&self.overlay.entries, fact);
        if let Some(&(_, id)) = overlaid.first() {
            return match id {
                OWN => Record::Own,
                _ => Record::Kept(overlaid, self.overlay.cut.binary_search(&fact).is_ok()),
            };
        }
        match in_order(&self.base, fact) {
            [] => Record::Own,
            entries => Record::Kept(entries, self.cut.binary_search(&fact).is_ok()),
        }
    }

    /// How the kept sets decide the inferred statement `fact` for a reader for whom
    /// `visible_sets` tells which sets lie in its graphs. An axiom without entries holds
    /// for everyone; another statement without them has no derivation the sets know of.
    fn decide(&self, fact: Triple, visible_sets: &[bool]) -> Decision {
        match self.record(fact) {
            Record::Own if self.axioms.binary_search(&fact).is_ok() => Decision::Shown,
            Record::Own => Decision::Hidden,
            Record::Kept(entries, _)
                if entries.iter().any(|&(_, id)| visible_sets[id as usize]) =>
            {
                Decision::Shown
            }
            Record::Kept(_, true) => Decision::Open,
            Record::Kept(_, false) => Decision::Hidden,
        }
    }

    /// Which sets lie in the graphs `access` reads.
    fn visible_sets(
        &self,
        snapshot: &Snapshot,
        access: &GraphAccess,
        working: &Arc<Budget>,
    ) -> Result<Kept<bool>, BudgetExceeded> {
        let mut readable: Kept<bool> =
            ChargedVec::with_capacity(Arc::clone(working), self.graphs.len())?;
        for &graph in self.graphs.iter() {
            readable.push(access.allows_id(snapshot, graph))?;
        }
        let mut visible: Kept<bool> =
            ChargedVec::with_capacity(Arc::clone(working), self.sets.sets.len())?;
        for set in self.sets.sets.iter() {
            visible.push(set.graphs().all(|graph| readable[graph as usize]))?;
        }
        Ok(visible)
    }

    /// The facts that may join a reader's view though the stack holds no copy of them:
    /// those of `elsewhere` (and of the ruleset's axioms) asserted only in graphs the
    /// reader with `access` may not read, sorted. Its scans' working memory on `working`.
    fn asserted_elsewhere(
        &self,
        snapshot: &Snapshot,
        access: &GraphAccess,
        working: &Arc<Budget>,
    ) -> Result<Kept<Triple>, BudgetExceeded> {
        let overlaid = |fact: &Triple| !in_order(&self.overlay.entries, *fact).is_empty();
        let bound = self.elsewhere.len() + self.overlay.elsewhere.len() + self.axioms.len();
        let mut found: Kept<Triple> = ChargedVec::with_capacity(Arc::clone(working), bound)?;
        let mut scanning = Reservation::new(Arc::clone(working));
        scanning.charge(snapshot.scan_bytes(ReadModel::Asserted))?;
        let candidates = self
            .elsewhere
            .iter()
            .filter(|fact| !overlaid(fact))
            .chain(self.overlay.elsewhere.iter())
            .chain(self.axioms.iter());
        for &fact in candidates {
            if asserted_out_of_reach(snapshot, access, fact) {
                found.push(fact)?;
            }
        }
        drop(scanning);
        found.sort_unstable();
        dedup(&mut found);
        Ok(found)
    }

    /// The inferred statements of `snapshot` (the revision the sets are of) that a reader
    /// with `access` sees, as the smaller side of the split, sorted: decided by the kept
    /// sets, and where they leave a statement open, by `within` (the reader's closure,
    /// sorted), asked once and only then. They include, as inferred, the facts asserted
    /// only in graphs the reader can't read that its graphs derive (the stack has no copy of
    /// them: an inferred statement is suppressed while it is asserted anywhere), so what the
    /// reader sees doesn't depend on what such a graph asserts. The side kept is charged to
    /// `kept`, the rest to `working`, whose query's cancellation stops it between its scans
    /// and while it decides.
    fn split(
        &self,
        snapshot: &Snapshot,
        access: &GraphAccess,
        kept: &Arc<Budget>,
        working: &Work,
        within: impl FnOnce() -> Result<Kept<Triple>, Unavailable>,
    ) -> Result<Split, Unavailable> {
        let visible_sets = self.visible_sets(snapshot, access, working)?;
        let quads = sorted_quads(snapshot, ReadModel::Inferred, working)?;
        working.go_on()?;
        let mut decisions: Kept<Decision> =
            ChargedVec::with_capacity(Arc::clone(working), quads.len())?;
        for (i, quad) in quads.iter().enumerate() {
            if i.is_multiple_of(4096) {
                working.go_on()?;
            }
            decisions.push(self.decide(triple(*quad), &visible_sets))?;
        }
        let elsewhere = self.asserted_elsewhere(snapshot, access, working)?;
        working.go_on()?;
        let mut derived: Kept<Decision> =
            ChargedVec::with_capacity(Arc::clone(working), elsewhere.len())?;
        for &fact in elsewhere.iter() {
            derived.push(self.decide(fact, &visible_sets))?;
        }
        drop(visible_sets);
        if decisions.contains(&Decision::Open) || derived.contains(&Decision::Open) {
            let held = within()?;
            let close = |decision: &mut Decision, fact: Triple| {
                if *decision == Decision::Open {
                    *decision = match held.binary_search(&fact).is_ok() {
                        true => Decision::Shown,
                        false => Decision::Hidden,
                    };
                }
            };
            for (decision, quad) in decisions.iter_mut().zip(quads.iter()) {
                close(decision, triple(*quad));
            }
            for (decision, &fact) in derived.iter_mut().zip(elsewhere.iter()) {
                close(decision, fact);
            }
        }
        let in_view = |fact: Triple| {
            let [s, p, o] = fact.map(TermId::from_raw);
            EncodedQuad::new(s, p, o, TermId::DEFAULT_GRAPH)
        };
        let added = derived.iter().filter(|&&d| d == Decision::Shown).count();
        let mut show: Kept<EncodedQuad> = ChargedVec::with_capacity(Arc::clone(working), added)?;
        for (decision, &fact) in derived.iter().zip(elsewhere.iter()) {
            if *decision == Decision::Shown {
                show.push(in_view(fact))?;
            }
        }
        drop((derived, elsewhere));
        let shown = decisions.iter().filter(|&&d| d == Decision::Shown).count();
        let hidden = quads.len() - shown;
        let stack_side = |shown_side: bool, list: &mut Kept<EncodedQuad>| {
            for (decision, quad) in decisions.iter().zip(quads.iter()) {
                if (*decision == Decision::Shown) == shown_side {
                    list.push(*quad)?;
                }
            }
            Ok::<_, BudgetExceeded>(())
        };
        if shown <= hidden {
            // What it sees, the derived extras among them, in order.
            let mut stack: Kept<EncodedQuad> =
                ChargedVec::with_capacity(Arc::clone(working), shown)?;
            stack_side(true, &mut stack)?;
            let mut list: Kept<EncodedQuad> =
                ChargedVec::with_capacity(Arc::clone(kept), shown + added)?;
            merge_sorted(stack.iter().copied(), show.iter().copied(), &mut list)?;
            return Ok(Split::Only(list));
        }
        let mut hide: Kept<EncodedQuad> = ChargedVec::with_capacity(Arc::clone(working), hidden)?;
        stack_side(false, &mut hide)?;
        Ok(Split::Mask { hide, show })
    }

    /// For a reader with `access`, how the statements of the facts that changed since
    /// `earlier` (the sets these were updated from, of the snapshot `before`) stand now.
    /// `None` if these sets weren't updated from `earlier`'s revision, if the kept sets
    /// leave one of those statements open, or if one of the facts is asserted in a graph
    /// the reader can't read and the sets don't hide it from the reader, before or now
    /// (the view is then made afresh).
    fn changes_since(
        &self,
        snapshot: &Snapshot,
        earlier: &SupportSets,
        before: &Snapshot,
        access: &GraphAccess,
        working: &Arc<Budget>,
    ) -> Result<Option<Changes>, BudgetExceeded> {
        let Some((from, changed)) = self.since.as_ref() else {
            return Ok(None);
        };
        if *from != before.revision() {
            return Ok(None);
        }
        let visible_now = self.visible_sets(snapshot, access, working)?;
        let visible_before = earlier.visible_sets(before, access, working)?;
        // A changed fact asserted only in graphs the reader can't read, before or now, joins
        // or leaves the view as a derived extra where the sets don't hide it
        // ([`Self::split`]): the view is then made afresh. Hidden by both, or asserted in a
        // graph the reader reads both times, it is no extra of either view.
        let mut scanning = Reservation::new(Arc::clone(working));
        scanning.charge(snapshot.scan_bytes(ReadModel::Asserted))?;
        let extra = |fact: Triple| {
            (earlier.decide(fact, &visible_before) != Decision::Hidden
                || self.decide(fact, &visible_now) != Decision::Hidden)
                && (asserted_out_of_reach(before, access, fact)
                    || asserted_out_of_reach(snapshot, access, fact))
        };
        if changed.iter().any(|&fact| extra(fact)) {
            return Ok(None);
        }
        drop(scanning);
        let mut changes = Changes {
            changed: changed.clone(),
            visible: ChargedVec::new(Arc::clone(working)),
            hide: ChargedVec::new(Arc::clone(working)),
            show: ChargedVec::new(Arc::clone(working)),
        };
        for &fact in changed.iter() {
            let [s, p, o] = fact.map(TermId::from_raw);
            let quad = EncodedQuad::new(s, p, o, TermId::DEFAULT_GRAPH);
            let decided = |sets: &SupportSets, visible: &[bool], present: bool| match present {
                true => Some(sets.decide(fact, visible)).filter(|d| *d != Decision::Open),
                false => Some(Decision::Hidden),
            };
            let (Some(was), Some(is)) = (
                decided(
                    earlier,
                    &visible_before,
                    before.contains_in(ReadModel::Inferred, &quad),
                ),
                decided(
                    self,
                    &visible_now,
                    snapshot.contains_in(ReadModel::Inferred, &quad),
                ),
            ) else {
                return Ok(None);
            };
            let present = snapshot.contains_in(ReadModel::Inferred, &quad);
            let hidden_before =
                before.contains_in(ReadModel::Inferred, &quad) && was == Decision::Hidden;
            let visible = present && is == Decision::Shown;
            let hidden = present && !visible;
            if visible {
                changes.visible.push(quad)?;
            }
            match (hidden_before, hidden) {
                (false, true) => changes.hide.push(quad)?,
                (true, false) => changes.show.push(quad)?,
                _ => {}
            }
        }
        Ok(Some(changes))
    }

    /// How many facts have entries, how many sets they have in all, and how many of them
    /// the cap cut (each a statement some readers are decided for by their closure).
    pub fn len(&self) -> (usize, usize, usize) {
        let overlaid = |fact: &Triple| !in_order(&self.overlay.entries, *fact).is_empty();
        let mut facts = 0;
        let mut sets = 0;
        let mut last = None;
        for (fact, _) in self.base.iter() {
            if overlaid(fact) {
                continue;
            }
            sets += 1;
            if last != Some(fact) {
                facts += 1;
                last = Some(fact);
            }
        }
        let mut last = None;
        for (fact, id) in self.overlay.entries.iter() {
            if *id == OWN {
                continue;
            }
            sets += 1;
            if last != Some(fact) {
                facts += 1;
                last = Some(fact);
            }
        }
        let cut = self.cut.iter().filter(|fact| !overlaid(fact)).count() + self.overlay.cut.len();
        (facts, sets, cut)
    }
}

/// The inferred statements a reader sees, as the smaller side ([`SupportSets::split`]).
enum Split {
    Only(Kept<EncodedQuad>),
    /// The stack's statements it misses (`hide`), and the extras it sees that the stack
    /// doesn't hold (`show`).
    Mask {
        hide: Kept<EncodedQuad>,
        show: Kept<EncodedQuad>,
    },
}

/// How the statements of changed facts stand for a reader, against an earlier revision
/// ([`SupportSets::changes_since`]); each list sorted.
#[derive(Debug)]
struct Changes {
    /// The changed facts.
    changed: Shared<Kept<Triple>>,
    /// Their statements visible now.
    visible: Kept<EncodedQuad>,
    /// Hidden now, not before (absent, or visible).
    hide: Kept<EncodedQuad>,
    /// Hidden before, not now (visible, or gone).
    show: Kept<EncodedQuad>,
}

/// A reader's view of a revision, kept to build its view of the next one from.
#[derive(Debug)]
struct Reader {
    access: Arc<GraphAccess>,
    snapshot: Snapshot,
    sets: Shared<SupportSets>,
    form: Form,
}

/// What a reader misses: the statements it misses, as runs over the stack (when it sees
/// most), or the statements it sees (when it misses most).
#[derive(Debug)]
enum Form {
    Mask(InferredMask),
    Only(Shared<Kept<EncodedQuad>>),
}

/// Runs a mask stacks before it is built again as one.
const MASK_RUNS: usize = 8;

/// `list` (sorted statements a reader sees) without the changed facts' statements, with
/// those it sees now.
fn patched(
    list: &[EncodedQuad],
    changes: &Changes,
    kept: &Arc<Budget>,
) -> Result<Kept<EncodedQuad>, BudgetExceeded> {
    let mut next = ChargedVec::with_capacity(Arc::clone(kept), list.len() + changes.visible.len())?;
    let mut added = changes.visible.iter().copied().peekable();
    for &quad in list {
        if changes.changed.binary_search(&triple(quad)).is_ok() {
            continue;
        }
        while let Some(first) = added.next_if(|&first| first < quad) {
            next.push(first)?;
        }
        next.push(quad)?;
    }
    for quad in added {
        next.push(quad)?;
    }
    Ok(next)
}

/// Whether `found` are the sets a fact starts with and all that matter: the empty set for
/// an axiom, else one per graph that holds it (`holds` of them, at most `cap`, `own`
/// telling which), with no set beyond those graphs left out. A fact the cap cut beyond its
/// own graphs is recorded however its kept sets look: a set it left out may lie in a
/// reader's graphs ([`SupportSets::split`]). One the cap cut only among its own graphs
/// isn't: a reader who reads none of them derives it from none. Allocates nothing.
fn starts_with(
    found: &Annotation,
    axiom: bool,
    holds: usize,
    own: &dyn Fn(u32) -> bool,
    cap: usize,
) -> bool {
    if axiom {
        return !found.truncated && found.sets.len() == 1 && found.sets[0].is_empty();
    }
    !found.beyond_own
        && found.sets.len() == holds.min(cap)
        && found.sets.iter().all(|set| {
            let mut members = set.graphs();
            matches!((members.next(), members.next()), (Some(graph), None) if own(graph))
        })
}

/// Whether `fact` is asserted in `snapshot`, in no graph a reader with `access` may read.
fn asserted_out_of_reach(snapshot: &Snapshot, access: &GraphAccess, fact: Triple) -> bool {
    let mut asserted = false;
    for quad in
        snapshot.quads_for_pattern_in(ReadModel::Asserted, &pattern(fact, GraphSelector::Any))
    {
        if access.allows_id(snapshot, quad.graph) {
            return false;
        }
        asserted = true;
    }
    asserted
}

/// `known` followed by the named graphs of `snapshot` it lacks, in IRI order, kept on
/// `kept`; their names, to sort them by, read on `working`.
fn graphs_in_order(
    snapshot: &Snapshot,
    known: &[TermId],
    kept: &Arc<Budget>,
    working: &Arc<Budget>,
) -> Result<Kept<TermId>, BudgetExceeded> {
    let mut new: ChargedVec<(String, TermId), Arc<Budget>> = ChargedVec::new(Arc::clone(working));
    let mut names = Reservation::new(Arc::clone(working));
    for graph in snapshot.named_graphs() {
        if known.contains(&graph) {
            continue;
        }
        // The decoded term, then its text: each within twice the bound (a string grows by
        // doubling), with the IRI's brackets.
        names.charge(4 * snapshot.decode_bound(graph).unwrap_or(0) + 64)?;
        let name = snapshot
            .decode(graph)
            .map(|term| term.to_string())
            .unwrap_or_default();
        new.push((name, graph))?;
    }
    new.sort_unstable();
    let mut graphs = ChargedVec::with_capacity(Arc::clone(kept), known.len() + new.len())?;
    graphs.extend_from_slice(known)?;
    for (_, graph) in new.iter() {
        graphs.push(*graph)?;
    }
    drop(new);
    drop(names);
    Ok(graphs)
}

/// The closure after a change, and the facts it removed (an update's joins reach what
/// was derived from them).
struct After<'a> {
    snapshot: &'a Snapshot,
    /// Sorted.
    removed: &'a [Triple],
}

impl After<'_> {
    fn removed_matching(&self, bound: [Option<u64>; 3]) -> impl Iterator<Item = Triple> + '_ {
        self.removed.iter().copied().filter(move |fact| {
            bound
                .iter()
                .zip(fact)
                .all(|(bound, term)| bound.is_none_or(|b| b == *term))
        })
    }
}

impl Source for After<'_> {
    fn scan(&self, bound: [Option<u64>; 3], _: Seg, f: &mut dyn FnMut(Triple)) {
        for quad in self
            .snapshot
            .quads_for_pattern_in(ReadModel::Materialised, &bound_pattern(bound))
        {
            f(triple(quad));
        }
        self.removed_matching(bound).for_each(f);
    }

    fn estimate(&self, bound: [Option<u64>; 3], _: Seg) -> usize {
        let stored = self
            .snapshot
            .estimate_in(ReadModel::Materialised, &bound_pattern(bound));
        usize::try_from(stored).unwrap_or(usize::MAX) + self.removed_matching(bound).count()
    }

    fn contains(&self, fact: Triple) -> bool {
        self.removed.binary_search(&fact).is_ok()
            || self
                .snapshot
                .exists_in(ReadModel::Materialised, &pattern(fact, GraphSelector::Any))
    }
}

/// Each graph with its index, sorted by graph.
fn index_of(
    graphs: &[TermId],
    working: &Arc<Budget>,
) -> Result<Kept<(TermId, u32)>, BudgetExceeded> {
    let mut index = ChargedVec::with_capacity(Arc::clone(working), graphs.len())?;
    for (i, &graph) in graphs.iter().enumerate() {
        index.push((graph, i as u32))?;
    }
    index.sort_unstable();
    Ok(index)
}

/// The index of `graph`, which `index` holds.
fn graph_index(index: &[(TermId, u32)], graph: TermId) -> u32 {
    let at = index
        .binary_search_by_key(&graph, |&(g, _)| g)
        .expect("every graph of the snapshot is numbered");
    index[at].1
}

/// The entries of `fact` in `list` (sorted by fact).
fn in_order<T>(list: &[(Triple, T)], fact: Triple) -> &[(Triple, T)] {
    let start = list.partition_point(|(f, _)| *f < fact);
    let end = start + list[start..].partition_point(|(f, _)| *f == fact);
    &list[start..end]
}

/// The quads of `model`, sorted by subject, predicate, object, graph, charged to
/// `working` first (the list, and the scan's own working memory while it runs).
fn sorted_quads(
    snapshot: &Snapshot,
    model: ReadModel,
    working: &Arc<Budget>,
) -> Result<Kept<EncodedQuad>, BudgetExceeded> {
    let len = usize::try_from(snapshot.len_in(model)).unwrap_or(usize::MAX);
    let mut quads = ChargedVec::with_capacity(Arc::clone(working), len)?;
    let mut scanning = Reservation::new(Arc::clone(working));
    scanning.charge(snapshot.scan_bytes(model))?;
    match snapshot.scan_sorted_in(model, &QuadPattern::all(), Permutation::Spog) {
        Some(sorted) => {
            for quad in sorted {
                quads.push(quad)?;
            }
        }
        None => {
            for quad in snapshot.quads_for_pattern_in(model, &QuadPattern::all()) {
                quads.push(quad)?;
            }
            quads.sort_unstable();
        }
    }
    drop(scanning);
    Ok(quads)
}

/// `list` without its first item (the oldest kept).
fn without_first<T>(list: &mut Kept<T>) {
    let mut first = true;
    list.retain(|_| !std::mem::replace(&mut first, false));
}

/// `list` (sorted) without repeats.
fn dedup<T: PartialEq, B: std::ops::Deref<Target = Budget>>(list: &mut ChargedVec<T, B>) {
    let mut len = 0;
    for i in 0..list.len() {
        if i == 0 || list[i] != list[len - 1] {
            list.swap(len, i);
            len += 1;
        }
    }
    list.truncate(len);
}

/// `a` and `b` (each sorted) merged in order into `out`.
fn merge_sorted<T: Ord>(
    a: impl Iterator<Item = T>,
    b: impl Iterator<Item = T>,
    out: &mut Kept<T>,
) -> Result<(), BudgetExceeded> {
    let mut b = b.peekable();
    for item in a {
        while let Some(first) = b.next_if(|first| *first < item) {
            out.push(first)?;
        }
        out.push(item)?;
    }
    for item in b {
        out.push(item)?;
    }
    Ok(())
}

fn triple(quad: EncodedQuad) -> Triple {
    [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()]
}

fn pattern([s, p, o]: Triple, graph: GraphSelector) -> QuadPattern {
    QuadPattern {
        subject: Some(TermId::from_raw(s)),
        predicate: Some(TermId::from_raw(p)),
        object: Some(TermId::from_raw(o)),
        graph,
    }
}

fn bound_pattern([s, p, o]: [Option<u64>; 3]) -> QuadPattern {
    QuadPattern {
        subject: s.map(TermId::from_raw),
        predicate: p.map(TermId::from_raw),
        object: o.map(TermId::from_raw),
        graph: GraphSelector::Any,
    }
}

/// One revision's sets, computed once (or not: a failure is kept by no one).
type Cell = Shared<OnceLock<Result<Option<Shared<SupportSets>>, Unavailable>>>;

/// The asserted facts of a revision that follow from its other facts without their own
/// assertion ([`nrese_reasoner::graph_sets::rederived`]; a cycle through the fact supports
/// nothing), sorted: what a reader who sees every
/// inference (`inferred = "visible"`) is shown of a fact asserted only in graphs it can't
/// read, as the inference it is. The stack holds no copy of them (an inferred statement is
/// suppressed while it is asserted anywhere), so without them that reader's answers would
/// depend on what those graphs assert.
#[derive(Debug)]
struct Rederived {
    facts: Kept<Triple>,
    /// The schema facts the ground program was grounded on, and the ruleset's axioms,
    /// sorted: a change of either changes the program, and every fact is checked afresh.
    schema: Kept<Triple>,
    axioms: Kept<Triple>,
}

impl Rederived {
    fn empty(kept: &Arc<Budget>) -> Self {
        Self {
            facts: ChargedVec::new(Arc::clone(kept)),
            schema: ChargedVec::new(Arc::clone(kept)),
            axioms: ChargedVec::new(Arc::clone(kept)),
        }
    }

    fn grounded_on(&self, schema: &HashSet<Triple>, axioms: &[Triple]) -> bool {
        schema.len() == self.schema.len()
            && self.schema.iter().all(|fact| schema.contains(fact))
            && *self.axioms == *axioms
    }
}

/// One revision's re-derived facts, computed once (or not: a failure is kept by no one).
type Rederiving = Shared<OnceLock<Result<Shared<Rederived>, Unavailable>>>;

/// A reader's view of one revision, computed once.
type ViewCell = Shared<OnceLock<Result<Snapshot, Unavailable>>>;

/// The views of one revision, by access.
type View = (Arc<GraphAccess>, ViewCell);

/// The latest revision read, the cells of its sets and re-derived facts, and its readers'
/// views.
type Latest = (Snapshot, Cell, Rederiving, Kept<View>);

/// The last sets computed, to update ([`Supports::base_for`]): their revision, the sets,
/// and the facts the commits since touched.
type Earlier = (u64, Shared<SupportSets>, Kept<Triple>);

/// The store's support graph sets and restricted views, for the latest revision read.
#[derive(Debug)]
pub(crate) struct Supports {
    /// The rules in place and what was computed under them.
    state: Mutex<State>,
    counters: Counters,
    /// Whether sets were computed since the rules were set: readers use them, so
    /// preparing them for a new revision pays.
    in_use: std::sync::atomic::AtomicBool,
    /// What the sets and views keep is charged to.
    kept: Arc<Budget>,
}

#[derive(Debug, Default)]
struct Counters {
    computed: std::sync::atomic::AtomicU64,
    updated: std::sync::atomic::AtomicU64,
    views: std::sync::atomic::AtomicU64,
    patched: std::sync::atomic::AtomicU64,
    closures: std::sync::atomic::AtomicU64,
    computing: std::sync::atomic::AtomicU64,
    updating: std::sync::atomic::AtomicU64,
    viewing: std::sync::atomic::AtomicU64,
}

impl Counters {
    fn add(counter: &std::sync::atomic::AtomicU64, n: u64) {
        counter.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How a store's support graph sets were obtained, since it started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SupportStatistics {
    /// Computed afresh for a revision.
    pub computed: u64,
    /// Updated from an earlier revision's for what the commits since touched.
    pub updated: u64,
    /// Readers' views built, and how many of them from the reader's earlier one.
    pub views: u64,
    pub patched: u64,
    /// Views that asked the reader's closure about statements their kept sets left open.
    pub closures: u64,
    /// Microseconds spent computing, updating, and building views.
    pub computing_us: u64,
    pub updating_us: u64,
    pub viewing_us: u64,
}

/// The rules the inferred stack is maintained with ([`crate::StoreService::use_reasoning_rules`])
/// and how many times they had been set: what a computation takes its cells under. Sets,
/// re-derived facts and views are made with these rules, and kept only while they are
/// still in place (the epoch unchanged): a computation that outlives a change of the rules
/// answers its own read by the rules it began with, and leaves the state of the new ones.
#[derive(Debug, Clone, Default)]
struct Generation {
    rules: Option<RuleProgram>,
    epoch: u64,
}

#[derive(Debug)]
struct State {
    generation: Generation,
    /// The latest revision read, its sets, its re-derived facts and its readers' views.
    latest: Option<Latest>,
    /// The last sets computed, and the revision they are of.
    computed: Option<(u64, Shared<SupportSets>)>,
    /// The last re-derived facts computed, and the revision they are of.
    rederived: Option<(u64, Shared<Rederived>)>,
    /// The commits reported since: the revisions before and after, the facts touched.
    commits: Kept<(u64, u64, Shared<Kept<Triple>>)>,
    /// Each reader's last view.
    readers: Kept<Shared<Reader>>,
}

impl State {
    fn new(kept: &Arc<Budget>, generation: Generation) -> Self {
        Self {
            generation,
            latest: None,
            computed: None,
            rederived: None,
            commits: ChargedVec::new(Arc::clone(kept)),
            readers: ChargedVec::new(Arc::clone(kept)),
        }
    }

    /// The oldest revision commits are reported to update from, if any.
    fn oldest(&self) -> Option<u64> {
        let sets = self.computed.as_ref().map(|(at, _)| *at);
        let rederived = self.rederived.as_ref().map(|(at, _)| *at);
        sets.into_iter().chain(rederived).min()
    }

    /// The latest revision's entry, `snapshot`'s from now on, its cells charged to `kept`.
    fn latest_for(
        &mut self,
        snapshot: &Snapshot,
        kept: &Arc<Budget>,
    ) -> Result<&mut Latest, BudgetExceeded> {
        if !self
            .latest
            .as_ref()
            .is_some_and(|(seen, ..)| seen.same_version(snapshot))
        {
            self.latest = Some((
                snapshot.clone(),
                Shared::new(OnceLock::new(), kept)?,
                Shared::new(OnceLock::new(), kept)?,
                ChargedVec::new(Arc::clone(kept)),
            ));
        }
        Ok(self.latest.as_mut().expect("set above"))
    }

    /// The facts the reported commits touched from revision `from` to `revision` (sorted,
    /// on `working`), if they lead there.
    fn touched_between(
        &self,
        from: u64,
        revision: u64,
        working: &Arc<Budget>,
    ) -> Result<Option<Kept<Triple>>, BudgetExceeded> {
        let after = |at: u64| self.commits.iter().find(|(start, ..)| *start == at);
        let (mut at, mut len) = (from, 0);
        while at < revision {
            let Some((_, to, facts)) = after(at) else {
                return Ok(None);
            };
            len += facts.len();
            at = *to;
        }
        if at != revision {
            return Ok(None);
        }
        let mut touched: Kept<Triple> = ChargedVec::with_capacity(Arc::clone(working), len)?;
        let mut at = from;
        while at < revision {
            let (_, to, facts) = after(at).expect("found above");
            touched.extend_from_slice(facts)?;
            at = *to;
        }
        touched.sort_unstable();
        dedup(&mut touched);
        Ok(Some(touched))
    }
}

/// How the store's configuration compiles the rules.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Compilation {
    pub(crate) hide_unnamed_classes: bool,
    pub(crate) by_representatives: bool,
    pub(crate) compact: bool,
    pub(crate) cap: usize,
}

impl Supports {
    /// Sets and views kept on `kept`.
    pub(crate) fn new(kept: Arc<Budget>) -> Self {
        Self {
            state: Mutex::new(State::new(&kept, Generation::default())),
            counters: Counters::default(),
            in_use: std::sync::atomic::AtomicBool::default(),
            kept,
        }
    }

    pub(crate) fn use_rules(&self, rules: Option<RuleProgram>) {
        // Sets and views of other rules are stale, and so is what is being computed under
        // them: the rules and their state change together.
        let mut state = self.state();
        let epoch = state.generation.epoch + 1;
        *state = State::new(&self.kept, Generation { rules, epoch });
    }

    /// What the sets and views keep is charged to.
    pub(crate) fn kept(&self) -> &Arc<Budget> {
        &self.kept
    }

    /// How many times the rules were set ([`Self::use_rules`]): what a read decided under
    /// them, a session's kept view included ([`crate::sessions`]), holds while it is the same.
    pub(crate) fn epoch(&self) -> u64 {
        self.state().generation.epoch
    }

    /// Whether readers used sets since the rules were set.
    pub(crate) fn in_use(&self) -> bool {
        self.in_use.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Computes the sets of `snapshot` on a thread of its own, unless they are there
    /// or being computed: the first restricted read then finds them (or waits for
    /// that computation instead of starting its own). What computing them reads is
    /// charged to `memory`, the queries' memory; a failure is logged and kept by no one.
    pub(crate) fn prepare(
        self: &Arc<Self>,
        snapshot: Snapshot,
        compilation: Compilation,
        memory: Envelope,
    ) {
        let (cell, generation) = match self.cell_for(&snapshot) {
            Ok(taken) => taken,
            Err(error) => {
                tracing::warn!(?error, "support graph sets not prepared");
                return;
            }
        };
        if cell.get().is_some() {
            return;
        }
        let supports = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("nrese-support-sets".to_owned())
            .spawn(move || {
                let working = Work {
                    budget: Arc::new(memory.part()),
                    cancellation: None,
                };
                let sets = cell.get_or_init(|| {
                    supports.compute(&snapshot, &generation, compilation, &working)
                });
                if let Err(error) = sets {
                    tracing::warn!(?error, "support graph sets not prepared");
                    supports.drop_failed(&snapshot, &cell, None, None);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "support graph sets not prepared");
        }
    }

    /// The cell of `snapshot`'s sets, which becomes the latest revision's, and the rules
    /// it is computed under.
    fn cell_for(&self, snapshot: &Snapshot) -> Result<(Cell, Generation), BudgetExceeded> {
        let mut state = self.state();
        let generation = state.generation.clone();
        let (_, cell, ..) = state.latest_for(snapshot, &self.kept)?;
        Ok((cell.clone(), generation))
    }

    pub(crate) fn statistics(&self) -> SupportStatistics {
        use std::sync::atomic::Ordering::Relaxed;
        let c = &self.counters;
        SupportStatistics {
            computed: c.computed.load(Relaxed),
            updated: c.updated.load(Relaxed),
            views: c.views.load(Relaxed),
            patched: c.patched.load(Relaxed),
            closures: c.closures.load(Relaxed),
            computing_us: c.computing.load(Relaxed),
            updating_us: c.updating.load(Relaxed),
            viewing_us: c.viewing.load(Relaxed),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Forgets the sets: the next ones are computed afresh (after commits that didn't
    /// report what they touched: a replica's, [`crate::StoreService::apply_replication_log`]).
    pub(crate) fn forget(&self) {
        let mut state = self.state();
        state.computed = None;
        state.rederived = None;
        state.commits.clear();
    }

    /// Whether commits should report what they touched: there are sets or re-derived facts
    /// to update.
    pub(crate) fn wants_commits(&self) -> bool {
        self.state().oldest().is_some()
    }

    /// What `tx` touches (the statements it inserts or deletes, asserted and inferred),
    /// sorted, to report after its commit ([`Self::note_commit`]), kept on the support
    /// budget; `None` if it has no room (the next sets are then computed afresh).
    pub(crate) fn touched(&self, tx: &nrese_engine::Transaction<'_>) -> Option<Kept<Triple>> {
        let quads = || tx.inserted().chain(tx.deleted());
        let inferred = || tx.inferred_inserted().chain(tx.inferred_deleted());
        let len = quads().count() + inferred().count();
        let mut facts: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(&self.kept), len).ok()?;
        let all = quads()
            .map(|q| [q.subject.raw(), q.predicate.raw(), q.object.raw()])
            .chain(inferred().map(|t| [t.subject.raw(), t.predicate.raw(), t.object.raw()]));
        facts.extend_bounded(len, all).ok()?;
        facts.sort_unstable();
        dedup(&mut facts);
        Some(facts)
    }

    /// A commit from revision `from` to `to` touched `touched` ([`Self::touched`]; `None`:
    /// it couldn't be recorded).
    pub(crate) fn note_commit(&self, from: u64, to: u64, touched: Option<Kept<Triple>>) {
        let mut state = self.state();
        let Some(at) = state.oldest() else {
            return;
        };
        let kept: usize = state.commits.iter().map(|(.., facts)| facts.len()).sum();
        let fits = touched
            .filter(|touched| to > at && kept + touched.len() <= MAX_TOUCHED)
            .map(|touched| {
                let touched = Shared::new(touched, &self.kept)?;
                state.commits.push((from, to, touched))
            });
        if !matches!(fits, Some(Ok(()))) {
            // Too much to update from: the next sets are computed afresh.
            state.computed = None;
            state.rederived = None;
            state.commits.clear();
        }
    }

    /// `snapshot` as a reader with `access` sees it. By support (`inferred = "supported"`):
    /// with only the inferred statements it may derive from the graphs it reads. Seeing
    /// every inference (`"visible"`): the stack, and the facts asserted only in graphs it
    /// can't read that follow from the other facts without that assertion, as inferred.
    /// Either way what it sees doesn't depend on what a graph it can't read asserts of what
    /// it derives. What computing the view reads and builds is charged to `memory`, the
    /// reading query's envelope; what is kept, to the support budget. `Err` if either has
    /// no room, or `cancellation` (the reading query's) is cancelled while it is computed:
    /// the read fails, and nothing of it is kept. A computation it waited for that another
    /// reader's cancellation stopped is made again, by this read.
    pub(crate) fn view(
        &self,
        snapshot: &Snapshot,
        access: &Arc<GraphAccess>,
        compilation: Compilation,
        memory: &Envelope,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Snapshot, Unavailable> {
        loop {
            match self.view_once(snapshot, access, compilation, memory, cancellation) {
                Err(Unavailable::Cancelled)
                    if !cancellation.is_some_and(CancellationToken::is_cancelled) => {}
                made => return made,
            }
        }
    }

    /// [`Self::view`], once: its failure is kept by no one.
    fn view_once(
        &self,
        snapshot: &Snapshot,
        access: &Arc<GraphAccess>,
        compilation: Compilation,
        memory: &Envelope,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Snapshot, Unavailable> {
        let (cell, rederiving, view, generation) = {
            let mut state = self.state();
            let generation = state.generation.clone();
            let (_, cell, rederiving, views) = state.latest_for(snapshot, &self.kept)?;
            let view = match views.iter().find(|(seen, _)| **seen == **access) {
                Some((_, view)) => view.clone(),
                None => {
                    if views.len() >= VIEWS {
                        without_first(views);
                    }
                    let view = Shared::new(OnceLock::new(), &self.kept)?;
                    views.push((Arc::clone(access), view.clone()))?;
                    view
                }
            };
            (cell.clone(), rederiving.clone(), view, generation)
        };
        let working = Work {
            budget: Arc::new(memory.part()),
            cancellation: cancellation.cloned(),
        };
        let made = view
            .get_or_init(|| match access.inferred_by_support {
                true => {
                    let sets = cell
                        .get_or_init(|| self.compute(snapshot, &generation, compilation, &working));
                    // The cell's sets were computed under its state's rules, `generation`'s:
                    // the closure deciding what they leave open is by the same rules.
                    match (sets, &generation.rules) {
                        (Ok(Some(sets)), Some(rules)) => {
                            let within =
                                || self.within(snapshot, access, rules, compilation, &working);
                            self.build_view(
                                snapshot,
                                access,
                                sets,
                                within,
                                generation.epoch,
                                &working,
                            )
                        }
                        (Ok(_), _) => Ok(snapshot.with_inferred_only(&[], &self.kept)?),
                        (Err(unavailable), _) => Err(*unavailable),
                    }
                }
                false => {
                    let found = rederiving.get_or_init(|| {
                        self.compute_rederived(snapshot, &generation, compilation, &working)
                    });
                    match found {
                        Ok(found) => self.visible_view(snapshot, access, found, &working),
                        Err(unavailable) => Err(*unavailable),
                    }
                }
            })
            .clone();
        if made.is_err() {
            self.drop_failed(snapshot, &cell, Some(&rederiving), Some(&view));
        }
        made
    }

    /// The view of `snapshot` for a reader with `access` who sees every inference: the
    /// facts of `found` asserted only in graphs it can't read, added to the stack as
    /// inferred statements (the snapshot itself where there are none).
    fn visible_view(
        &self,
        snapshot: &Snapshot,
        access: &GraphAccess,
        found: &Rederived,
        working: &Arc<Budget>,
    ) -> Result<Snapshot, Unavailable> {
        let started = std::time::Instant::now();
        let mut scanning = Reservation::new(Arc::clone(working));
        scanning.charge(snapshot.scan_bytes(ReadModel::Asserted))?;
        let mut show: Kept<EncodedQuad> = ChargedVec::new(Arc::clone(working));
        for &fact in found.facts.iter() {
            if asserted_out_of_reach(snapshot, access, fact) {
                let [s, p, o] = fact.map(TermId::from_raw);
                show.push(EncodedQuad::new(s, p, o, TermId::DEFAULT_GRAPH))?;
            }
        }
        drop(scanning);
        let view = match show.is_empty() {
            true => snapshot.clone(),
            false => {
                let mask = InferredMask::default().changed(snapshot, &[], &show, &self.kept)?;
                snapshot
                    .with_inferred_mask(&mask, &self.kept)?
                    .expect("a mask built for this stack's layout")
            }
        };
        Counters::add(&self.counters.views, 1);
        Counters::add(&self.counters.viewing, started.elapsed().as_micros() as u64);
        Ok(view)
    }

    /// The re-derived facts of `snapshot`: the last ones updated for what the commits since
    /// touched, where they lead there and the program is the same, else checked afresh
    /// over every asserted fact. What computing them reads charged to `working`. Under
    /// `reasoner.equality = "compact"` none: the stack is over representatives (its
    /// spellings aren't the asserted ones), and that mode's restricted reads are their own
    /// design (item 18, compact equality).
    fn compute_rederived(
        &self,
        snapshot: &Snapshot,
        generation: &Generation,
        compilation: Compilation,
        working: &Work,
    ) -> Result<Shared<Rederived>, Unavailable> {
        let Some(rules) = &generation.rules else {
            return Ok(Shared::new(Rederived::empty(&self.kept), &self.kept)?);
        };
        if compilation.compact {
            return Ok(Shared::new(Rederived::empty(&self.kept), &self.kept)?);
        }
        let program = self.program(rules, snapshot, compilation);
        let ground = program.ground_program(snapshot);
        let schema = ground.schema_premises();
        let mut axioms: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(&self.kept), program.axioms().len())?;
        axioms.extend_from_slice(program.axioms())?;
        axioms.sort_unstable();
        let revision = snapshot.revision();
        let following = Following::new(working);
        let watch = |bytes: usize| following.watch(bytes);
        let closure = After {
            snapshot,
            removed: &[],
        };
        let asserted = |fact: Triple| {
            snapshot.exists_in(ReadModel::Asserted, &pattern(fact, GraphSelector::Any))
        };
        // Past this many facts, a candidate's cone is no cheaper than the whole closure.
        let limit = usize::try_from(snapshot.len_in(ReadModel::Materialised)).unwrap_or(usize::MAX)
            / 4
            + 10_000;
        // The candidates (sorted) that follow from the other facts without their own
        // assertion, sorted; those whose cone can't decide, by the closure of the asserted
        // facts without each.
        let decide = |candidates: &[Triple]| -> Result<Kept<Triple>, Unavailable> {
            let (found, undecided) = rederived(
                &closure, &ground, &axioms, &asserted, candidates, limit, &watch,
            )
            .map_err(|stopped| following.failure(stopped))?;
            let mut passing: Kept<Triple> =
                ChargedVec::with_capacity(Arc::clone(working), found.len() + undecided.len())?;
            passing.extend_from_slice(&found)?;
            drop(found);
            if !undecided.is_empty() {
                let graphs = graphs_in_order(snapshot, &[TermId::DEFAULT_GRAPH], working, working)?;
                let whole = Closure::of(snapshot, &graphs, &axioms, working)?;
                for &fact in &undecided {
                    let without = |other: Triple| other != fact && asserted(other);
                    let held = closure_within(&whole.facts, &ground, &axioms, &without, &watch)
                        .map_err(|stopped| following.failure(stopped))?;
                    if held.contains_key(&fact) {
                        passing.push(fact)?;
                    }
                }
            }
            passing.sort_unstable();
            Ok(passing)
        };
        let started = std::time::Instant::now();
        let earlier = {
            let state = self.state();
            match &state.rederived {
                Some((from, earlier))
                    if state.generation.epoch == generation.epoch
                        && earlier.grounded_on(&schema, &axioms) =>
                {
                    state
                        .touched_between(*from, revision, working)?
                        .map(|touched| (earlier.clone(), touched))
                }
                _ => None,
            }
        };
        let mut updated = None;
        if let Some((earlier, touched)) = earlier {
            let in_closure = |fact: Triple| {
                axioms.binary_search(&fact).is_ok()
                    || snapshot
                        .exists_in(ReadModel::Materialised, &pattern(fact, GraphSelector::Any))
            };
            let mut removed: Kept<Triple> =
                ChargedVec::with_capacity(Arc::clone(working), touched.len())?;
            for &fact in touched.iter() {
                if !in_closure(fact) {
                    removed.push(fact)?;
                }
            }
            let after = After {
                snapshot,
                removed: &removed,
            };
            let candidates = rederived_candidates(&after, &ground, &touched, limit, &watch)
                .map_err(|stopped| following.failure(stopped))?;
            if let Some(candidates) = candidates {
                let mut reachable: Kept<Triple> =
                    ChargedVec::with_capacity(Arc::clone(working), candidates.len())?;
                for &fact in &candidates {
                    if asserted(fact) {
                        reachable.push(fact)?;
                    }
                }
                let passing = decide(&reachable)?;
                drop(reachable);
                // The earlier ones the change can't reach, with the candidates that pass.
                let mut reached = candidates.iter().peekable();
                let unreached = earlier.facts.iter().copied().filter(|fact| {
                    while reached.next_if(|candidate| *candidate < fact).is_some() {}
                    reached.peek() != Some(&fact)
                });
                let mut facts: Kept<Triple> = ChargedVec::with_capacity(
                    Arc::clone(&self.kept),
                    earlier.facts.len() + passing.len(),
                )?;
                merge_sorted(unreached, passing.iter().copied(), &mut facts)?;
                updated = Some(facts);
            }
        }
        let (facts, counter, time) = match updated {
            Some(facts) => (facts, &self.counters.updated, &self.counters.updating),
            None => {
                let quads = sorted_quads(snapshot, ReadModel::Asserted, working)?;
                let mut all: Kept<Triple> =
                    ChargedVec::with_capacity(Arc::clone(working), quads.len())?;
                for quad in quads.iter() {
                    all.push(triple(*quad))?;
                }
                drop(quads);
                dedup(&mut all);
                let passing = decide(&all)?;
                drop(all);
                let mut facts: Kept<Triple> =
                    ChargedVec::with_capacity(Arc::clone(&self.kept), passing.len())?;
                facts.extend_from_slice(&passing)?;
                (facts, &self.counters.computed, &self.counters.computing)
            }
        };
        drop(ground);
        Counters::add(counter, 1);
        Counters::add(time, started.elapsed().as_micros() as u64);
        let mut kept_schema: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(&self.kept), schema.len())?;
        for &fact in &schema {
            kept_schema.push(fact)?;
        }
        kept_schema.sort_unstable();
        let found = Shared::new(
            Rederived {
                facts,
                schema: kept_schema,
                axioms,
            },
            &self.kept,
        )?;
        let mut state = self.state();
        if state.generation.epoch == generation.epoch
            && state
                .rederived
                .as_ref()
                .is_none_or(|(at, _)| *at < revision)
        {
            state.rederived = Some((revision, found.clone()));
            if let Some(oldest) = state.oldest() {
                state.commits.retain(|&(_, to, _)| to > oldest);
            }
        }
        Ok(found)
    }

    /// Forgets a failed computation of `snapshot`'s sets (`cell`), of its re-derived facts
    /// (`rederiving`) or of a view of it, so that the next read tries again.
    fn drop_failed(
        &self,
        snapshot: &Snapshot,
        cell: &Cell,
        rederiving: Option<&Rederiving>,
        view: Option<&ViewCell>,
    ) {
        let mut state = self.state();
        let Some((seen, latest, latest_rederiving, views)) = state.latest.as_mut() else {
            return;
        };
        if !seen.same_version(snapshot) {
            return;
        }
        if let Some(view) = view {
            views.retain(|(_, kept)| !kept.ptr_eq(view));
        }
        let failed = (latest.ptr_eq(cell) && matches!(cell.get(), Some(Err(_))))
            || rederiving.is_some_and(|rederiving| {
                latest_rederiving.ptr_eq(rederiving) && matches!(rederiving.get(), Some(Err(_)))
            });
        if failed {
            // The revision's cells and views are made again by its next read.
            state.latest = None;
        }
    }

    /// The view of `snapshot` for `access` under `sets`: from the reader's last one where
    /// the sets were updated from its revision and they decide every changed statement,
    /// else from the whole stack, `within` (the reader's closure, by the sets' rules)
    /// deciding what the sets leave open. The sets are of the rules set `epoch` times: the
    /// reader's last view is taken, and this one kept, only while they are in place. The
    /// reading query's cancellation stops it before it charges anything, while it splits,
    /// and before it keeps the view: a cancelled read builds and keeps nothing, though the
    /// sets were made before.
    fn build_view(
        &self,
        snapshot: &Snapshot,
        access: &Arc<GraphAccess>,
        sets: &Shared<SupportSets>,
        within: impl FnOnce() -> Result<Kept<Triple>, Unavailable>,
        epoch: u64,
        working: &Work,
    ) -> Result<Snapshot, Unavailable> {
        working.go_on()?;
        let started = std::time::Instant::now();
        let earlier = {
            let state = self.state();
            (state.generation.epoch == epoch)
                .then(|| {
                    state
                        .readers
                        .iter()
                        .find(|reader| *reader.access == **access)
                        .cloned()
                })
                .flatten()
        };
        let mut form = None;
        if let Some(reader) = earlier
            && let Some(changes) =
                sets.changes_since(snapshot, &reader.sets, &reader.snapshot, access, working)?
        {
            form = match &reader.form {
                Form::Mask(mask) => {
                    let mask = mask.changed(snapshot, &changes.hide, &changes.show, &self.kept)?;
                    (mask.runs() <= MASK_RUNS).then_some(Form::Mask(mask))
                }
                Form::Only(list) => {
                    let list = patched(list, &changes, &self.kept)?;
                    Some(Form::Only(Shared::new(list, &self.kept)?))
                }
            };
        }
        let form = match form {
            Some(form) => {
                Counters::add(&self.counters.patched, 1);
                form
            }
            None => {
                let within = || {
                    Counters::add(&self.counters.closures, 1);
                    within()
                };
                match sets.split(snapshot, access, &self.kept, working, within)? {
                    Split::Mask { hide, show } => Form::Mask(
                        InferredMask::default().changed(snapshot, &hide, &show, &self.kept)?,
                    ),
                    Split::Only(shown) => Form::Only(Shared::new(shown, &self.kept)?),
                }
            }
        };
        let view = match &form {
            Form::Mask(mask) => snapshot
                .with_inferred_mask(mask, &self.kept)?
                .expect("a mask built for this stack's layout"),
            Form::Only(list) => snapshot.with_inferred_only(list, &self.kept)?,
        };
        working.go_on()?;
        let reader = Shared::new(
            Reader {
                access: Arc::clone(access),
                snapshot: snapshot.clone(),
                sets: sets.clone(),
                form,
            },
            &self.kept,
        )?;
        {
            let mut state = self.state();
            if state.generation.epoch == epoch {
                state.readers.retain(|reader| *reader.access != **access);
                if state.readers.len() >= VIEWS {
                    without_first(&mut state.readers);
                }
                state.readers.push(reader)?;
            }
        }
        Counters::add(&self.counters.views, 1);
        Counters::add(&self.counters.viewing, started.elapsed().as_micros() as u64);
        Ok(view)
    }

    /// The program the rules compile to over `snapshot`'s ids, as the store configures it.
    fn program(
        &self,
        rules: &RuleProgram,
        snapshot: &Snapshot,
        compilation: Compilation,
    ) -> Program {
        // The program's constants by the snapshot's ids: a constant the store doesn't hold
        // gets an id no statement uses (its rules can't fire).
        let unknown = std::cell::Cell::new(u64::MAX >> 1);
        Program::compile(rules, &|term| {
            snapshot.lookup(term).unwrap_or_else(|| {
                unknown.set(unknown.get() - 1);
                TermId::from_raw(unknown.get())
            })
        })
        .hiding_unnamed_classes(compilation.hide_unnamed_classes)
        .by_representatives(compilation.by_representatives)
    }

    /// The closure of the statements a reader with `access` reads in `snapshot`, sorted,
    /// by the evaluation that gives the sets, under `rules` (the sets' own): what decides
    /// the statements its kept sets leave open. Charged to `working`.
    fn within(
        &self,
        snapshot: &Snapshot,
        access: &GraphAccess,
        rules: &RuleProgram,
        compilation: Compilation,
        working: &Work,
    ) -> Result<Kept<Triple>, Unavailable> {
        let program = self.program(rules, snapshot, compilation);
        let mut axioms: Kept<Triple> =
            ChargedVec::with_capacity(Arc::clone(working), program.axioms().len())?;
        axioms.extend_from_slice(program.axioms())?;
        axioms.sort_unstable();
        let graphs = graphs_in_order(snapshot, &[TermId::DEFAULT_GRAPH], working, working)?;
        let closure = Closure::of(snapshot, &graphs, &axioms, working)?;
        let mut readable: Kept<bool> =
            ChargedVec::with_capacity(Arc::clone(working), graphs.len())?;
        for &graph in graphs.iter() {
            readable.push(access.allows_id(snapshot, graph))?;
        }
        let reads = |fact: Triple| {
            closure
                .graphs_of(fact)
                .iter()
                .any(|&(_, g)| readable[g as usize])
        };
        let ground = program.ground_program(snapshot);
        let following = Following::new(working);
        let held = closure_within(&closure.facts, &ground, &axioms, &reads, &|bytes| {
            following.watch(bytes)
        })
        .map_err(|stopped| following.failure(stopped))?;
        drop(ground);
        let mut facts: Kept<Triple> = ChargedVec::with_capacity(Arc::clone(working), held.len())?;
        facts.extend_bounded(held.len(), held.keys().copied())?;
        drop(held);
        facts.sort_unstable();
        Ok(facts)
    }

    /// The sets of `snapshot` under `generation`'s rules: the last ones updated by the
    /// commits since, where they lead there, else computed afresh. What computing them
    /// reads charged to `working`.
    fn compute(
        &self,
        snapshot: &Snapshot,
        generation: &Generation,
        compilation: Compilation,
        working: &Work,
    ) -> Result<Option<Shared<SupportSets>>, Unavailable> {
        let Some(rules) = &generation.rules else {
            return Ok(None);
        };
        if compilation.compact {
            return Ok(None);
        }
        let program = self.program(rules, snapshot, compilation);
        let cap = compilation.cap.max(1);
        let revision = snapshot.revision();
        self.in_use
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let started = std::time::Instant::now();
        let updated = match self.base_for(revision, generation.epoch, working)? {
            Some((from, earlier, touched)) => {
                earlier.updated(&program, snapshot, from, &touched, cap, &self.kept, working)?
            }
            None => None,
        };
        let c = &self.counters;
        let (sets, counter, time) = match updated {
            Some(sets) => (sets, &c.updated, &c.updating),
            None => (
                SupportSets::compute(&program, snapshot, cap, &self.kept, working)?,
                &c.computed,
                &c.computing,
            ),
        };
        let elapsed = started.elapsed();
        Counters::add(counter, 1);
        Counters::add(time, elapsed.as_micros() as u64);
        if sets.since.is_none() {
            let (facts, total, truncated) = sets.len();
            tracing::info!(
                revision,
                facts,
                sets = total,
                truncated,
                elapsed_ms = elapsed.as_millis() as u64,
                "support graph sets computed"
            );
            if truncated > 0 {
                tracing::info!(
                    truncated,
                    cap,
                    "statements have more support graph sets than reasoner.support_sets \
                     keeps; a reader none of their kept sets fits is decided by the closure \
                     of its graphs"
                );
            }
        }
        let sets = Shared::new(sets, &self.kept)?;
        let mut state = self.state();
        if state.generation.epoch == generation.epoch
            && state.computed.as_ref().is_none_or(|(at, _)| *at < revision)
        {
            state.computed = Some((revision, sets.clone()));
            if let Some(oldest) = state.oldest() {
                state.commits.retain(|&(_, to, _)| to > oldest);
            }
        }
        Ok(Some(sets))
    }

    /// The last sets computed, their revision, and the facts the reported commits
    /// touched from there to `revision` (sorted, on `working`), if the commits lead there
    /// and the rules set `epoch` times are still in place.
    fn base_for(
        &self,
        revision: u64,
        epoch: u64,
        working: &Arc<Budget>,
    ) -> Result<Option<Earlier>, BudgetExceeded> {
        let state = self.state();
        if state.generation.epoch != epoch {
            return Ok(None);
        }
        let Some((from, sets)) = state.computed.clone() else {
            return Ok(None);
        };
        Ok(state
            .touched_between(from, revision, working)?
            .map(|touched| (from, sets, touched)))
    }
}
