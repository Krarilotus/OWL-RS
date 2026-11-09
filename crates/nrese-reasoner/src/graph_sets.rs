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
//! Memory: a round's derivations, which grow with the rule instances (a complete
//! transitive relation of n² facts has n³), are charged to the evaluation's working budget
//! ([`Limits::scratch`]) before they are allocated, and reduced per fact as they come,
//! exactly ([`found`]); a refusal stops the evaluation ([`Stopped::Refused`]). The rest of
//! its working set (the closure's store, the sets, a round's delta) is not charged before
//! it is allocated: it reports its size to a [`Watch`] at round boundaries and stops when
//! the watch refuses (the reading query's envelope), or when the process passes its memory
//! limit. A temporary exemption, shared with the materialiser's working set, until the
//! reasoner's working set is charged (notes/2026-10-20-executor-admission-design.md, W1
//! site "reasoner fixpoint").

mod found;

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hashbrown::{HashMap, HashSet};
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
use rayon::prelude::*;

use self::found::Found;
use super::batch::Store;
use super::eval::{
    GroundProgram, Grounding, Job, NEVER, OperationStop, Poller, Probe, Seg, Source, Stop,
    Stopped as ReadStopped, Visit, Visits, derivations, instantiate_head, transitivity,
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
    /// The process's memory couldn't be read for its limit
    /// ([`nrese_exec::memory::process_limit_stop`]): the limit can't be enforced, so it
    /// stopped.
    MemoryUnreadable,
    /// Its caller's stop fired ([`Limits::stop`]): the reading query was cancelled, or its
    /// time ran out.
    Cancelled,
    /// Its working budget ([`Limits::scratch`]) had no room for a round's derivations: they
    /// weren't allocated.
    Refused(BudgetExceeded),
}

// A read its poller stopped ([`ReadStopped`]) has no `From` here: a run's pollers stop on
// its caller's stop, the process's memory and another morsel's latched refusal alike
// (`Run::fired`), so each such read takes the run's own cause, `Run::stopped` (REAS-2 fix
// check r1, B11: a refusal read as a cancel would lose the store's `Unavailable::Memory`).

/// A budget refusal keeps its cause through a lookup's `?` ([`Lookup::sets_of`]).
impl From<BudgetExceeded> for Stopped {
    fn from(exceeded: BudgetExceeded) -> Self {
        Self::Refused(exceeded)
    }
}

impl Stopped {
    /// The stop the process limit's `cause` makes.
    fn of_process(cause: nrese_exec::memory::MemoryStopCause) -> Self {
        match cause {
            nrese_exec::memory::MemoryStopCause::Unread => Self::MemoryUnreadable,
            nrese_exec::memory::MemoryStopCause::OverLimit => Self::ProcessMemory,
        }
    }
}

/// What an evaluation reports its working set's bytes to at round boundaries: `false`
/// stops it ([`Stopped::Memory`]).
pub type Watch<'a> = &'a (dyn Fn(usize) -> bool + Sync);

fn unwatched(_: usize) -> bool {
    true
}

/// A [`Watch`] that lets an evaluation hold anything (the process limit still applies).
pub const UNWATCHED: Watch<'static> = &unwatched;

/// What an evaluation answers to: `watch` sees its working set's bytes at round
/// boundaries, and `stop` (its caller's cancellation) is asked there too. `stop` must take
/// no lock: it may be asked inside a scan. A round's derivations are charged to `scratch`
/// before they are allocated.
#[derive(Clone, Copy)]
pub struct Limits<'a> {
    pub watch: Watch<'a>,
    pub stop: Stop<'a>,
    pub scratch: &'a Arc<Budget>,
}

impl<'a> Limits<'a> {
    /// Nothing but the process's memory limit stops the evaluation; its derivations are
    /// charged to `scratch` (an unlimited budget, for a caller with nothing to follow).
    pub fn unlimited(scratch: &'a Arc<Budget>) -> Self {
        Self {
            watch: UNWATCHED,
            stop: NEVER,
            scratch,
        }
    }
}

/// One evaluation call's limits, its stop latched with the process's memory limit
/// ([`OperationStop`]): once it has stopped it stays stopped, with its cause, so no stage
/// it cut short is read as whole.
struct Run<'a> {
    watch: Watch<'a>,
    stop: OperationStop<'a>,
    scratch: &'a Arc<Budget>,
    /// Whether a charge to `scratch` was refused: the run stops ([`Self::refuse`]).
    refused: AtomicBool,
    /// The first refusal, to report.
    refusal: Mutex<Option<BudgetExceeded>>,
}

impl<'a> Run<'a> {
    fn new(limits: Limits<'a>) -> Self {
        Self {
            watch: limits.watch,
            stop: OperationStop::new(limits.stop),
            scratch: limits.scratch,
            refused: AtomicBool::new(false),
            refusal: Mutex::new(None),
        }
    }

    /// Whether it has stopped, a refused charge included: what its stages' pollers ask
    /// ([`Poller`]).
    fn fired(&self) -> bool {
        self.refused.load(Ordering::Relaxed) || self.stop.fired()
    }

    /// A charge to its working budget was refused: the run stops, its other morsels at
    /// their next poll, and reports the first refusal ([`Stopped::Refused`]).
    fn refuse(&self, exceeded: BudgetExceeded) {
        self.refusal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert(exceeded);
        self.refused.store(true, Ordering::Relaxed);
    }

    /// Room for `bytes` of copies on the way to [`minimal`], charged to the run's working
    /// budget before they are made, released when it drops.
    fn room(&self, bytes: usize) -> Result<Reservation<&'a Budget>, BudgetExceeded> {
        // Borrowed: no reference count per call (it is taken per rule instance's product).
        let mut held = Reservation::new(&**self.scratch);
        held.charge(bytes)?;
        Ok(held)
    }

    /// A task's derivations, charged to the run's working budget.
    fn found(&self) -> Found<'a> {
        Found::new(self.scratch)
    }

    /// Every task's derivations in one, its room charged first; `Err` if the run has
    /// stopped (a part may be cut short) or the room is refused.
    ///
    /// Gathered into the part with the most room, which usually holds them all already: a
    /// fresh buffer of their total beside every part was the round's peak, 1.1 MB above
    /// the base's on support-update-committed (merge-time count
    /// `p2c5-r4-memcount1-2df9ed96`, by phase `p2c5-r4-memprof3-e2a444ec`).
    fn gather(&self, mut parts: Vec<Found<'a>>) -> Result<Found<'a>, Stopped> {
        if self.fired() {
            return Err(self.stopped());
        }
        let roomiest = (0..parts.len()).max_by_key(|&i| parts[i].capacity());
        let mut all = match roomiest {
            Some(i) => parts.swap_remove(i),
            None => self.found(),
        };
        all.reserve(parts.iter().map(Found::len).sum())
            .map_err(Stopped::Refused)?;
        for part in parts {
            all.append(part).map_err(Stopped::Refused)?;
        }
        Ok(all.finish())
    }

    /// Why it stopped, once it has.
    fn stopped(&self) -> Stopped {
        if let Some(exceeded) = *self.refusal.lock().unwrap_or_else(|p| p.into_inner()) {
            return Stopped::Refused(exceeded);
        }
        match self.stop.interrupted().memory {
            Some(cause) => Stopped::of_process(cause),
            None => Stopped::Cancelled,
        }
    }

    /// `Err` if the run has stopped (its caller, or the process past its memory limit) or
    /// `watch` refuses `bytes`: after every stage, before its output is read.
    fn check(&self, bytes: usize) -> Result<(), Stopped> {
        if self.fired() {
            return Err(self.stopped());
        }
        match (self.watch)(bytes) {
            true => Ok(()),
            false => Err(Stopped::Memory { bytes }),
        }
    }
}

/// The bytes a hash map of `capacity` entries of `(K, V)` holds in its table.
fn table_bytes<K, V>(capacity: usize) -> usize {
    capacity * (size_of::<(K, V)>() + 1)
}

/// A set of graphs (indices into the caller's graph list) as a bitset: inline up to 128
/// graphs, so a union is a word-wise OR and a subset test an AND-NOT, without allocation.
/// Trailing zero words are trimmed, so equal sets are equal values.
///
/// A copy holds exactly its words ([`Clone`] below, and a union's): what a copy's charge
/// counts is what it allocates (`SmallVec`'s own clone rounds a spilled copy's capacity up to
/// a power of two; review r1 C1 fix proof `p2c5-green17-e62a4843`).
#[derive(Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GraphSet(smallvec::SmallVec<[u64; 2]>);

impl Clone for GraphSet {
    fn clone(&self) -> Self {
        Self(smallvec::SmallVec::from_slice(&self.0))
    }
}

impl GraphSet {
    /// The set of one graph.
    pub fn of(graph: u32) -> Self {
        let word = (graph / 64) as usize;
        let mut words = smallvec::SmallVec::from_elem(0, word + 1);
        words[word] = 1 << (graph % 64);
        Self(words)
    }

    /// The bytes [`Self::of`]`(graph)` holds on the heap, without making it: its words past the
    /// two held inline (`from_elem` allocates exactly the words it is given).
    fn heap_bytes_of(graph: u32) -> usize {
        let words = (graph / 64) as usize + 1;
        match words > 2 {
            true => words * size_of::<u64>(),
            false => 0,
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        let (long, short) = if self.0.len() >= other.0.len() {
            (self, other)
        } else {
            (other, self)
        };
        // The longer operand's words, exactly (see the type's doc).
        let mut words = smallvec::SmallVec::from_slice(&long.0);
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

/// Reads count on the inner sources'.
impl<S: Source + ?Sized> Visits for Changed<'_, S> {
    fn visit(&self, pattern: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>) {
        match seg.plain() {
            // The inner scan counts each position it visits on this visit's poller; stopped,
            // the read around this visit answers `Stopped` (the poller is latched).
            Seg::Old | Seg::All => {
                let scanned = self
                    .all
                    .scan(pattern, Seg::All, visit.poller(), &mut |fact| {
                        visit.counted(fact)
                    });
                debug_assert!(scanned.is_ok() || visit.stopped());
            }
            _ => self.changed.visit(pattern, Seg::All, visit),
        }
    }

    fn size(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<usize, ReadStopped> {
        match seg.plain() {
            Seg::Old | Seg::All => self.all.estimate(pattern, Seg::All, probe.poller()),
            _ => self.changed.size(pattern, Seg::All, probe),
        }
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, ReadStopped> {
        self.all.contains(fact, probe.poller())
    }
}

/// Where facts are asserted, as an evaluation asks it: read where it is kept, never copied
/// into a list per question (fix check r2, C1.b residual). Each question is read on the
/// reading task's poller (REAS-2, sweep M16): the call is counted before any lookup, then
/// each graph handed on; on a stopped poller nothing more is read and the read answers
/// [`ReadStopped`], never a partial list or a "no" (fix check r1, rule 2).
pub trait Graphs: Sync {
    /// Calls `graph` with each graph that asserts `fact` (none for a fact only inferred), in
    /// any order, the same ones each time it is asked; `Err` if its poller stopped it, the
    /// graphs handed on so far a part of them.
    fn each(
        &self,
        fact: Triple,
        poll: &Poller<'_>,
        graph: &mut dyn FnMut(u32),
    ) -> Result<(), ReadStopped>;

    /// Whether `graph` asserts `fact`.
    fn holds(&self, fact: Triple, graph: u32, poll: &Poller<'_>) -> Result<bool, ReadStopped>;
}

/// At most one graph a fact (`None`: in none), as a function gives it: one unit a call.
impl<F: Fn(Triple) -> Option<u32> + Sync> Graphs for F {
    fn each(
        &self,
        fact: Triple,
        poll: &Poller<'_>,
        graph: &mut dyn FnMut(u32),
    ) -> Result<(), ReadStopped> {
        if poll.tick() {
            return Err(ReadStopped);
        }
        if let Some(one) = self(fact) {
            graph(one);
        }
        Ok(())
    }

    fn holds(&self, fact: Triple, graph: u32, poll: &Poller<'_>) -> Result<bool, ReadStopped> {
        if poll.tick() {
            return Err(ReadStopped);
        }
        Ok(self(fact) == Some(graph))
    }
}

/// Each asserted fact with a graph that holds it, sorted, each pair once.
#[derive(Clone, Copy)]
pub struct InGraphs<'a>(pub &'a [(Triple, u32)]);

impl InGraphs<'_> {
    /// The pairs of `fact`.
    fn of(&self, fact: Triple) -> &[(Triple, u32)] {
        let start = self.0.partition_point(|(f, _)| *f < fact);
        let end = start + self.0[start..].partition_point(|(f, _)| *f == fact);
        &self.0[start..end]
    }
}

/// The call counted, then the fact's entries as they are handed on (review r1 F1).
impl Graphs for InGraphs<'_> {
    fn each(
        &self,
        fact: Triple,
        poll: &Poller<'_>,
        graph: &mut dyn FnMut(u32),
    ) -> Result<(), ReadStopped> {
        if poll.tick() {
            return Err(ReadStopped);
        }
        poll.counted(self.of(fact)).for_each(|&(_, g)| graph(g));
        poll.ended()
    }

    /// One search in the fact's entries: the call's count is its work.
    fn holds(&self, fact: Triple, graph: u32, poll: &Poller<'_>) -> Result<bool, ReadStopped> {
        if poll.tick() {
            return Err(ReadStopped);
        }
        Ok(self
            .of(fact)
            .binary_search_by_key(&graph, |&(_, g)| g)
            .is_ok())
    }
}

/// `fact`'s graphs in a list, read on `poll`: for the sets an evaluation keeps (its result,
/// under the W1 exemption of the module doc), never for a lookup's. `Err`, the partial list
/// dropped, where its poller stopped the read.
fn listed(graphs: &dyn Graphs, fact: Triple, poll: &Poller<'_>) -> Result<Vec<u32>, ReadStopped> {
    let mut list = Vec::new();
    graphs.each(fact, poll, &mut |graph| list.push(graph))?;
    Ok(list)
}

/// A fact's sets where its caller keeps them, read by reference: a list, or the entries of
/// a pool of distinct sets. Nothing is copied (fix check r2, C1.b residual).
#[derive(Debug, Clone, Copy)]
pub struct Sets<'k> {
    list: List<'k>,
    /// As [`Annotation::truncated`].
    pub truncated: bool,
    /// As [`Annotation::beyond_own`].
    pub beyond_own: bool,
}

#[derive(Debug, Clone, Copy)]
enum List<'k> {
    Slice(&'k [GraphSet]),
    /// `pool[id]` for each `(_, id)` of `ids`, in their order.
    Pooled {
        ids: &'k [(Triple, u32)],
        pool: &'k [GraphSet],
    },
}

impl Sets<'static> {
    /// No sets.
    pub const NONE: Self = Self {
        list: List::Slice(&[]),
        truncated: false,
        beyond_own: false,
    };
}

impl<'k> Sets<'k> {
    /// `annotation`'s sets and marks.
    pub fn of(annotation: &'k Annotation) -> Self {
        Self {
            list: List::Slice(&annotation.sets),
            truncated: annotation.truncated,
            beyond_own: annotation.beyond_own,
        }
    }

    /// The sets of `pool` that `ids` index (a fact's entries, each with an index into the
    /// pool), marked `truncated` and as wide (one mark kept for both).
    pub fn pooled(ids: &'k [(Triple, u32)], pool: &'k [GraphSet], truncated: bool) -> Self {
        Self {
            list: List::Pooled { ids, pool },
            truncated,
            beyond_own: truncated,
        }
    }

    pub fn len(&self) -> usize {
        match self.list {
            List::Slice(sets) => sets.len(),
            List::Pooled { ids, .. } => ids.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, i: usize) -> &'k GraphSet {
        match self.list {
            List::Slice(sets) => &sets[i],
            List::Pooled { ids, pool } => &pool[ids[i].1 as usize],
        }
    }

    /// The sets, in order.
    pub fn iter(&self) -> impl Iterator<Item = &'k GraphSet> + 'k {
        let sets = *self;
        (0..sets.len()).map(move |i| sets.get(i))
    }

    /// The one set, if there is exactly one.
    fn only(&self) -> Option<&'k GraphSet> {
        (self.len() == 1).then(|| self.get(0))
    }
}

/// A fact's sets before a change ([`Change::previous`]).
#[derive(Debug, Clone, Copy)]
pub enum Previous<'k> {
    /// These, where the caller keeps them.
    Sets(Sets<'k>),
    /// Those it starts with, from its graphs ([`initial_sets`]; the empty set for an axiom):
    /// the caller keeps none to lend, so the update makes them, charged first. Only for a
    /// fact whose graphs the change left as they were (a fact whose graphs changed is
    /// touched, so recomputed and never asked for).
    Own,
}

/// The sets of facts as an evaluation reads them: `None` for a fact without sets yet. Read
/// where they are kept; made under the run's charge where nothing is kept ([`Looked`]),
/// [`Stopped::Refused`] where that is refused. A store's answer is read on the reading
/// task's poller (REAS-2, sweep M14): [`Stopped::Cancelled`] where that stopped it, never a
/// "no sets" (fix check r1, rule 2).
trait Lookup<'r>: Sync {
    fn sets_of(&self, fact: &Triple, poll: &Poller<'_>) -> Result<Option<Looked<'_, 'r>>, Stopped>;
}

/// The sets a lookup found.
enum Looked<'k, 'r> {
    /// Where they are kept.
    Kept(Sets<'k>),
    /// One set held inline: nothing on the heap, nothing charged.
    One(GraphSet),
    /// Made for the lookup, under their charge on the run's working budget.
    Made(Held<'r>),
}

impl Looked<'_, '_> {
    fn sets(&self) -> Sets<'_> {
        match self {
            Self::Kept(sets) => *sets,
            Self::One(set) => Sets {
                list: List::Slice(std::slice::from_ref(set)),
                truncated: false,
                beyond_own: false,
            },
            Self::Made(held) => Sets::of(held),
        }
    }
}

/// An in-memory map: one lookup, which the reading derivation already counted.
impl<'r> Lookup<'r> for HashMap<Triple, Annotation> {
    fn sets_of(&self, fact: &Triple, _: &Poller<'_>) -> Result<Option<Looked<'_, 'r>>, Stopped> {
        Ok(self.get(fact).map(|found| Looked::Kept(Sets::of(found))))
    }
}

/// During an update: the facts it recomputes as they are now, the others as they were,
/// borrowed from where `previous` keeps them, or, for those it keeps none of
/// ([`Previous::Own`]), made from `graphs` and `axioms` (sorted) at `cap`, charged to `run`.
struct Recomputing<'a, 'p, 'r> {
    now: &'a HashMap<Triple, Annotation>,
    affected: &'a HashSet<Triple>,
    previous: &'a (dyn Fn(Triple, &Poller<'_>) -> Result<Previous<'p>, ReadStopped> + Sync),
    graphs: &'a dyn Graphs,
    axioms: &'a [Triple],
    cap: usize,
    run: &'a Run<'r>,
}

impl<'r> Lookup<'r> for Recomputing<'_, '_, 'r> {
    fn sets_of(&self, fact: &Triple, poll: &Poller<'_>) -> Result<Option<Looked<'_, 'r>>, Stopped> {
        if self.affected.contains(fact) {
            return self.now.sets_of(fact, poll);
        }
        match (self.previous)(*fact, poll).map_err(|_| self.run.stopped())? {
            Previous::Sets(sets) => Ok((!sets.is_empty()).then_some(Looked::Kept(sets))),
            Previous::Own => own_sets(*fact, self.graphs, self.axioms, self.cap, self.run, poll),
        }
    }
}

/// The graphs an own lookup keeps on the stack while it reads them ([`own_sets`]): 256 bytes,
/// past the 32 graphs a fact of support-product-1024 is asserted in; a fact in more is read
/// a second time. A stack buffer's size, not a limit: nothing is refused past it.
const OWN_READ_ONCE: usize = 64;

/// The sets `fact` starts with ([`initial_sets`] of its graphs; the empty set for an axiom,
/// one of `axioms`, sorted), made for a lookup: one graph's set held inline is made with
/// nothing allocated; any more are charged to `run` before they are made (fix check r2,
/// C1.b residual). `None` where it has none. Its graphs are read on `poll`; where that
/// stopped a read, [`Stopped::Cancelled`], before any set is made from a partly read list
/// (fix check r1, B11).
fn own_sets<'k, 'r>(
    fact: Triple,
    graphs: &dyn Graphs,
    axioms: &[Triple],
    cap: usize,
    run: &Run<'r>,
    poll: &Poller<'_>,
) -> Result<Option<Looked<'k, 'r>>, Stopped> {
    if axioms.binary_search(&fact).is_ok() {
        return Ok(Some(Looked::One(GraphSet::default())));
    }
    // Read once: the graphs kept on the stack as they are counted, read again only past
    // its room (a second snapshot scan per lookup cost the product's commit-2 update 13 %,
    // `p2c5-r4-memcount3-caded3cf`).
    let mut seen = [0u32; OWN_READ_ONCE];
    let (mut n, mut words) = (0usize, 0usize);
    graphs
        .each(fact, poll, &mut |graph| {
            if let Some(slot) = seen.get_mut(n) {
                *slot = graph;
            }
            n += 1;
            words = words.saturating_add(GraphSet::heap_bytes_of(graph));
        })
        .map_err(|_| run.stopped())?;
    match n {
        0 => Ok(None),
        1 if words == 0 && cap > 0 => Ok(Some(Looked::One(GraphSet::of(seen[0])))),
        _ => {
            // As `initial_sets`: `minimal` of the graphs' sets, each its own.
            let mut made = Charged::with_capacity(run, n, cap, words)?;
            match seen.get(..n) {
                Some(kept) => kept
                    .iter()
                    .for_each(|&graph| made.push(GraphSet::of(graph))),
                None => graphs
                    .each(fact, poll, &mut |graph| {
                        if made.sets.len() < n {
                            made.push(GraphSet::of(graph));
                        }
                    })
                    .map_err(|_| run.stopped())?,
            }
            let held = made.minimal(cap, &|_| true)?;
            Ok((!held.is_empty()).then_some(Looked::Made(held)))
        }
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
    // At most `cap` and at most the input: allocated once, at the size a caller charges.
    let mut out: Vec<GraphSet> = Vec::with_capacity(sets.len().min(cap));
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

/// Whether a set is one graph that asserts `fact` ([`minimal`]'s `own`): one question to
/// `graphs` per set asked, no list of them made, each counted on `poll`.
fn own_graph_of<'a>(
    fact: Triple,
    graphs: &'a dyn Graphs,
    poll: &'a Poller<'_>,
) -> impl Fn(&GraphSet) -> bool + 'a {
    // A stopped question reads as "not one's own": `minimal`'s loop over a fact's sets is
    // bounded by its cap, and the stage's check after it ends the run.
    move |set: &GraphSet| {
        let mut members = set.graphs();
        matches!((members.next(), members.next()), (Some(graph), None)
            if graphs.holds(fact, graph, poll) == Ok(true))
    }
}

/// The support graph sets of every fact of the closure `facts` under `program`.
/// `graphs_of` gives an asserted fact's graphs (none for an inferred one); `axioms`
/// (sorted) hold in no graph, for every reader (a ruleset's axiomatic triples); `cap`
/// bounds the sets kept per fact. `limits` see the working set's bytes after each round,
/// and stop it.
pub fn support_sets(
    facts: &[Triple],
    program: &GroundProgram,
    axioms: &[Triple],
    graphs_of: &dyn Graphs,
    cap: usize,
    limits: Limits<'_>,
) -> Result<HashMap<Triple, Annotation>, Stopped> {
    support_sets_run(facts, program, axioms, graphs_of, cap, &Run::new(limits))
}

/// [`support_sets`] under one run's limits.
fn support_sets_run(
    facts: &[Triple],
    program: &GroundProgram,
    axioms: &[Triple],
    graphs_of: &dyn Graphs,
    cap: usize,
    run: &Run<'_>,
) -> Result<HashMap<Triple, Annotation>, Stopped> {
    let fired = || run.fired();
    let all = Store::new(facts.to_vec());
    // Each fact's graphs, a store read, on one poller per parallel task (sweep M16). The first
    // read a stop ended ends them all, with the run's cause (fix check r1, rule 2).
    let initial: Vec<Option<(Triple, Annotation)>> = facts
        .par_iter()
        .map_init(
            || Poller::new(&fired),
            |poll, &fact| -> Result<Option<(Triple, Annotation)>, Stopped> {
                let poll = poll.as_ref().ok_or_else(|| run.stopped())?;
                let axiom = axioms.binary_search(&fact).is_ok();
                let graphs = match axiom {
                    true => Vec::new(),
                    false => listed(graphs_of, fact, poll).map_err(|_| run.stopped())?,
                };
                let initial = initial_sets(graphs, axiom, cap);
                Ok((!initial.is_empty()).then_some((fact, initial)))
            },
        )
        .collect::<Result<_, Stopped>>()?;
    let initial: Vec<(Triple, Annotation)> = initial.into_iter().flatten().collect();
    // The heap bytes of the annotations the map holds, kept as they change.
    let mut annotations: usize = initial.iter().map(|(_, a)| a.heap_bytes()).sum();
    let mut sets: HashMap<Triple, Annotation> = HashMap::with_capacity(facts.len());
    sets.extend(initial);
    let empty = Annotation::empty_set();
    // The sequential stages' poller (the premise maps, the bodiless and schema sets): each
    // premise alternative they read is attempted work (sol-loop r7, R7-1). The parallel
    // morsels make their own. Made after the initial sets: a stop during them ends it here.
    let poll = Poller::new(&fired).ok_or_else(|| run.stopped())?;
    // Facts without a body (ground from schema facts alone, `C subClassOf C` from
    // `C a owl:Class`): evaluated again whenever one of their premises changes.
    let mut bodiless_by_premise: HashMap<Triple, Vec<Triple>> = HashMap::new();
    for &fact in &program.bodiless {
        let grounding = Grounding::Bodiless(fact);
        program.for_each_alternative(grounding, &poll, |premises, _| {
            for &premise in premises {
                bodiless_by_premise.entry(premise).or_default().push(fact);
            }
            ControlFlow::<()>::Continue(())
        });
        if poll.stopped() {
            return Err(run.stopped());
        }
        let from = grounding_sets(program, grounding, &sets, cap, &empty, &poll, run)?;
        merge(
            &mut sets,
            fact,
            from.annotation.clone(),
            cap,
            graphs_of,
            &poll,
            &mut annotations,
        );
    }
    // A merge reads the head's own graphs: stopped there, the sets mean nothing.
    if poll.stopped() {
        return Err(run.stopped());
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
        program.for_each_alternative(grounding, &poll, |premises, _| {
            for &premise in premises {
                rules_by_premise.entry(premise).or_default().push(k);
            }
            ControlFlow::<()>::Continue(())
        });
        if poll.stopped() {
            return Err(run.stopped());
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
    run.check(held(&sets, annotations))?;
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
        // are truncated) triples, charged and reduced per head as they come ([`found`]).
        let mut bodiless_found = run.found();
        for fact in delta_facts(&delta, &poll).map_err(|_| run.stopped())? {
            for &bodiless in bodiless_by_premise.get(&fact).into_iter().flatten() {
                let grounding = Grounding::Bodiless(bodiless);
                let from = grounding_sets(program, grounding, &sets, cap, &empty, &poll, run)?;
                bodiless_found
                    .add_all(bodiless, &from.sets, from.truncated)
                    .map_err(Stopped::Refused)?;
            }
        }
        // The rules' schema parts, with their charges, for the round: one each, rule `k`'s at
        // `k`; jobs refer to them by index, never copy them (review r2 C1.b).
        let mut schemas: Vec<Held<'_>> = Vec::with_capacity(rules.len());
        for &(_, grounding) in &rules {
            // The schema facts' part, once per rule and round: the minimal sets over the
            // alternative groundings.
            let held = grounding_sets(program, grounding, &sets, cap, &empty, &poll, run)?;
            schemas.push(held);
        }
        // Their construction reads on the sequential poller (sweep M01), checked before use.
        let jobs = round_jobs(&source, &rules, &schemas, &whole, &poll);
        if poll.stopped() {
            return Err(run.stopped());
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
        let mut parts: Vec<Found> = tasks
            .par_iter()
            .map(|(j, range)| {
                let (job, heads, k) = &jobs[*j];
                let schema = &schemas[*k];
                let mut out = run.found();
                let mut body = Vec::with_capacity(job.rule.body.len());
                let Some(poll) = Poller::new(&fired) else {
                    return out;
                };
                // Refused, or a lookup stopped: the morsel's later bindings do nothing.
                let mut refused = false;
                let ran = job.run(&source, range.clone(), &poll, &mut |bindings| {
                    if refused {
                        return;
                    }
                    body.clear();
                    body.extend(
                        job.rule
                            .body
                            .iter()
                            .map(|atom| super::eval::instantiate_head(atom, bindings)),
                    );
                    let from = match premise_sets_from(&body, schema, sets_now, cap, run, &poll) {
                        Ok(from) => from,
                        Err(Stopped::Refused(exceeded)) => {
                            run.refuse(exceeded);
                            refused = true;
                            return;
                        }
                        // Stopped: the morsel ends, and the stage's check after it asks the
                        // run's latched stop (its cause).
                        Err(_) => {
                            refused = true;
                            return;
                        }
                    };
                    for head in heads.iter() {
                        let fact = super::eval::instantiate_head(head, bindings);
                        if let Err(exceeded) = out.add_all(fact, &from.sets, from.truncated) {
                            run.refuse(exceeded);
                            refused = true;
                            return;
                        }
                    }
                });
                // Stopped or refused part-way: nothing of it is kept; the round's check
                // ends the run.
                if ran.is_err() || poll.stopped() || refused {
                    return run.found();
                }
                // Reduced once, with the round's other parts ([`Run::gather`]).
                out
            })
            .collect();
        parts.push(bodiless_found);
        // The round's peak beside the sets: its delta and its morsels (its derivations are
        // charged, [`Limits::scratch`]).
        let round = delta.bytes().total()
            + jobs.capacity() * size_of::<(Job<'_>, &[super::ir::Atom], usize)>()
            + tasks.capacity() * size_of::<(usize, std::ops::Range<usize>)>()
            + parts.capacity() * size_of::<Found>();
        run.check(held(&sets, annotations) + round)?;
        drop((jobs, tasks));
        // Per head: its new sets merged into its old ones, the derivations sorted by head
        // and reduced ([`found`]: exactly what `minimal` keeps of them).
        let gathered = run.gather(parts)?;
        let found = gathered.entries();
        let mut starts: Vec<usize> = (0..found.len())
            .filter(|&i| i == 0 || found[i - 1].0 != found[i].0)
            .collect();
        starts.push(found.len());
        // In parallel against the sets as they are; then the changes written back. A
        // change of the mark alone is news too: the facts derived from it are marked.
        // A head's own graphs are a store read ([`own_graph_of`]): one poller per task.
        let updates: Vec<(Triple, Annotation)> = starts
            .par_windows(2)
            .map_init(
                || Poller::new(&fired),
                |poll, w| {
                    #[cfg(test)]
                    crate::witness::add(crate::witness::Site::HeadGroup, 1);
                    let poll = poll.as_ref()?;
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
                    // The copies `minimal` takes, charged first (sol-loop r1 S1).
                    let copied = words(kept.iter().chain(group.iter().map(|(_, set, _)| set)));
                    let chosen = Charged::with_capacity(run, kept.len() + group.len(), cap, copied)
                        .and_then(|mut all| {
                            kept.iter().for_each(|set| all.push(set.clone()));
                            group.iter().for_each(|(_, set, _)| all.push(set.clone()));
                            all.minimal(cap, &own_graph_of(fact, graphs_of, poll))
                        });
                    // Kept as the fact's sets: the retained sets' exemption (module doc).
                    let mut next = match chosen {
                        Ok(chosen) => chosen.into_retained(),
                        Err(exceeded) => {
                            run.refuse(exceeded);
                            return None;
                        }
                    };
                    next.truncated |= truncated || marks;
                    next.beyond_own |= beyond_own || marks;
                    (next.sets.as_slice() != kept
                        || next.truncated != truncated
                        || next.beyond_own != beyond_own)
                        .then_some((fact, next))
                },
            )
            .flatten()
            .collect();
        drop((gathered, starts));
        // A refused copy cut the heads short, or a task's poller stopped while it read their
        // graphs (the stop is the run's): none of it is written back.
        if run.fired() {
            return Err(run.stopped());
        }
        for (fact, next) in updates {
            annotations += next.heap_bytes();
            if let Some(old) = sets.insert(fact, next) {
                annotations = annotations.saturating_sub(old.heap_bytes());
            }
            changed.push(fact);
        }
        run.check(held(&sets, annotations))?;
    }
    Ok(sets)
}

/// A support round's jobs over `source`: of each rule with fact heads and a schema part
/// (`schemas[k]` for rule `k`), the instances with a changed premise (all of them where
/// `whole[k]`), each with its heads and `k`. They refer to their schema part by index and
/// never copy it: the one charged part serves them all (review r2 C1.b). Their construction
/// reads on `poll` (sweep M01); the caller checks it before the jobs are used.
fn round_jobs<'s, S: Source + ?Sized>(
    source: &'s S,
    rules: &[(&'s Rule, Grounding)],
    schemas: &[Held<'_>],
    whole: &[bool],
    poll: &Poller<'_>,
) -> Vec<(Job<'s>, &'s [super::ir::Atom], usize)> {
    let mut jobs = Vec::new();
    for (k, &(rule, _)) in rules.iter().enumerate() {
        let Head::Facts(heads) = &rule.head else {
            continue;
        };
        if schemas[k].is_empty() {
            continue;
        }
        // Instances with a changed premise; all of them when a schema fact changed.
        if whole[k] {
            jobs.extend(Job::full(source, rule, poll).map(|job| (job, heads.as_slice(), k)));
        } else {
            for i in 0..rule.body.len() {
                let seg_of = |j| if j == i { Seg::Delta } else { Seg::All };
                let job = Job::new(source, rule, i, seg_of, poll);
                jobs.extend(job.map(|job| (job, heads.as_slice(), k)));
            }
        }
    }
    jobs
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
    readable: &(dyn Fn(Triple, &Poller<'_>) -> Result<bool, ReadStopped> + Sync),
    limits: Limits<'_>,
) -> Result<HashMap<Triple, Annotation>, Stopped> {
    let one_graph = OneGraph(readable);
    let mut sets = support_sets(facts, program, axioms, &one_graph, 1, limits)?;
    sets.retain(|_, annotation| !annotation.is_empty());
    debug_assert!(sets.values().all(|annotation| !annotation.truncated));
    Ok(sets)
}

/// The facts a store callback accepts (a reader's statements, the asserted ones) as one
/// graph, 0, read on the asking task's poller: the callback, a store read, counts itself
/// (REAS-2, sweep M14).
struct OneGraph<'a>(&'a (dyn Fn(Triple, &Poller<'_>) -> Result<bool, ReadStopped> + Sync));

impl Graphs for OneGraph<'_> {
    fn each(
        &self,
        fact: Triple,
        poll: &Poller<'_>,
        graph: &mut dyn FnMut(u32),
    ) -> Result<(), ReadStopped> {
        if (self.0)(fact, poll)? {
            graph(0);
        }
        Ok(())
    }

    fn holds(&self, fact: Triple, graph: u32, poll: &Poller<'_>) -> Result<bool, ReadStopped> {
        Ok(graph == 0 && (self.0)(fact, poll)?)
    }
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
///
/// It counts on `poll` ([`Poller`], one per parallel task) each unit it attempts, found or
/// not: the fact, each producer, each search and each fact a search scans. Stopped, it
/// answers false, decided by nothing, and the caller asks its (latched) stop again before
/// it uses the answer.
pub fn derived_otherwise<S: Source + ?Sized>(
    source: &S,
    program: &GroundProgram,
    axioms: &[Triple],
    fact: Triple,
    poll: &Poller<'_>,
) -> bool {
    if poll.attempt(1) {
        return false;
    }
    if axioms.binary_search(&fact).is_ok() {
        return true;
    }
    // Each premise's probe counts too (REAS-2); stopped, it reads as absent.
    let holds = |premises: &[Triple], poll: &Poller<'_>| {
        premises
            .iter()
            .all(|&p| p != fact && source.contains(p, poll) == Ok(true))
    };
    // Each premise alternative is attempted work (sol-loop r7, R7-1); stopped, the grounding
    // reads as not holding, decided by nothing.
    let grounded = |grounding: Grounding, poll: &Poller<'_>| {
        program
            .for_each_alternative(grounding, poll, |premises, poll| {
                match holds(premises, poll) {
                    true => ControlFlow::Break(()),
                    false => ControlFlow::Continue(()),
                }
            })
            .is_some()
    };
    if program.bodiless.contains(&fact) && grounded(Grounding::Bodiless(fact), poll) {
        return true;
    }
    let derives = |rule: &Rule, poll: &Poller<'_>| {
        let mut found = false;
        let mut body = Vec::with_capacity(rule.body.len());
        derivations(source, rule, fact, Seg::All, poll, &mut |bindings| {
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
    // Each producer is attempted work before its schema check, an empty search or not
    // (review r1, F1).
    let by_rule = program.for_each_producer(fact, poll, |r, poll| {
        match grounded(Grounding::Rule(r), poll) && derives(&program.rules[r], poll) {
            true => ControlFlow::Break(()),
            false => ControlFlow::Continue(()),
        }
    });
    if by_rule.is_some() {
        return true;
    }
    let p = fact[1];
    program.transitive.contains(&p)
        && !poll.attempt(1)
        && grounded(Grounding::Transitive(p), poll)
        && derives(&transitivity(p), poll)
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
/// ([`closure_within`]), exact as well. Work: the cone's derivations; `limits` see its
/// working set after each round, and stop it.
pub fn independently_derived<S: Source + Sync + ?Sized>(
    source: &S,
    program: &GroundProgram,
    axioms: &[Triple],
    asserted: &(dyn Fn(Triple, &Poller<'_>) -> Result<bool, ReadStopped> + Sync),
    fact: Triple,
    limit: usize,
    limits: Limits<'_>,
) -> Result<Option<bool>, Stopped> {
    let run = Run::new(limits);
    independently_derived_run(source, program, axioms, asserted, fact, limit, &run)
}

/// [`independently_derived`] under one run's limits.
fn independently_derived_run<S: Source + Sync + ?Sized>(
    source: &S,
    program: &GroundProgram,
    axioms: &[Triple],
    asserted: &(dyn Fn(Triple, &Poller<'_>) -> Result<bool, ReadStopped> + Sync),
    fact: Triple,
    limit: usize,
    run: &Run<'_>,
) -> Result<Option<bool>, Stopped> {
    let derivable = Annotation {
        sets: vec![GraphSet::of(0)],
        ..Annotation::default()
    };
    let empty = Annotation::empty_set();
    // Lent, not copied per lookup.
    let previous = |other: Triple, _: &Poller<'_>| {
        Ok(Previous::Sets(Sets::of(
            match axioms.binary_search(&other).is_ok() {
                true => &empty,
                false => &derivable,
            },
        )))
    };
    let without = |other: Triple, poll: &Poller<'_>| Ok(other != fact && asserted(other, poll)?);
    let one_graph = OneGraph(&without);
    let change = Change {
        source,
        in_closure: &|other, poll| source.contains(other, poll),
        touched: &[fact],
        previous: &previous,
        limit,
    };
    let found = update_support_sets_run(&change, program, axioms, &one_graph, 1, run)?;
    Ok(found.map(|sets| sets.get(&fact).is_some_and(|found| !found.is_empty())))
}

/// The facts of `candidates` (sorted, each asserted in the closure `source`) that follow
/// from the closure's other facts without their own assertion, sorted, and those this
/// can't decide, sorted. A one-step derivation from other facts ([`derived_otherwise`],
/// checked in parallel) is necessary; [`independently_derived`] then decides each in
/// turn. The undecided ones (their cone reaches a schema fact or passes `limit`) the
/// caller decides with the closure of the asserted facts without each ([`closure_within`]).
/// `limits` see the candidates' bytes before the checks, and each check's working set, and
/// stop them.
pub fn rederived<S: Source + Sync + ?Sized>(
    source: &S,
    program: &GroundProgram,
    axioms: &[Triple],
    asserted: &(dyn Fn(Triple, &Poller<'_>) -> Result<bool, ReadStopped> + Sync),
    candidates: &[Triple],
    limit: usize,
    limits: Limits<'_>,
) -> Result<(Vec<Triple>, Vec<Triple>), Stopped> {
    let run = Run::new(limits);
    run.check(size_of_val(candidates))?;
    let fired = || run.fired();
    // One poller per parallel task, not per candidate: a candidate whose search scans
    // nothing then costs no poll (a poll reads the process's memory limit).
    let one_step: Vec<Triple> = candidates
        .par_iter()
        .copied()
        .map_init(
            || Poller::new(&fired),
            |poll, fact| {
                #[cfg(test)]
                crate::witness::add(crate::witness::Site::OneStepCandidate, 1);
                let poll = poll.as_ref()?;
                derived_otherwise(source, program, axioms, fact, poll).then_some(fact)
            },
        )
        .flatten_iter()
        .collect();
    run.check((candidates.len() + 2 * one_step.capacity()) * size_of::<Triple>())?;
    let (mut found, mut undecided) = (Vec::with_capacity(one_step.len()), Vec::new());
    for fact in one_step {
        if axioms.binary_search(&fact).is_ok() {
            found.push(fact);
            continue;
        }
        match independently_derived_run(source, program, axioms, asserted, fact, limit, &run)? {
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
    limits: Limits<'_>,
) -> Result<Option<Vec<Triple>>, Stopped> {
    let run = Run::new(limits);
    let schema = program.schema_premises();
    let rules = body_rules(program);
    let rules: Vec<(&Rule, Grounding)> =
        rules.iter().map(|(rule, g)| (rule.as_ref(), *g)).collect();
    let fixed = table_bytes::<Triple, ()>(schema.capacity());
    let Some(cone) = forward_cone(source, &rules, &schema, touched, limit, fixed, &run)? else {
        return Ok(None);
    };
    let mut cone: Vec<Triple> = cone.into_iter().collect();
    run.check(fixed + cone.capacity() * size_of::<Triple>())?;
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
/// `run` sees `fixed` and what it holds after each round.
fn forward_cone<S: Source + Sync + ?Sized>(
    source: &S,
    rules: &[(&Rule, Grounding)],
    schema: &HashSet<Triple>,
    touched: &[Triple],
    limit: usize,
    fixed: usize,
    run: &Run<'_>,
) -> Result<Option<HashSet<Triple>>, Stopped> {
    let mut affected: HashSet<Triple> = touched.iter().copied().collect();
    if affected.iter().any(|fact| schema.contains(fact)) {
        return Ok(None);
    }
    let none = GraphSet::default();
    let mut frontier: Vec<Triple> = affected.iter().copied().collect();
    while !frontier.is_empty() {
        frontier.sort_unstable();
        let delta = Store::new(std::mem::take(&mut frontier));
        let changed = Changed {
            all: source,
            changed: &delta,
        };
        // The heads, without sets: the reduction keeps each once.
        let heads = instances(&changed, rules, run, &|k, bindings, _, out| {
            let rule = rules[k].0;
            if let Head::Facts(heads) = &rule.head {
                for head in heads {
                    out.add(instantiate_head(head, bindings), &none, false)?;
                }
            }
            Ok(())
        })?;
        run.check(fixed + table_bytes::<Triple, ()>(affected.capacity()) + delta.bytes().total())?;
        for &(head, ..) in heads.entries() {
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
    /// Whether a fact is in the closure after the change, read on the reading task's
    /// poller (as `previous` is: store reads, REAS-2 sweep M14); `Err` where that stopped it.
    pub in_closure: &'a (dyn Fn(Triple, &Poller<'_>) -> Result<bool, ReadStopped> + Sync),
    /// Every fact whose own graphs changed, or that the change added to the closure or
    /// removed from it.
    pub touched: &'a [Triple],
    /// A fact's sets before the change, with its mark: lent from where the caller keeps them
    /// for the whole update, never copied for it (fix check r2, C1.b residual); `Err` where
    /// the reading task's poller stopped it.
    pub previous: &'a (dyn Fn(Triple, &Poller<'_>) -> Result<Previous<'a>, ReadStopped> + Sync),
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
/// all. `limits` see the working set's bytes after each round, and stop it.
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
    graphs_of: &dyn Graphs,
    cap: usize,
    limits: Limits<'_>,
) -> Result<Option<HashMap<Triple, Annotation>>, Stopped> {
    update_support_sets_run(change, program, axioms, graphs_of, cap, &Run::new(limits))
}

/// [`update_support_sets`] under one run's limits.
fn update_support_sets_run<'r, S: Source + Sync + ?Sized>(
    change: &Change<'_, S>,
    program: &GroundProgram,
    axioms: &[Triple],
    graphs_of: &dyn Graphs,
    cap: usize,
    run: &Run<'r>,
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
        run,
    )?
    else {
        return Ok(None);
    };
    // The sequential stages' poller: the members' probes, their graphs, the schema sets,
    // the merges (store reads, sweep M14); each stage's check comes before its output is
    // used.
    let fired = || run.fired();
    let stage = Poller::new(&fired).ok_or_else(|| run.stopped())?;
    // The first read its poller stopped ends the update with the run's cause (fix check r1,
    // SW111: a stopped probe read as "not in the closure" walked every affected fact).
    let mut members: HashSet<Triple> = HashSet::new();
    for &fact in affected.iter() {
        if (change.in_closure)(fact, &stage).map_err(|_| run.stopped())? {
            members.insert(fact);
        }
    }
    let empty = Annotation::empty_set();
    let mut now: HashMap<Triple, Annotation> = HashMap::with_capacity(members.len());
    for &fact in &members {
        let axiom = axioms.binary_search(&fact).is_ok();
        let graphs = match axiom {
            true => Vec::new(),
            false => listed(graphs_of, fact, &stage).map_err(|_| run.stopped())?,
        };
        now.insert(fact, initial_sets(graphs, axiom, cap));
    }
    if stage.stopped() {
        return Err(run.stopped());
    }
    let mut annotations: usize = now.values().map(Annotation::heap_bytes).sum();
    let fixed = fixed
        + table_bytes::<Triple, ()>(affected.capacity())
        + table_bytes::<Triple, ()>(members.capacity());
    let held = |now: &HashMap<Triple, Annotation>, annotations: usize| {
        fixed + table_bytes::<Triple, Annotation>(now.capacity()) + annotations
    };
    run.check(held(&now, annotations))?;
    // The schema facts' part per rule: they aren't affected, so it is fixed. A sequential
    // stage on the stage's poller (sol-loop r7, R7-1), checked before its sets are used.
    let schemas: Vec<Held<'_>> = {
        let lookup = Recomputing {
            now: &now,
            affected: &affected,
            previous: change.previous,
            graphs: graphs_of,
            axioms,
            cap,
            run,
        };
        rules
            .iter()
            .map(|&(_, grounding)| {
                grounding_sets(program, grounding, &lookup, cap, &empty, &stage, run)
            })
            .collect::<Result<_, Stopped>>()?
    };
    if stage.stopped() {
        return Err(run.stopped());
    }
    run.check(held(&now, annotations))?;
    let by_grounding: HashMap<Grounding, usize> = rules
        .iter()
        .enumerate()
        .map(|(k, &(_, grounding))| (grounding, k))
        .collect();
    // `Err` at the first premise its poller stopped.
    let premises_in_closure = |body: &[Triple], poll: &Poller<'_>| -> Result<bool, ReadStopped> {
        for &premise in body {
            if !(change.in_closure)(premise, poll)? {
                return Ok(false);
            }
        }
        Ok(true)
    };
    // Every derivation of every affected fact, against the sets as they start: each fact's
    // sets charged and kept once as they come, moved in, not copied ([`Gathered`]), then held
    // as the base held them, a fact and its annotation, their charges in `pool` (declared
    // first: dropped after them).
    let pool = Pooled::new(run.scratch);
    let found: Vec<(Triple, Annotation)> = {
        let lookup = Recomputing {
            now: &now,
            affected: &affected,
            previous: change.previous,
            graphs: graphs_of,
            axioms,
            cap,
            run,
        };
        let list: Vec<Triple> = members.iter().copied().collect();
        // One poller per parallel task, not per derivation search (as `rederived`).
        list.par_iter()
            .map_init(
                || Poller::new(&fired),
                |poll, &fact| {
                    // The fact is attempted work before either prelude (its bodiless sets,
                    // its producers), and so is each grounding (review r1, F1; sol-loop r6,
                    // R6-1). Stopped, or no poller (stopped before the task began): the run's
                    // cause, which ends the pass (fix check r1, rule 2: an empty answer per
                    // fact walked every member after the stop).
                    let poll = poll.as_ref().ok_or_else(|| run.stopped())?;
                    if poll.attempt(1) {
                        return Err(run.stopped());
                    }
                    // Charged and kept once per set as they come ([`Gathered`]); a refusal
                    // latches the run, which its pollers see. A derivation's mark counts
                    // where it gave a set (as an entry's did).
                    let mut from = Gathered::new(run.scratch);
                    let (mut marked, mut refused) = (false, false);
                    let mut take = |found: Result<Held<'r>, Stopped>| {
                        if refused {
                            return;
                        }
                        let added = found.and_then(|found| {
                            marked |= found.truncated && !found.is_empty();
                            from.take(found).map_err(Stopped::Refused)
                        });
                        match added {
                            Ok(()) => {}
                            Err(Stopped::Refused(exceeded)) => {
                                run.refuse(exceeded);
                                refused = true;
                            }
                            Err(_) => refused = true,
                        }
                    };
                    if program.bodiless.contains(&fact) {
                        take(grounding_sets(
                            program,
                            Grounding::Bodiless(fact),
                            &lookup,
                            cap,
                            &empty,
                            poll,
                            run,
                        ));
                    }
                    // Each grounding's derivations of the fact, against its schema part.
                    let mut search = |grounding: Grounding, poll: &Poller<'_>| {
                        let Some(&k) = by_grounding.get(&grounding) else {
                            return;
                        };
                        let (rule, schema) = (rules[k].0, &schemas[k]);
                        if schema.is_empty() {
                            return;
                        }
                        let mut body = Vec::with_capacity(rule.body.len());
                        derivations(change.source, rule, fact, Seg::All, poll, &mut |bindings| {
                            body.clear();
                            body.extend(
                                rule.body
                                    .iter()
                                    .map(|atom| instantiate_head(atom, bindings)),
                            );
                            // Stopped: no sets taken, the search ends at its next read, and
                            // the fact's check below answers the run's cause.
                            if premises_in_closure(&body, poll) == Ok(true) {
                                take(premise_sets_from(&body, schema, &lookup, cap, run, poll));
                            }
                            false
                        });
                    };
                    // Each producer is attempted work (for_each_producer), and so is the
                    // transitivity case.
                    program.for_each_producer(fact, poll, |r, poll| {
                        search(Grounding::Rule(r), poll);
                        ControlFlow::<()>::Continue(())
                    });
                    if program.transitive.contains(&fact[1]) && !poll.attempt(1) {
                        search(Grounding::Transitive(fact[1]), poll);
                    }
                    // A search its poller or a refusal cut short found a part (B9).
                    if refused || poll.stopped() {
                        return Err(run.stopped());
                    }
                    // Its copies are `minimal`'s to sort and drop, as before.
                    Ok((fact, from.into_annotation(marked, &pool)))
                },
            )
            .collect::<Result<_, Stopped>>()?
    };
    run.check(held(&now, annotations) + found.capacity() * size_of::<(Triple, Annotation)>())?;
    let mut changed: Vec<Triple> = Vec::new();
    for (fact, from) in found {
        let merged = merge_owned(
            run,
            &mut now,
            fact,
            from,
            cap,
            graphs_of,
            &stage,
            &mut annotations,
        )?;
        if merged {
            changed.push(fact);
        }
    }
    // A merge reads the head's own graphs: stopped there, the sets mean nothing.
    if stage.stopped() {
        return Err(run.stopped());
    }
    // Every fact's derivations merged (and freed, or kept as its sets): their charge goes.
    drop(pool);
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
                graphs: graphs_of,
                axioms,
                cap,
                run,
            };
            instances(&source, &rules, run, &|k, bindings, poll, out| {
                let rule = rules[k].0;
                let Head::Facts(heads) = &rule.head else {
                    return Ok(());
                };
                let body: Vec<Triple> = rule
                    .body
                    .iter()
                    .map(|atom| instantiate_head(atom, bindings))
                    .collect();
                // This callback's own error is a refusal: a read its poller stopped ends the
                // callback, and `instances`' morsel ends on the latched poller, the stage's
                // check after it answering the run's cause.
                if premises_in_closure(&body, poll) != Ok(true) {
                    return Ok(());
                }
                let from = match premise_sets_from(&body, &schemas[k], &lookup, cap, run, poll) {
                    Ok(from) => from,
                    Err(Stopped::Refused(exceeded)) => return Err(exceeded),
                    Err(_) => return Ok(()),
                };
                for head in heads {
                    let fact = instantiate_head(head, bindings);
                    if members.contains(&fact) && !from.is_empty() {
                        out.add_all(fact, &from.sets, from.truncated)?;
                    }
                }
                Ok(())
            })?
        };
        run.check(held(&now, annotations) + delta.bytes().total())?;
        // Sorted by fact: each fact's derivations merged into its sets.
        let entries = found.entries();
        let mut start = 0;
        while start < entries.len() {
            let fact = entries[start].0;
            let end = start + entries[start..].partition_point(|e| e.0 == fact);
            let from = &entries[start..end];
            let marked = from.iter().any(|&(_, _, marked)| marked);
            let sets = from.iter().map(|(_, set, _)| set);
            if merge_found(
                run,
                &mut now,
                fact,
                sets,
                marked,
                cap,
                graphs_of,
                &stage,
                &mut annotations,
            )? {
                changed.push(fact);
            }
            start = end;
        }
        if stage.stopped() {
            return Err(run.stopped());
        }
    }
    Ok(Some(now))
}

/// The bytes of a list of `n` sets' slots.
fn slots(n: usize) -> usize {
    n.saturating_mul(size_of::<GraphSet>())
}

/// The bytes a copy of `sets` holds beyond its slots: the words of sets past 128 graphs.
fn words<'s>(sets: impl IntoIterator<Item = &'s GraphSet>) -> usize {
    sets.into_iter().map(GraphSet::heap_bytes).sum()
}

/// [`merge`] of one fact's derivations, their sets `found` borrowed (marked if one was:
/// [`merge`] reads the mark as one beyond the fact's own graphs), into its sets in `now`:
/// their copy (its slots and words) charged before it is made (sol-loop r1 S1), then
/// [`merge_owned`].
#[allow(clippy::too_many_arguments)]
fn merge_found<'s>(
    run: &Run<'_>,
    now: &mut HashMap<Triple, Annotation>,
    fact: Triple,
    found: impl ExactSizeIterator<Item = &'s GraphSet> + Clone,
    marked: bool,
    cap: usize,
    graphs_of: &dyn Graphs,
    poll: &Poller<'_>,
    annotations: &mut usize,
) -> Result<bool, Stopped> {
    let _copy = run
        .room(slots(found.len()) + words(found.clone()))
        .map_err(Stopped::Refused)?;
    let from = Annotation {
        sets: found.cloned().collect(),
        truncated: marked,
        beyond_own: false,
    };
    merge_owned(run, now, fact, from, cap, graphs_of, poll, annotations)
}

/// [`merge`] of one fact's derivations it holds, `from` (charged by its holder), into its
/// sets in `now`, moved, not copied (the base's merge; a copy of a fact's first-pass sets was
/// the product's commit-2 peak, 1 MiB above the base's, phase `upd: first merge` of
/// `p2c5-r4-memprof5-1809b4a9`): `merge`'s list, at its final capacity, of the fact's sets
/// (copied: slots and words) with `from`'s (moved: slots only), charged first (sol-loop r1
/// S1, r2 S1/S2). The fact's own graphs are read on `poll`.
#[allow(clippy::too_many_arguments)]
fn merge_owned(
    run: &Run<'_>,
    now: &mut HashMap<Triple, Annotation>,
    fact: Triple,
    from: Annotation,
    cap: usize,
    graphs_of: &dyn Graphs,
    poll: &Poller<'_>,
    annotations: &mut usize,
) -> Result<bool, Stopped> {
    let kept = now.get(&fact).map_or(&[][..], |a| a.sets.as_slice());
    let _room = run
        .room(slots(kept.len() + from.sets.len()) + words(kept))
        .map_err(Stopped::Refused)?;
    Ok(merge(now, fact, from, cap, graphs_of, poll, annotations))
}

/// An annotation of the support algebra and the charge of what its sets hold, on the run's
/// working budget: the caller holds both until it has taken the sets (review r1 C1, sol-loop
/// r5 S1). Fields drop in order: the sets before their charge.
struct Held<'a> {
    annotation: Annotation,
    /// `None` where nothing is charged: a zero-byte reservation's drop still runs the
    /// budget's atomics, once per derivation on the inline path (sol-loop r11 S1).
    _charge: Option<Reservation<&'a Budget>>,
}

impl<'a> Held<'a> {
    /// `annotation`, with nothing more charged for it (sets held inline, or nothing).
    fn uncharged(annotation: Annotation, _run: &Run<'a>) -> Self {
        Self {
            annotation,
            _charge: None,
        }
    }
}

impl<'a> Held<'a> {
    /// The annotation alone, its charge released: for sets the evaluation keeps (the
    /// retained sets, under the W1 exemption of the module doc).
    fn into_retained(self) -> Annotation {
        let Self { annotation, .. } = self;
        annotation
    }
}

/// Sets made under a charge taken before they were, owned together: the storage drops
/// before its charge, and [`Self::minimal`] consumes the sets while the charge holds, so the
/// release-before-free order can't be written by hand (sol-loop r5–r7, three rounds of it;
/// the master, 8 Nov 05:10).
struct Charged<'a> {
    /// Before `charge`: fields drop in order.
    sets: Vec<GraphSet>,
    charge: Reservation<&'a Budget>,
}

impl<'a> Charged<'a> {
    /// Room for `n` sets, for the output of the [`Self::minimal`] to come at `cap` (0: none
    /// comes), and `words` bytes beside them (the sets' words), charged before anything is
    /// allocated: the output's room is the type's to charge, not each caller's to remember.
    fn with_capacity(
        run: &Run<'a>,
        n: usize,
        cap: usize,
        words: usize,
    ) -> Result<Self, BudgetExceeded> {
        let bytes = slots(n)
            .saturating_add(slots(n.min(cap)))
            .saturating_add(words);
        let charge = run.room(bytes)?;
        Ok(Self {
            sets: Vec::with_capacity(n),
            charge,
        })
    }

    /// One more set, within the room charged.
    fn push(&mut self, set: GraphSet) {
        debug_assert!(
            self.sets.len() < self.sets.capacity(),
            "past the room charged"
        );
        self.sets.push(set);
    }

    /// `minimal` of the sets, made while their charge holds, with the charge of what its
    /// output holds alone.
    fn minimal(
        self,
        cap: usize,
        own: &dyn Fn(&GraphSet) -> bool,
    ) -> Result<Held<'a>, BudgetExceeded> {
        let Self { sets, mut charge } = self;
        let out = minimal(sets, cap, own);
        charge.resize(slots(out.sets.capacity()) + words(&out.sets))?;
        Ok(Held {
            annotation: out,
            _charge: Some(charge),
        })
    }
}

/// One set under the charge of its words (the singleton fast path's union), owned together:
/// the set drops before its charge; each replacement is charged before it is made, the one
/// it replaces released after it is freed (review r2 C1.a). A set held inline costs no charge.
struct ChargedSet<'a> {
    /// Before `charge`: fields drop in order.
    set: GraphSet,
    /// `None` until something is charged: an inline set costs no budget call at all.
    charge: Option<Reservation<&'a Budget>>,
    budget: &'a Budget,
}

impl<'a> ChargedSet<'a> {
    /// A copy of `set`, its words charged first.
    fn copy(set: &GraphSet, budget: &'a Budget) -> Result<Self, BudgetExceeded> {
        let mut copy = Self {
            set: GraphSet::default(),
            charge: None,
            budget,
        };
        copy.charge(set.heap_bytes())?;
        copy.set = set.clone();
        Ok(copy)
    }

    /// `bytes` more charged, where there are any.
    fn charge(&mut self, bytes: usize) -> Result<(), BudgetExceeded> {
        if bytes > 0 {
            let budget = self.budget;
            self.charge
                .get_or_insert_with(|| Reservation::new(budget))
                .charge(bytes)?;
        }
        Ok(())
    }

    /// Becomes its union with `other`: the union's words (at most the longer operand's)
    /// charged before it is made, the old set's released after it is freed.
    fn union_with(&mut self, other: &GraphSet) -> Result<(), BudgetExceeded> {
        self.charge(self.set.heap_bytes().max(other.heap_bytes()))?;
        let next = self.set.union(other);
        let old = std::mem::replace(&mut self.set, next);
        let freed = old.heap_bytes();
        drop(old);
        if let Some(charge) = &mut self.charge {
            charge.release(freed);
        }
        Ok(())
    }

    /// The set as a one-set annotation: where it holds words, its list's slot is charged
    /// too before the list is made.
    fn into_held(mut self, truncated: bool) -> Result<Held<'a>, BudgetExceeded> {
        if self
            .charge
            .as_ref()
            .is_some_and(|charge| charge.bytes() > 0)
        {
            // Refused here, `self` drops whole: the set before its charge.
            self.charge(slots(1))?;
        }
        let Self { set, charge, .. } = self;
        Ok(Held {
            annotation: Annotation {
                sets: vec![set],
                truncated,
                beyond_own: truncated,
            },
            _charge: charge,
        })
    }
}

/// Charges a parallel pass's tasks hand over for what they keep, held as one on `budget` and
/// released when it drops: declared before what they charge, it drops after it, also as an
/// unwinding pass ends (free before release).
struct Pooled<'a> {
    budget: &'a Budget,
    bytes: AtomicUsize,
}

impl<'a> Pooled<'a> {
    fn new(budget: &'a Budget) -> Self {
        Self {
            budget,
            bytes: AtomicUsize::new(0),
        }
    }

    /// `bytes` handed over (`Reservation::transfer`): held until this drops.
    fn add(&self, bytes: usize) {
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

impl Drop for Pooled<'_> {
    fn drop(&mut self) {
        drop(Reservation::adopt(self.budget, *self.bytes.get_mut()));
    }
}

/// Sets gathered as they come under one budget, owned together: their list (charged as it
/// grows) drops before their words' charge. Each set kept once as they come, at doubling
/// thresholds ([`found`]'s reduction, without the fact: a fact's derivations take a set's
/// slot each, as the base's did; merge-time count `p2c5-r4-memcount3-caded3cf`).
struct Gathered<'a> {
    /// Before `words`: fields drop in order.
    sets: ChargedVec<GraphSet, &'a Budget>,
    words: Reservation<&'a Budget>,
    /// The length at which the next reduction runs: twice what the last one kept.
    reduce_at: usize,
    budget: &'a Budget,
}

impl<'a> Gathered<'a> {
    fn new(budget: &'a Budget) -> Self {
        Self {
            sets: ChargedVec::new(budget),
            words: Reservation::new(budget),
            reduce_at: found::REDUCE_AT,
            budget,
        }
    }

    /// Its sets sorted, each once, the dropped copies' words released.
    fn reduce(&mut self) {
        let sets = &mut self.sets[..];
        sets.sort_unstable();
        let mut write = 0;
        for i in 0..sets.len() {
            if write > 0 && sets[write - 1] == sets[i] {
                continue;
            }
            sets.swap(write, i);
            write += 1;
        }
        let dropped: usize = sets[write..].iter().map(GraphSet::heap_bytes).sum();
        self.sets.truncate(write);
        self.words.release(dropped);
        self.reduce_at = found::REDUCE_AT.max(2 * write);
    }

    /// Its sets as an annotation marked `truncated`, and the bytes charged for them (their
    /// slots and words), handed over to `pool`, which keeps them charged while the sets
    /// live: a fact's derivations held as the base held them, a fact and its sets.
    fn into_annotation(self, truncated: bool, pool: &Pooled<'a>) -> Annotation {
        let Self { sets, words, .. } = self;
        let (sets, slots) = sets.into_parts();
        pool.add(slots.transfer() + words.transfer());
        Annotation {
            sets,
            truncated,
            beyond_own: false,
        }
    }

    /// `held`'s sets moved in, `held` whole until they are (sol-loop r9 S5): the list's room
    /// charged first; refused, or unwinding, `held` drops as it was, its sets before their
    /// charge. Moved, its old list is freed: of its charge, its slots' part is released and
    /// its sets' words' part handed over, not charged again.
    fn take(&mut self, mut held: Held<'a>) -> Result<(), BudgetExceeded> {
        if self.sets.len() >= self.reduce_at {
            self.reduce();
        }
        let n = held.sets.len();
        let moved_words = words(&held.sets);
        let sets = &mut held.annotation.sets;
        self.sets.fill(n, |room| {
            std::mem::take(sets)
                .into_iter()
                .for_each(|set| room.push(set))
        })?;
        let mut words_part =
            Reservation::adopt(self.budget, held._charge.map_or(0, Reservation::transfer));
        words_part.resize(moved_words)?;
        let mine = std::mem::replace(&mut self.words, Reservation::new(self.budget));
        self.words = Reservation::adopt(self.budget, mine.transfer() + words_part.transfer());
        Ok(())
    }

    /// `minimal` of the sets (its output's slots charged first, then every charge they hold
    /// made one, so nothing is charged twice; sol-loop r6 S3), with the charge of what the
    /// output holds alone.
    fn choose(self, cap: usize, run: &Run<'a>) -> Result<Held<'a>, BudgetExceeded> {
        // Refused here, the whole drops: the list before the words.
        let out = run.room(slots(self.sets.len().min(cap)))?;
        let Self { sets, words, .. } = self;
        let (sets, slots_held) = sets.into_parts();
        let bytes = out.transfer() + slots_held.transfer() + words.transfer();
        Charged {
            sets,
            charge: Reservation::adopt(&**run.scratch, bytes),
        }
        .minimal(cap, &|_| false)
    }
}

impl std::ops::Deref for Held<'_> {
    type Target = Annotation;

    fn deref(&self) -> &Annotation {
        &self.annotation
    }
}

/// What `emit` adds of every instance of `rules` (by index) with a premise among
/// `source`'s delta facts, the instances in parallel morsels, each polling `run`'s stop and
/// charged and reduced per fact ([`found`]); then all of them, sorted by fact. `Err` where
/// `run` stopped (its caller, the process's memory, or a charge `emit` was refused).
/// `emit` reads on the morsel's poller.
fn instances<'r, S, E>(
    source: &S,
    rules: &[(&Rule, Grounding)],
    run: &Run<'r>,
    emit: &E,
) -> Result<Found<'r>, Stopped>
where
    S: Source + Sync + ?Sized,
    E: Fn(usize, &[Option<u64>], &Poller<'_>, &mut Found<'r>) -> Result<(), BudgetExceeded> + Sync,
{
    let fired = || run.fired();
    // The jobs' construction reads (estimates, collected drivers) on one sequential poller
    // (sweep M01, M13).
    let stage = Poller::new(&fired).ok_or_else(|| run.stopped())?;
    let stage = &stage;
    let jobs: Vec<(usize, Job<'_>)> = rules
        .iter()
        .enumerate()
        .flat_map(|(k, &(rule, _))| {
            (0..rule.body.len()).filter_map(move |i| {
                let seg_of = |j| if j == i { Seg::Delta } else { Seg::All };
                Job::new(source, rule, i, seg_of, stage).map(|job| (k, job))
            })
        })
        .collect();
    if stage.stopped() {
        return Err(run.stopped());
    }
    let tasks: Vec<(usize, std::ops::Range<usize>)> = jobs
        .iter()
        .enumerate()
        .flat_map(|(j, (_, job))| {
            (0..job.drivers())
                .step_by(MORSEL)
                .map(move |start| (j, start..(start + MORSEL).min(job.drivers())))
        })
        .collect();
    let parts: Vec<Found> = tasks
        .par_iter()
        .map(|(j, range)| {
            let (k, job) = &jobs[*j];
            let mut out = run.found();
            let Some(poll) = Poller::new(&fired) else {
                return out;
            };
            let mut refused = false;
            let ran = job.run(source, range.clone(), &poll, &mut |bindings| {
                if refused {
                    return;
                }
                if let Err(exceeded) = emit(*k, bindings, &poll, &mut out) {
                    run.refuse(exceeded);
                    refused = true;
                }
            });
            // Stopped or refused part-way: nothing of it is kept; `gather` ends the run.
            if ran.is_err() || poll.stopped() || refused {
                return run.found();
            }
            // Reduced once, with the other parts ([`Run::gather`]).
            out
        })
        .collect();
    run.gather(parts)
}

/// Every fact of `store`, read on `poll`; `Err` where that stopped the read, never a part.
fn delta_facts(store: &Store, poll: &Poller<'_>) -> Result<Vec<Triple>, ReadStopped> {
    let mut out = Vec::new();
    store.scan([None, None, None], Seg::All, poll, &mut |f| out.push(f))?;
    Ok(out)
}

/// The sets of the schema facts `grounding` was grounded on: over its alternatives, the
/// minimal unions of one set per premise, marked if an alternative's were or the cap
/// cut them; none if no alternative has sets yet.
///
/// Each alternative is a unit of attempted work on `poll`, counted before its premises are
/// read (sol-loop r7, R7-1): stopped, the sets are incomplete, and the stage's check ends
/// the run before they are used.
///
/// Every alternative's sets are held together until `minimal` chooses among them: their
/// slots and words are charged to `run`'s working budget as they are added, and held
/// through `minimal` (review r1 C1); `Err` where it has no room.
fn grounding_sets<'a, L: Lookup<'a> + ?Sized>(
    program: &GroundProgram,
    grounding: Grounding,
    sets: &L,
    cap: usize,
    empty: &Annotation,
    poll: &Poller<'_>,
    run: &Run<'a>,
) -> Result<Held<'a>, Stopped> {
    let mut all = Gathered::new(run.scratch);
    let (mut truncated, mut ended) = (false, None);
    program.for_each_alternative(grounding, poll, |premises, poll| {
        let added = premise_sets_from(premises, empty, sets, cap, run, poll).and_then(|found| {
            truncated |= found.truncated;
            all.take(found).map_err(Stopped::Refused)
        });
        match added {
            Ok(()) => ControlFlow::Continue(()),
            Err(stopped) => {
                ended = Some(stopped);
                ControlFlow::Break(())
            }
        }
    });
    if let Some(stopped) = ended {
        return Err(stopped);
    }
    // Alternatives its poller cut short are a part of them (fix check r1, B9).
    if poll.stopped() {
        return Err(run.stopped());
    }
    let mut held = all.choose(cap, run)?;
    // A derivation's sets: whatever they lack is beyond the head's own graphs.
    held.annotation.truncated |= truncated;
    held.annotation.beyond_own |= truncated;
    Ok(held)
}

/// The minimal unions of the sets `start` with one set per premise of `body`, marked if
/// `start`'s, a premise's or a step's were; none if a premise has none yet.
///
/// Each step's product (every union of a kept set with one of the premise's) is charged to
/// `run`'s working budget before it is built and held until `minimal` has consumed it: its
/// slots, and at most the longer operand's words per union (review r1 C1); `Err` where it
/// has no room. Exact: `minimal` sees the whole product, as before. A premise's sets are read
/// where they are kept ([`Lookup`]), never copied first (fix check r2, C1.b residual), on
/// `poll`.
fn premise_sets_from<'a, L: Lookup<'a> + ?Sized>(
    body: &[Triple],
    start: &Annotation,
    sets: &L,
    cap: usize,
    run: &Run<'a>,
    poll: &Poller<'_>,
) -> Result<Held<'a>, Stopped> {
    // A budget refusal is `Stopped::Refused` (its `From`), a lookup its poller stopped the
    // run's cause (`Lookup::sets_of`).
    // The usual case, one set everywhere: a single union, nothing to minimise. Its copies
    // charged before they are made (review r2 C1.a); held inline, nothing is charged, so a
    // derivation makes no budget call.
    if let [only] = start.sets.as_slice() {
        let mut one = ChargedSet::copy(only, run.scratch)?;
        let mut truncated = start.truncated;
        let mut single = true;
        for premise in body {
            let Some(found) = sets.sets_of(premise, poll)? else {
                return Ok(Held::uncharged(Annotation::default(), run));
            };
            let own = found.sets();
            match own.only() {
                Some(set) => {
                    one.union_with(set)?;
                    truncated |= own.truncated;
                }
                None => {
                    single = false;
                    break;
                }
            }
        }
        if single {
            return Ok(one.into_held(truncated)?);
        }
    }
    // The product as it stands, with its charge, until the next replaces it (sol-loop r5 S1).
    let mut product = {
        let mut copy = Charged::with_capacity(run, start.sets.len(), 0, words(&start.sets))?;
        start.sets.iter().for_each(|set| copy.push(set.clone()));
        Held {
            annotation: Annotation {
                sets: copy.sets,
                truncated: start.truncated,
                beyond_own: start.beyond_own,
            },
            _charge: Some(copy.charge),
        }
    };
    for premise in body {
        let Some(found) = sets.sets_of(premise, poll)? else {
            return Ok(Held::uncharged(Annotation::default(), run));
        };
        let own = found.sets();
        // Charged first: the unions (their slots, at most the longer operand's words each)
        // and `minimal`'s output beside them; past what a budget can count, refused.
        let n = product.sets.len().saturating_mul(own.len());
        let unions_words = own
            .len()
            .saturating_mul(words(&product.sets))
            .saturating_add(product.sets.len().saturating_mul(words(own.iter())));
        let mut next = Charged::with_capacity(run, n, cap, unions_words)?;
        for a in &product.sets {
            for b in own.iter() {
                next.push(a.union(b));
            }
        }
        let truncated = product.truncated || own.truncated;
        let mut chosen = next.minimal(cap, &|_| false)?;
        chosen.annotation.truncated |= truncated;
        chosen.annotation.beyond_own |= truncated;
        // The last product goes (its sets, then its charge) once this one is made.
        product = chosen;
    }
    // A premise without sets yet: the instance gives nothing now, whatever was cut.
    Ok(match product.is_empty() {
        true => Held::uncharged(Annotation::default(), run),
        false => product,
    })
}

/// Adds `from` (derivations of `fact`) to `fact`'s sets, `annotations` following the heap
/// bytes; whether its sets or its marks changed (news for the facts derived from it). The
/// fact's own graphs are read on `poll`.
fn merge(
    sets: &mut HashMap<Triple, Annotation>,
    fact: Triple,
    from: Annotation,
    cap: usize,
    graphs_of: &dyn Graphs,
    poll: &Poller<'_>,
    annotations: &mut usize,
) -> bool {
    #[cfg(test)]
    crate::witness::add(crate::witness::Site::Merge, 1);
    let current = sets.entry(fact).or_default();
    // At its final capacity ([`merge_found`] charges it so).
    let mut all = Vec::with_capacity(current.sets.len() + from.sets.len());
    all.extend_from_slice(&current.sets);
    all.extend(from.sets);
    let mut next = minimal(all, cap, &own_graph_of(fact, graphs_of, poll));
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
    /// Review r2 C1.a, by the allocator: the singleton fast path charges each spilled copy
    /// and union before it is made, under a budget with room or none, and holds nothing
    /// beyond its charge where it leaves early (a premise with no sets) or falls back to the
    /// product (a premise with two). Each row is checked on its own.
    #[test]
    fn a_singleton_union_is_charged_before_it_is_made() {
        use super::{Annotation, GraphSet, Limits, Run, premise_sets_from};
        use hashbrown::HashMap;
        use nrese_exec::{Budget, measure};
        use std::sync::Arc;
        if !measure::alone() {
            return;
        }
        // Graphs past 2^16: a singleton holds 1,025 words on the heap.
        let one = |graph: u32| Annotation {
            sets: vec![GraphSet::of(graph)],
            ..Annotation::default()
        };
        let (p, q, r, unknown) = ([1, 2, 3], [3, 2, 4], [4, 2, 5], [9, 9, 9]);
        let mut sets: HashMap<[u64; 3], Annotation> = HashMap::new();
        sets.insert(p, one(1 << 16));
        sets.insert(q, one((1 << 16) + 1));
        sets.insert(
            r,
            Annotation {
                sets: vec![GraphSet::of(1 << 16), GraphSet::of(70_000)],
                ..Annotation::default()
            },
        );
        let start = Annotation::empty_set();
        let rows: [(&str, Vec<[u64; 3]>, usize, bool); 4] = [
            ("spilled singletons", vec![p, q], usize::MAX, false),
            ("refused", vec![p, q], 0, true),
            ("early exit", vec![p, unknown], usize::MAX, false),
            ("fallback to the product", vec![p, r], usize::MAX, false),
        ];
        let mut failed = Vec::new();
        for (what, body, limit, refused) in rows {
            let budget = Arc::new(Budget::new(limit));
            let (got, excess) = measure::measure_within(&budget, || {
                let run = Run::new(Limits::unlimited(&budget));
                let never = crate::eval::Poller::never();
                premise_sets_from(&body, &start, &sets, 8, &run, &never).map(|held| held.sets.len())
            });
            if got.is_err() != refused || excess != 0 || budget.used() != 0 {
                failed.push(format!(
                    "{what}: {:?}, {excess} bytes beyond the charge, {} still charged",
                    got.map_err(|_| "refused"),
                    budget.used()
                ));
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    /// Fix check r2 C1.b residual, by the allocator: an update's lookup of a premise it doesn't
    /// recompute reads the premise's sets where its caller keeps them (a store's pool of
    /// distinct sets, which the premise's entries index), and where nothing is kept (sets
    /// that are just those it starts with, from its graphs) makes them charged first. The
    /// unaffected premise `x` comes first, the affected `a` (own graph 2^16) second; rows vary
    /// how many sets `x` keeps and how far their graphs reach, and refuse with no room. Each
    /// row is checked on its own: nothing held beyond the charge, nothing charged after, and
    /// either the specified sets (each of `x`'s with `a`'s graph, unmarked) or a refusal.
    #[test]
    fn an_unaffected_premise_is_read_where_it_is_kept() {
        use super::{
            Annotation, GraphSet, InGraphs, Limits, Previous, Recomputing, Run, Sets,
            premise_sets_from,
        };
        use hashbrown::{HashMap, HashSet};
        use nrese_exec::{Budget, measure};
        use std::sync::Arc;
        if !measure::alone() {
            return;
        }
        let (x, a): ([u64; 3], [u64; 3]) = ([1, 2, 3], [3, 2, 4]);
        let a_graph = 1u32 << 16;
        let mut now: HashMap<[u64; 3], Annotation> = HashMap::new();
        now.insert(
            a,
            Annotation {
                sets: vec![GraphSet::of(a_graph)],
                ..Annotation::default()
            },
        );
        let affected: HashSet<[u64; 3]> = [a].into_iter().collect();
        let start = Annotation::empty_set();
        let cap = 64;
        let spread = |n: u32, step: u32| (0..n).map(|j| 128 + j * step).collect::<Vec<u32>>();
        let near = ((1u32 << 16) - 4..1 << 16).collect::<Vec<u32>>();
        // (row, x's sets are only its own graphs', x's graphs, the budget's limit)
        let rows: [(&str, bool, Vec<u32>, usize); 6] = [
            ("kept 1, span 2^16", false, vec![(1 << 16) + 1], usize::MAX),
            ("kept 8, span 2^12", false, spread(8, 512), usize::MAX),
            ("kept 64, span 2^16", false, spread(64, 1022), usize::MAX),
            ("own 4, at 2^16", true, near.clone(), usize::MAX),
            ("kept 64, refused first", false, spread(64, 1022), 0),
            ("own 4, refused first", true, near, 0),
        ];
        let mut failed = Vec::new();
        for (what, own, graphs, limit) in rows {
            // As a store keeps them: a kept fact's distinct sets in a pool and its entries
            // indexing it; an own fact's graphs only (each asserted fact with its graph).
            let pool: Vec<GraphSet> = graphs.iter().map(|&g| GraphSet::of(g)).collect();
            let entries: Vec<([u64; 3], u32)> = (0..pool.len() as u32).map(|i| (x, i)).collect();
            let placed: Vec<([u64; 3], u32)> = graphs.iter().map(|&g| (x, g)).collect();
            // The store's callback (`nrese-store` support.rs, `SupportSets::updated`).
            let previous = |fact: [u64; 3], _: &crate::eval::Poller<'_>| {
                Ok(match (fact == x, own) {
                    (false, _) => Previous::Sets(Sets::NONE),
                    (true, false) => Previous::Sets(Sets::pooled(&entries, &pool, false)),
                    (true, true) => Previous::Own,
                })
            };
            let asserted = InGraphs(&placed);
            let budget = Arc::new(Budget::new(limit));
            let (got, excess) = measure::measure_within(&budget, || {
                let run = Run::new(Limits::unlimited(&budget));
                let lookup = Recomputing {
                    now: &now,
                    affected: &affected,
                    previous: &previous,
                    graphs: &asserted,
                    axioms: &[],
                    cap,
                    run: &run,
                };
                let never = crate::eval::Poller::never();
                premise_sets_from(&[x, a], &start, &lookup, cap, &run, &never)
            });
            let got = got.map(|held| {
                let mut sets: Vec<Vec<u32>> =
                    held.sets.iter().map(|set| set.graphs().collect()).collect();
                sets.sort_unstable();
                (sets, held.truncated)
            });
            let want = match limit {
                0 => None,
                _ => {
                    // Each union's graphs, ascending (as `GraphSet::graphs` gives them).
                    let mut sets: Vec<Vec<u32>> = graphs
                        .iter()
                        .map(|&g| vec![g.min(a_graph), g.max(a_graph)])
                        .collect();
                    sets.sort_unstable();
                    Some((sets, false))
                }
            };
            let answered = match (&got, &want) {
                (Ok(got), Some(want)) => got == want,
                (Err(_), None) => true,
                _ => false,
            };
            if !answered || excess != 0 || budget.used() != 0 {
                failed.push(format!(
                    "{what}: {}, {excess} bytes beyond the charge, {} still charged",
                    match (&got, answered) {
                        (Ok((sets, _)), true) => format!("{} sets as specified", sets.len()),
                        (Ok(got), false) => format!("{got:?}, not as specified"),
                        (Err(_), true) => "refused".to_owned(),
                        (Err(_), false) => "refused, not as specified".to_owned(),
                    },
                    budget.used()
                ));
            } else {
                eprintln!("{what}: ok");
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    /// Merge-time count of #159 (`p2c5-r4-memcount1-2df9ed96`, the master 12:50): a round's
    /// parts gathered into one hold their entries once, as the base's one buffer did. Where a
    /// part has room for all of them, gathering allocates nothing; where none has, at most one
    /// buffer of their total. Each row on its own, by the allocator, with every entry kept
    /// once and sorted.
    #[test]
    fn gathered_parts_hold_their_entries_once() {
        use super::found::Entry;
        use super::{Found, GraphSet, Limits, Run};
        use nrese_exec::{Budget, measure};
        use std::sync::Arc;
        if !measure::alone() {
            return;
        }
        let entry = |i: u64| -> Entry { ([i, 2, 3], GraphSet::of((i % 64) as u32), false) };
        // A row, each part's (entries, room for), and what gathering may allocate at most.
        type Row = (&'static str, Vec<(u64, usize)>, usize);
        let slot = size_of::<Entry>();
        let rows: [Row; 2] = [
            ("a part has room for all", vec![(600, 1024), (100, 100)], 0),
            ("none has room", vec![(500, 500), (500, 500)], 1000 * slot),
        ];
        let mut failed = Vec::new();
        for (what, shape, most) in rows {
            let budget = Arc::new(Budget::unlimited());
            let mut next = 0u64;
            let parts: Vec<Found<'_>> = shape
                .iter()
                .map(|&(n, room)| {
                    let mut part = Found::new(&budget);
                    part.reserve(room).unwrap();
                    for _ in 0..n {
                        part.add(entry(next).0, &entry(next).1, false).unwrap();
                        next += 1;
                    }
                    part
                })
                .collect();
            let total: usize = shape.iter().map(|&(n, _)| n as usize).sum();
            let run = Run::new(Limits::unlimited(&budget));
            let (gathered, held) = measure::measure(|| run.gather(parts));
            let gathered = gathered.expect("an unlimited budget");
            let mut want: Vec<Entry> = (0..total as u64).map(entry).collect();
            want.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let answered = gathered.entries() == want.as_slice();
            if !answered || held > most {
                failed.push(format!(
                    "{what}: {} entries{}, {held} bytes allocated (at most {most})",
                    gathered.len(),
                    if answered { "" } else { ", not as specified" },
                ));
            } else {
                eprintln!("{what}: ok, {held} bytes");
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    /// Merge-time count of #159 (`p2c5-r4-memcount3-caded3cf`: product's commit-2 update 13 %
    /// slower than before r4): a lookup of a fact whose sets are its own graphs' reads them
    /// once where they fit its buffer, and a second time only past it, by the work counter
    /// (its calls to `Graphs::each`). Each row gives `initial_sets` of the fact's graphs.
    #[test]
    fn own_sets_read_their_graphs_once() {
        use super::{Graphs, InGraphs, Limits, Run, initial_sets, own_sets};
        use crate::eval::Poller;
        use nrese_exec::Budget;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counted<'a>(InGraphs<'a>, AtomicUsize);
        impl Graphs for Counted<'_> {
            fn each(
                &self,
                fact: [u64; 3],
                poll: &Poller<'_>,
                graph: &mut dyn FnMut(u32),
            ) -> Result<(), super::ReadStopped> {
                self.1.fetch_add(1, Ordering::Relaxed);
                self.0.each(fact, poll, graph)
            }
            fn holds(
                &self,
                fact: [u64; 3],
                graph: u32,
                poll: &Poller<'_>,
            ) -> Result<bool, super::ReadStopped> {
                self.0.holds(fact, graph, poll)
            }
        }
        let never = Poller::never();
        let fact = [1, 2, 3];
        let budget = Arc::new(Budget::unlimited());
        let run = Run::new(Limits::unlimited(&budget));
        let mut failed = Vec::new();
        // (graphs the fact is asserted in, reads at most)
        for (k, most) in [(1u32, 1usize), (32, 1), (64, 1), (65, 2)] {
            let placed: Vec<([u64; 3], u32)> = (0..k).map(|g| (fact, 200 + g)).collect();
            let graphs = Counted(InGraphs(&placed), AtomicUsize::new(0));
            let cap = 16;
            let got = own_sets(fact, &graphs, &[], cap, &run, &never)
                .expect("an unlimited budget")
                .map(|looked| {
                    let sets = looked.sets();
                    (sets.iter().cloned().collect::<Vec<_>>(), sets.truncated)
                });
            let want = initial_sets((0..k).map(|g| 200 + g).collect(), false, cap);
            let reads = graphs.1.load(Ordering::Relaxed);
            let answered = got.as_ref() == Some(&(want.sets.clone(), want.truncated));
            if !answered || reads > most {
                failed.push(format!(
                    "{k} graphs: {reads} reads (at most {most}){}",
                    if answered {
                        ""
                    } else {
                        ", sets not as specified"
                    }
                ));
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    /// What an own set's charge counts before it is made ([`super::own_sets`]) is what the
    /// set holds: either side of the inline edge (128 graphs) and far past it.
    #[test]
    fn a_singletons_heap_bytes_are_known_before_it_is_made() {
        use super::GraphSet;
        for graph in [0, 63, 64, 127, 128, 129, 191, 192, 1 << 16, (1 << 16) + 1] {
            assert_eq!(
                GraphSet::heap_bytes_of(graph),
                GraphSet::of(graph).heap_bytes(),
                "graph {graph}"
            );
        }
    }

    /// Review r2 C1.b, by the allocator: a round's jobs refer to their rule's charged schema
    /// part and hold no copy of it. Over several live jobs, what building them allocates is
    /// the same whatever the schema parts' set count and span.
    #[test]
    fn round_jobs_hold_no_copy_of_their_schema_parts() {
        use super::{Annotation, Changed, GraphSet, Grounding, Held, round_jobs};
        use crate::batch::Store;
        use crate::ir::{Rule, Term, Triple, parse_rules};
        use crate::vocabulary::LocalVocabulary;
        use nrese_exec::{Budget, measure};
        use std::sync::Arc;
        if !measure::alone() {
            return;
        }
        let mut vocabulary = LocalVocabulary::default();
        let mut rules = parse_rules(
            "r1: (?x rdfs:p ?y), (?y rdfs:p ?z) -> (?x rdfs:q ?z)",
            &mut vocabulary,
        )
        .unwrap();
        rules.extend(parse_rules("r2: (?x rdfs:p ?y) -> (?x rdfs:r ?y)", &mut vocabulary).unwrap());
        let Term::Const(p) = rules[0].body[0].0[1] else {
            panic!("a constant predicate")
        };
        let mut facts: Vec<Triple> = (0..64u64).map(|i| [1_000 + i, p, 1_001 + i]).collect();
        facts.sort_unstable();
        let store = Store::new(facts);
        let source = Changed {
            all: &store,
            changed: &store,
        };
        let rule_refs: Vec<(&Rule, Grounding)> = rules
            .iter()
            .enumerate()
            .map(|(r, rule)| (rule, Grounding::Rule(r)))
            .collect();
        // Rule 1 run whole (one job), rule 2 by its changed premise.
        let whole = [true, false];
        let budget = Arc::new(Budget::unlimited());
        let mut beyond = Vec::new();
        for (count, span) in [(2u32, 64u32), (64, 1 << 12), (512, 1 << 16)] {
            let schemas: Vec<Held<'_>> = rule_refs
                .iter()
                .map(|_| Held {
                    annotation: Annotation {
                        sets: (0..count)
                            .map(|j| GraphSet::of(j * (span / count)))
                            .collect(),
                        ..Annotation::default()
                    },
                    _charge: None,
                })
                .collect();
            let (live, excess) = measure::measure_within(&budget, || {
                let never = crate::eval::Poller::never();
                round_jobs(&source, &rule_refs, &schemas, &whole, &never).len()
            });
            assert!(live >= 2, "several live jobs: {live}");
            beyond.push(excess);
        }
        assert!(
            beyond.windows(2).all(|w| w[0] == w[1]),
            "what the jobs hold grows with their schema parts: {beyond:?}"
        );
    }

    /// Sol-loop r6 S3: the sets a grounding's alternatives gathered are chosen among under a
    /// budget that holds exactly them and `minimal`'s output slots, and nothing more is
    /// asked: their words are carried over, not charged again. After, only what the output
    /// holds stays charged.
    #[test]
    fn chosen_sets_are_charged_once() {
        use super::{Budget, ChargedVec, Gathered, GraphSet, Limits, Run, slots, words};
        use std::sync::Arc;
        // Eight sets past 128 graphs, each with words on the heap, two of them covering
        // others (`minimal` drops them), cap 8.
        let mut sets: Vec<GraphSet> = (0..6).map(|g| GraphSet::of(200 + g)).collect();
        sets.push(GraphSet::of(200).union(&GraphSet::of(250)));
        sets.push(GraphSet::of(201).union(&GraphSet::of(251)));
        let gathered = {
            let probe = Arc::new(Budget::unlimited());
            let mut all: ChargedVec<GraphSet, &Budget> = ChargedVec::new(&probe);
            all.extend_from_slice(&sets).unwrap();
            all.charged() + words(&sets)
        };
        let budget = Arc::new(Budget::new(gathered + slots(sets.len())));
        let mut all = Gathered::new(&budget);
        all.words.charge(words(&sets)).unwrap();
        all.sets.extend_from_slice(&sets).unwrap();
        let run = Run::new(Limits::unlimited(&budget));
        let held = all.choose(8, &run).expect("nothing asked twice");
        assert_eq!(held.sets.len(), 6, "the two covering sets dropped");
        assert_eq!(
            budget.used(),
            slots(held.sets.capacity()) + words(&held.sets),
            "only what the output holds stays charged"
        );
        drop(held);
        assert_eq!(budget.used(), 0);
    }

    /// Sol-loop r7 S4, by the allocator: a choice refused (no room for `minimal`'s output)
    /// frees its sets before it releases their words' charge: nothing held beyond the
    /// charge, and nothing charged after.
    #[test]
    fn a_refused_choice_frees_its_sets_before_their_charge() {
        use super::{Budget, ChargedVec, Gathered, GraphSet, Limits, Run, words};
        use nrese_exec::measure;
        use std::sync::Arc;
        if !measure::alone() {
            return;
        }
        // Eight singleton sets past 128 graphs: words on the heap, all kept.
        let sets: Vec<GraphSet> = (0..8).map(|g| GraphSet::of(200 + g)).collect();
        let gathered = {
            let probe = Arc::new(Budget::unlimited());
            let mut all: ChargedVec<GraphSet, &Budget> = ChargedVec::new(&probe);
            all.extend_from_slice(&sets).unwrap();
            all.charged() + words(&sets)
        };
        // Room for the gathered sets alone: the output's slots are refused.
        let budget = Arc::new(Budget::new(gathered));
        let (refused, excess) = measure::measure_within(&budget, || {
            let mut all = Gathered::new(&budget);
            all.words.charge(words(&sets)).unwrap();
            all.sets.extend_from_slice(&sets).unwrap();
            let run = Run::new(Limits::unlimited(&budget));
            all.choose(8, &run).is_err()
        });
        assert!(refused, "no room for the output");
        assert_eq!(excess, 0, "{excess} bytes held beyond the charge");
        assert_eq!(budget.used(), 0);
    }

    /// Sol-loop r9 S5, by the allocator: a support algebra result moved into the sets a
    /// grounding gathers stays charged until its old list is freed, and the choice after
    /// holds nothing beyond its charge either.
    #[test]
    fn a_gathered_result_stays_charged_until_its_list_is_freed() {
        use super::{Charged, Gathered, GraphSet, Limits, Run, words};
        use nrese_exec::{Budget, measure};
        use std::sync::Arc;
        if !measure::alone() {
            return;
        }
        // Eight singleton sets past 128 graphs: a result of eight, all kept.
        let sets: Vec<GraphSet> = (0..8).map(|g| GraphSet::of(200 + g)).collect();
        let budget = Arc::new(Budget::unlimited());
        let (kept, excess) = measure::measure_within(&budget, || {
            let run = Run::new(Limits::unlimited(&budget));
            let mut made = Charged::with_capacity(&run, sets.len(), 8, words(&sets)).unwrap();
            sets.iter().for_each(|set| made.push(set.clone()));
            let held = made.minimal(8, &|_| false).unwrap();
            let mut all = Gathered::new(&budget);
            all.take(held).unwrap();
            all.choose(8, &run).unwrap().sets.len()
        });
        assert_eq!(kept, 8);
        assert_eq!(excess, 0, "{excess} bytes held beyond the charge");
        assert_eq!(budget.used(), 0);
    }

    /// Sol-loop r5 S1, by the allocator (`nrese_exec::measure`, which `Budget::peak` can't
    /// replace: it sees allocations nothing charged). A premise product's unions and
    /// `minimal`'s output are charged before they are made, the product's charge lives as long
    /// as it does, and the result's goes with it: nothing held beyond the charge, at a cap as
    /// large as the product (every union kept). Rows: unions inline, and unions whose words
    /// spill past 128 graphs.
    #[test]
    fn a_premise_product_holds_nothing_beyond_its_charge() {
        use super::{Annotation, GraphSet, Limits, Run, premise_sets_from};
        use hashbrown::HashMap;
        use nrese_exec::{Budget, measure};
        use std::sync::Arc;
        if !measure::alone() {
            return;
        }
        let n = 64u32;
        for (what, b) in [("inline", 64u32), ("spilled", 200)] {
            let (p, q) = ([1, 2, 3], [3, 2, 4]);
            let mut sets: HashMap<[u64; 3], Annotation> = HashMap::new();
            let family = |graphs: std::ops::Range<u32>| Annotation {
                sets: graphs.map(GraphSet::of).collect(),
                ..Annotation::default()
            };
            sets.insert(p, family(0..n));
            sets.insert(q, family(b..b + n));
            let start = Annotation::empty_set();
            let cap = (n * n) as usize;
            let budget = Arc::new(Budget::unlimited());
            let (kept, excess) = measure::measure_within(&budget, || {
                let run = Run::new(Limits::unlimited(&budget));
                let never = crate::eval::Poller::never();
                let held = premise_sets_from(&[p, q], &start, &sets, cap, &run, &never).unwrap();
                held.sets.len()
            });
            assert_eq!(kept, cap, "{what}: every union kept");
            assert_eq!(excess, 0, "{what}: {excess} bytes held beyond the charge");
            assert_eq!(budget.used(), 0, "{what}: still charged");
        }
    }

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

    /// DC-07: a stop by the process limit whose memory couldn't be read is reported as
    /// that, not as the limit; a stop past the limit stays the limit's.
    #[test]
    fn an_unread_process_limit_stops_as_memory_unreadable() {
        use super::Stopped;
        use nrese_exec::memory::MemoryStopCause;
        assert_eq!(
            Stopped::of_process(MemoryStopCause::Unread),
            Stopped::MemoryUnreadable
        );
        assert_eq!(
            Stopped::of_process(MemoryStopCause::OverLimit),
            Stopped::ProcessMemory
        );
    }

    /// REAS-2 review r1 F1 (the reviewer's witness): a fact's graphs as the support sets read
    /// them ([`super::InGraphs`], the store's `SupportSets::compute` and the own lookups) are
    /// handed on as they are counted. One fact asserted in 3 × `POLL_UNITS` graphs, the stop
    /// set from the first graph handed on: at most `POLL_UNITS` plus one run more are handed
    /// on after it (REAS-2's bound; the run counted at once is at most `POLL_UNITS`). Counted
    /// all at once, every one of them was.
    #[test]
    fn a_fact_s_graphs_are_handed_on_within_a_run_of_the_stop() {
        use super::{Graphs, InGraphs};
        use crate::eval::Poller;
        use std::sync::atomic::{AtomicBool, Ordering};
        let interval = nrese_exec::POLL_UNITS as usize;
        let fact = [1, 2, 3];
        let placed: Vec<([u64; 3], u32)> = (0..3 * interval as u32).map(|g| (fact, g)).collect();
        let fired = AtomicBool::new(false);
        let stop = || fired.load(Ordering::Relaxed);
        let poll = Poller::new(&stop).expect("not stopped yet");
        let (mut handed, mut after) = (0usize, 0usize);
        let read = InGraphs(&placed).each(fact, &poll, &mut |_| {
            handed += 1;
            match fired.load(Ordering::Relaxed) {
                true => after += 1,
                false => fired.store(true, Ordering::Relaxed),
            }
        });
        assert!(read.is_err(), "a read its poller stopped says so");
        assert!(handed > 0, "the first graph is handed on before the stop");
        let run = interval;
        assert!(
            after <= interval + run,
            "{after} graphs handed on after the stop (at most {interval} plus one run of {run})"
        );
    }
}
