//! Inferred statements under graph access (`inferred = "supported"` in the access
//! policy; docs/design/reasoner-provenance.md). A user who may read the graphs `R` sees
//! an inferred statement when one of its support graph sets lies in `R`: one of its
//! derivations uses statements of readable graphs alone, so the user could have derived
//! it.
//!
//! - **The sets** ([`SupportSets`]) are computed for a committed revision on the first
//!   read that needs them, from the stack and the ground program
//!   ([`nrese_reasoner::v2::graph_sets`]); readers of that revision wait for one
//!   computation. Once there are sets, the mutation pipeline reports what each commit
//!   touched, and the next revision's sets are the last ones updated for the facts that
//!   can follow from those ([`SupportSets::updated`]): a base shared between revisions,
//!   with the recomputed facts on top until they are folded in. A write it didn't report
//!   (a bulk load, a rematerialisation), a change of the schema, or a change reaching too
//!   many facts computes them afresh.
//! - **A reader's view** is the snapshot with its invisible inferred statements removed
//!   ([`nrese_engine::Snapshot::with_inferred_subset`]): every read path (counts, sorted
//!   scans, equality, the query cache keyed by access) sees the subset alone. Views are
//!   kept per revision and access set; the next revision's view is the last one's list
//!   corrected for the facts the update recomputed.
//! - **Not computable** (no reasoner v2 rules registered, or the stack over
//!   representatives of `reasoner.equality = "compact"`): the reader sees no inferred
//!   statement, the safe side.
//!
//! The cap on sets per statement (`reasoner.support_sets`) can only hide a statement from
//! a reader who could have seen it, never show one. Graphs are numbered in IRI order, so
//! which sets the cap keeps doesn't depend on the order terms were stored in.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use hashbrown::HashSet;
use nrese_engine::quad::Permutation;
use nrese_engine::{
    EncodedQuad, GraphSelector, InferredSubset, QuadPattern, ReadModel, Snapshot, TermId,
};
use nrese_reasoner::RuleProgram;
use nrese_reasoner::v2::eval::{Seg, Source};
use nrese_reasoner::v2::graph_sets::{
    Change, GraphSet, initial_sets, support_sets, update_support_sets,
};
use nrese_reasoner::v2::ir::Triple;
use nrese_sparql::GraphAccess;

use crate::reasoning::Program;

/// Restricted views kept per revision.
const VIEWS: usize = 32;

/// Touched facts kept for updates; past this many, the next sets are computed afresh.
const MAX_TOUCHED: usize = 1 << 20;

/// Recomputed facts kept on top of the base before they are folded into it.
const MAX_OVERLAY: usize = 1 << 16;

/// The support graph sets of one revision's facts.
#[derive(Debug, Clone)]
pub struct SupportSets {
    /// The graph of each set member: the default graph, then the named graphs in IRI
    /// order (later ones appended as they appear).
    graphs: Vec<TermId>,
    /// The distinct sets, and the index of each.
    sets: Arc<Vec<GraphSet>>,
    interned: Arc<HashMap<GraphSet, u32>>,
    /// The facts whose sets aren't just those they start with (an inferred statement's,
    /// an asserted one's also derived from other graphs), each with one of its sets (an
    /// index into `sets`), sorted: a fact's sets are adjacent.
    base: Arc<Vec<(Triple, u32)>>,
    /// Facts recomputed since `base` was built, with their sets (none: they start with
    /// their own); these replace their entries in `base`.
    overlay: Arc<HashMap<Triple, Vec<u32>>>,
    /// The ruleset's axioms, sorted: their set is the empty one.
    axioms: Arc<Vec<Triple>>,
    /// The schema facts the ground program was grounded on: a change of them changes
    /// the program, and the sets are computed afresh.
    schema: Arc<HashSet<Triple>>,
    /// Where these sets were updated from: that revision, and the facts (sorted) whose
    /// sets or presence may differ from it.
    since: Option<(u64, Arc<Vec<Triple>>)>,
}

impl SupportSets {
    /// The sets of `snapshot`'s facts under `program`, at most `cap` per fact.
    pub fn compute(program: &Program, snapshot: &Snapshot, cap: usize) -> Self {
        let graphs = graphs_in_order(snapshot, vec![TermId::DEFAULT_GRAPH]);
        let index = index_of(&graphs);
        // Each asserted statement with a graph that holds it, sorted.
        let asserted: Vec<(Triple, u32)> = sorted_quads(snapshot, ReadModel::Asserted)
            .into_iter()
            .map(|quad| (triple(quad), index[&quad.graph]))
            .collect();
        let inferred: Vec<Triple> = sorted_quads(snapshot, ReadModel::Inferred)
            .into_iter()
            .map(triple)
            .collect();
        let axioms = program.axioms().to_vec();
        let mut facts: Vec<Triple> = asserted.iter().map(|&(fact, _)| fact).collect();
        facts.extend_from_slice(&inferred);
        facts.extend_from_slice(&axioms);
        facts.sort_unstable();
        facts.dedup();
        let graphs_of = |fact: Triple| -> Vec<u32> {
            let start = asserted.partition_point(|&(f, _)| f < fact);
            asserted[start..]
                .iter()
                .take_while(|&&(f, _)| f == fact)
                .map(|&(_, graph)| graph)
                .collect()
        };
        let ground = program.ground_program(snapshot);
        let by_fact = support_sets(&facts, &ground, &axioms, &graphs_of, cap);
        let schema = ground.schema_premises();
        drop(ground);
        let (mut sets, mut interned) = (Vec::new(), HashMap::new());
        let mut base = Vec::with_capacity(inferred.len());
        for (fact, found) in by_fact {
            let axiom = axioms.binary_search(&fact).is_ok();
            if found != initial(axiom, graphs_of(fact), cap) {
                for id in intern(&mut sets, &mut interned, found) {
                    base.push((fact, id));
                }
            }
        }
        base.sort_unstable();
        Self {
            graphs,
            sets: Arc::new(sets),
            interned: Arc::new(interned),
            base: Arc::new(base),
            overlay: Arc::default(),
            axioms: Arc::new(axioms),
            schema: Arc::new(schema),
            since: None,
        }
    }

    /// These sets, of revision `from`, brought to `snapshot`, whose changes since
    /// touched the facts `touched` (sorted: their graphs, or whether they are in the
    /// closure). `None` where the update can't serve: the schema changed, or the change
    /// reaches too many facts.
    pub fn updated(
        &self,
        program: &Program,
        snapshot: &Snapshot,
        from: u64,
        touched: &[Triple],
        cap: usize,
    ) -> Option<Self> {
        let ground = program.ground_program(snapshot);
        let schema = ground.schema_premises();
        if schema != *self.schema || program.axioms() != self.axioms.as_slice() {
            return None;
        }
        let graphs = graphs_in_order(snapshot, self.graphs.clone());
        let index = index_of(&graphs);
        let graphs_of = |fact: Triple| -> Vec<u32> {
            let mut found: Vec<u32> = snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &pattern(fact, GraphSelector::Any))
                .filter_map(|quad| index.get(&quad.graph).copied())
                .collect();
            found.sort_unstable();
            found.dedup();
            found
        };
        let in_closure = |fact: Triple| {
            snapshot.exists_in(ReadModel::Materialised, &pattern(fact, GraphSelector::Any))
                || self.axioms.binary_search(&fact).is_ok()
        };
        let removed: Vec<Triple> = touched
            .iter()
            .copied()
            .filter(|&fact| !in_closure(fact))
            .collect();
        let source = After {
            snapshot,
            removed: &removed,
        };
        let previous = |fact: Triple| -> Vec<GraphSet> {
            match self.ids_of(fact) {
                Some(ids) => ids
                    .iter()
                    .map(|&id| self.sets[id as usize].clone())
                    .collect(),
                None => self.initial(fact, graphs_of(fact), cap),
            }
        };
        let change = Change {
            source: &source,
            in_closure: &in_closure,
            touched,
            previous: &previous,
            limit: self.base.len() / 4 + 10_000,
        };
        let recomputed = update_support_sets(&change, &ground, &self.axioms, &graphs_of, cap)?;
        drop(ground);
        let mut sets = Arc::clone(&self.sets);
        let mut interned = Arc::clone(&self.interned);
        let mut overlay = (*self.overlay).clone();
        let mut changed: Vec<Triple> = touched.to_vec();
        for (fact, found) in recomputed {
            let ids = match found == self.initial(fact, graphs_of(fact), cap) {
                true => Vec::new(),
                false => intern(
                    Arc::make_mut(&mut sets),
                    Arc::make_mut(&mut interned),
                    found,
                ),
            };
            overlay.insert(fact, ids);
            changed.push(fact);
        }
        for &fact in &removed {
            overlay.insert(fact, Vec::new());
        }
        changed.sort_unstable();
        changed.dedup();
        let mut updated = Self {
            graphs,
            sets,
            interned,
            base: Arc::clone(&self.base),
            overlay: Arc::new(overlay),
            axioms: Arc::clone(&self.axioms),
            schema: Arc::new(schema),
            since: Some((from, Arc::new(changed))),
        };
        if updated.overlay.len() > MAX_OVERLAY.max(updated.base.len() / 16) {
            updated.fold();
        }
        Some(updated)
    }

    /// The overlay folded into the base.
    fn fold(&mut self) {
        let overlay = std::mem::take(&mut self.overlay);
        let mut on_top: Vec<(Triple, u32)> = overlay
            .iter()
            .flat_map(|(&fact, ids)| ids.iter().map(move |&id| (fact, id)))
            .collect();
        on_top.sort_unstable();
        let kept = self
            .base
            .iter()
            .copied()
            .filter(|(fact, _)| !overlay.contains_key(fact));
        let mut merged = Vec::with_capacity(self.base.len() + on_top.len());
        let mut on_top = on_top.into_iter().peekable();
        for entry in kept {
            while let Some(next) = on_top.next_if(|&next| next < entry) {
                merged.push(next);
            }
            merged.push(entry);
        }
        merged.extend(on_top);
        self.base = Arc::new(merged);
    }

    /// The sets `fact` starts with, from its graphs.
    fn initial(&self, fact: Triple, graphs: Vec<u32>, cap: usize) -> Vec<GraphSet> {
        initial(self.axioms.binary_search(&fact).is_ok(), graphs, cap)
    }

    /// The entries of `fact` in the base.
    fn in_base(&self, fact: Triple) -> &[(Triple, u32)] {
        let start = self.base.partition_point(|&(f, _)| f < fact);
        let end = self.base.partition_point(|&(f, _)| f <= fact);
        &self.base[start..end]
    }

    /// The set ids recorded for `fact`; `None` if its sets are those it starts with.
    fn ids_of(&self, fact: Triple) -> Option<Vec<u32>> {
        if let Some(ids) = self.overlay.get(&fact) {
            return (!ids.is_empty()).then(|| ids.clone());
        }
        let entries = self.in_base(fact);
        (!entries.is_empty()).then(|| entries.iter().map(|&(_, id)| id).collect())
    }

    /// Whether a reader for whom `visible_sets` tells which sets lie in its graphs sees
    /// the inferred statement `fact`. An axiom without entries holds for everyone;
    /// another statement without them has no derivation the sets know of.
    fn visible(&self, fact: Triple, visible_sets: &[bool]) -> bool {
        let any = |ids: &mut dyn Iterator<Item = u32>| -> Option<bool> {
            let mut some = false;
            for id in ids {
                if visible_sets[id as usize] {
                    return Some(true);
                }
                some = true;
            }
            some.then_some(false)
        };
        let found = match self.overlay.get(&fact) {
            Some(ids) => any(&mut ids.iter().copied()),
            None => any(&mut self.in_base(fact).iter().map(|&(_, id)| id)),
        };
        found.unwrap_or_else(|| self.axioms.binary_search(&fact).is_ok())
    }

    /// Which sets lie in the graphs `access` reads.
    fn visible_sets(&self, snapshot: &Snapshot, access: &GraphAccess) -> Vec<bool> {
        let readable: Vec<bool> = self
            .graphs
            .iter()
            .map(|&graph| access.allows_id(snapshot, graph))
            .collect();
        self.sets
            .iter()
            .map(|set| set.graphs().all(|graph| readable[graph as usize]))
            .collect()
    }

    /// The inferred statements of `snapshot` (the revision the sets are of) that a reader
    /// with `access` sees, as the smaller side of the split, sorted.
    pub fn subset(&self, snapshot: &Snapshot, access: &GraphAccess) -> InferredSubset {
        let visible_sets = self.visible_sets(snapshot, access);
        let (mut shown, mut hidden) = (Vec::new(), Vec::new());
        for quad in sorted_quads(snapshot, ReadModel::Inferred) {
            match self.visible(triple(quad), &visible_sets) {
                true => shown.push(quad),
                false => hidden.push(quad),
            }
        }
        match shown.len() <= hidden.len() {
            true => InferredSubset::Only(shown),
            false => InferredSubset::Without(hidden),
        }
    }

    /// [`Self::subset`] from the subset `earlier` of the revision these sets were updated
    /// from, corrected for the facts that changed since: O(k + c log n) for a list of k
    /// statements and c changed facts. `None` if these sets weren't updated from
    /// `revision`.
    pub fn subset_from(
        &self,
        snapshot: &Snapshot,
        access: &GraphAccess,
        revision: u64,
        earlier: &InferredSubset,
    ) -> Option<InferredSubset> {
        let (from, changed) = self.since.as_ref()?;
        if *from != revision {
            return None;
        }
        let visible_sets = self.visible_sets(snapshot, access);
        let (list, keep_visible) = match earlier {
            InferredSubset::Only(list) => (list, true),
            InferredSubset::Without(list) => (list, false),
        };
        let mut added: Vec<EncodedQuad> = changed
            .iter()
            .map(|&[s, p, o]| {
                let id = TermId::from_raw;
                EncodedQuad::new(id(s), id(p), id(o), TermId::DEFAULT_GRAPH)
            })
            .filter(|quad| {
                snapshot.contains_in(ReadModel::Inferred, quad)
                    && self.visible(triple(*quad), &visible_sets) == keep_visible
            })
            .collect();
        added.sort_unstable();
        let mut next = Vec::with_capacity(list.len() + added.len());
        let mut added = added.into_iter().peekable();
        for &quad in list {
            if changed.binary_search(&triple(quad)).is_ok() {
                continue;
            }
            while let Some(first) = added.next_if(|&first| first < quad) {
                next.push(first);
            }
            next.push(quad);
        }
        next.extend(added);
        Some(match keep_visible {
            true => InferredSubset::Only(next),
            false => InferredSubset::Without(next),
        })
    }

    /// How many facts have entries, how many sets they have in all, and how many have as
    /// many as `cap` (the cap may have left out others).
    pub fn len(&self, cap: usize) -> (usize, usize, usize) {
        let mut counts: HashMap<Triple, usize> = HashMap::new();
        for &(fact, _) in self.base.iter() {
            if !self.overlay.contains_key(&fact) {
                *counts.entry(fact).or_default() += 1;
            }
        }
        for (&fact, own) in self.overlay.iter() {
            if !own.is_empty() {
                counts.insert(fact, own.len());
            }
        }
        let total = counts.values().sum();
        let at_cap = counts.values().filter(|&&n| n >= cap).count();
        (counts.len(), total, at_cap)
    }
}

/// The sets a fact starts with.
fn initial(axiom: bool, graphs: Vec<u32>, cap: usize) -> Vec<GraphSet> {
    initial_sets(if axiom { Vec::new() } else { graphs }, axiom, cap)
}

/// The ids of `found`, interning those `sets` doesn't hold yet.
fn intern(
    sets: &mut Vec<GraphSet>,
    interned: &mut HashMap<GraphSet, u32>,
    found: Vec<GraphSet>,
) -> Vec<u32> {
    found
        .into_iter()
        .map(|set| {
            *interned.entry(set).or_insert_with_key(|set| {
                sets.push(set.clone());
                (sets.len() - 1) as u32
            })
        })
        .collect()
}

/// `known` followed by the named graphs of `snapshot` it lacks, in IRI order.
fn graphs_in_order(snapshot: &Snapshot, mut known: Vec<TermId>) -> Vec<TermId> {
    let mut new: Vec<(String, TermId)> = snapshot
        .named_graphs()
        .filter(|graph| !known.contains(graph))
        .map(|graph| {
            let name = snapshot
                .decode(graph)
                .map(|term| term.to_string())
                .unwrap_or_default();
            (name, graph)
        })
        .collect();
    new.sort_unstable();
    known.extend(new.into_iter().map(|(_, graph)| graph));
    known
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

fn index_of(graphs: &[TermId]) -> HashMap<TermId, u32> {
    graphs
        .iter()
        .enumerate()
        .map(|(i, &graph)| (graph, i as u32))
        .collect()
}

/// The quads of `model`, sorted by subject, predicate, object, graph.
fn sorted_quads(snapshot: &Snapshot, model: ReadModel) -> Vec<EncodedQuad> {
    match snapshot.scan_sorted_in(model, &QuadPattern::all(), Permutation::Spog) {
        Some(quads) => quads.collect(),
        None => {
            let mut quads: Vec<EncodedQuad> = snapshot
                .quads_for_pattern_in(model, &QuadPattern::all())
                .collect();
            quads.sort_unstable();
            quads
        }
    }
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

/// One revision's sets, computed once.
type Cell = Arc<OnceLock<Option<Arc<SupportSets>>>>;

/// A reader's view of one revision, computed once.
type View = (Arc<GraphAccess>, Arc<OnceLock<Snapshot>>);

/// The store's support graph sets and restricted views, for the latest revision read.
#[derive(Debug, Default)]
pub(crate) struct Supports {
    /// The rules the inferred stack is maintained with
    /// ([`crate::StoreService::use_reasoning_rules`]).
    rules: Mutex<Option<RuleProgram>>,
    state: Mutex<State>,
    counters: Counters,
}

#[derive(Debug, Default)]
struct Counters {
    computed: std::sync::atomic::AtomicU64,
    updated: std::sync::atomic::AtomicU64,
    views: std::sync::atomic::AtomicU64,
    patched: std::sync::atomic::AtomicU64,
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
    /// Microseconds spent computing, updating, and building views.
    pub computing_us: u64,
    pub updating_us: u64,
    pub viewing_us: u64,
}

#[derive(Debug, Default)]
struct State {
    /// The latest revision read, its sets and its readers' views.
    latest: Option<(Snapshot, Cell, Vec<View>)>,
    /// The last sets computed, and the revision they are of.
    computed: Option<(u64, Arc<SupportSets>)>,
    /// The commits reported since: the revisions before and after, the facts touched.
    commits: Vec<(u64, u64, Arc<[Triple]>)>,
    /// Each reader's last subset, and the revision it is of.
    subsets: Vec<(Arc<GraphAccess>, u64, Arc<InferredSubset>)>,
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
    pub(crate) fn use_rules(&self, rules: Option<RuleProgram>) {
        *self.rules.lock().unwrap_or_else(|p| p.into_inner()) = rules;
        // Sets and views of other rules are stale.
        *self.state() = State::default();
    }

    pub(crate) fn statistics(&self) -> SupportStatistics {
        use std::sync::atomic::Ordering::Relaxed;
        let c = &self.counters;
        SupportStatistics {
            computed: c.computed.load(Relaxed),
            updated: c.updated.load(Relaxed),
            views: c.views.load(Relaxed),
            patched: c.patched.load(Relaxed),
            computing_us: c.computing.load(Relaxed),
            updating_us: c.updating.load(Relaxed),
            viewing_us: c.viewing.load(Relaxed),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether commits should report what they touched: there are sets to update.
    pub(crate) fn wants_commits(&self) -> bool {
        self.state().computed.is_some()
    }

    /// A commit from revision `from` to `to` touched `touched` (the statements it
    /// inserted or deleted, asserted and inferred).
    pub(crate) fn note_commit(&self, from: u64, to: u64, touched: Vec<Triple>) {
        let mut state = self.state();
        let Some((at, _)) = state.computed else {
            return;
        };
        let kept: usize = state.commits.iter().map(|(.., facts)| facts.len()).sum();
        if to <= at || kept + touched.len() > MAX_TOUCHED {
            // Too much to update from: the next sets are computed afresh.
            state.computed = None;
            state.commits.clear();
            return;
        }
        state.commits.push((from, to, touched.into()));
    }

    /// `snapshot` as a reader with `access` sees it: with only the inferred statements
    /// the sets let it see.
    pub(crate) fn view(
        &self,
        snapshot: &Snapshot,
        access: &Arc<GraphAccess>,
        compilation: Compilation,
    ) -> Snapshot {
        let (cell, view) = {
            let mut state = self.state();
            if !state
                .latest
                .as_ref()
                .is_some_and(|(seen, ..)| seen.same_version(snapshot))
            {
                state.latest = Some((snapshot.clone(), Cell::default(), Vec::new()));
            }
            let (_, cell, views) = state.latest.as_mut().expect("set above");
            let view = match views.iter().find(|(seen, _)| **seen == **access) {
                Some((_, view)) => Arc::clone(view),
                None => {
                    if views.len() >= VIEWS {
                        views.remove(0);
                    }
                    let view = Arc::default();
                    views.push((Arc::clone(access), Arc::clone(&view)));
                    view
                }
            };
            (Arc::clone(cell), view)
        };
        view.get_or_init(|| {
            let sets = cell.get_or_init(|| self.compute(snapshot, compilation));
            match sets {
                Some(sets) => self.build_view(snapshot, access, sets),
                None => snapshot.with_inferred_subset(InferredSubset::Only(Vec::new())),
            }
        })
        .clone()
    }

    /// The view of `snapshot` for `access` under `sets`: from the reader's last subset
    /// where the sets were updated from its revision, else from the whole stack.
    fn build_view(
        &self,
        snapshot: &Snapshot,
        access: &Arc<GraphAccess>,
        sets: &SupportSets,
    ) -> Snapshot {
        let started = std::time::Instant::now();
        let earlier = self
            .state()
            .subsets
            .iter()
            .find(|(seen, ..)| **seen == **access)
            .map(|(_, revision, subset)| (*revision, Arc::clone(subset)));
        let patched = earlier
            .and_then(|(revision, subset)| sets.subset_from(snapshot, access, revision, &subset));
        if patched.is_some() {
            Counters::add(&self.counters.patched, 1);
        }
        let subset = Arc::new(patched.unwrap_or_else(|| sets.subset(snapshot, access)));
        {
            let mut state = self.state();
            state.subsets.retain(|(seen, ..)| **seen != **access);
            if state.subsets.len() >= VIEWS {
                state.subsets.remove(0);
            }
            state
                .subsets
                .push((Arc::clone(access), snapshot.revision(), Arc::clone(&subset)));
        }
        let view = snapshot.with_inferred_subset((*subset).clone());
        Counters::add(&self.counters.views, 1);
        Counters::add(&self.counters.viewing, started.elapsed().as_micros() as u64);
        view
    }

    /// The sets of `snapshot`: the last ones updated by the commits since, where they
    /// lead there, else computed afresh.
    fn compute(&self, snapshot: &Snapshot, compilation: Compilation) -> Option<Arc<SupportSets>> {
        let rules = self
            .rules
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()?;
        if compilation.compact {
            return None;
        }
        // The program's constants by the snapshot's ids: a constant the store doesn't hold
        // gets an id no statement uses (its rules can't fire).
        let unknown = std::cell::Cell::new(u64::MAX >> 1);
        let program = Program::new(&rules, &|term| {
            snapshot.lookup(term).unwrap_or_else(|| {
                unknown.set(unknown.get() - 1);
                TermId::from_raw(unknown.get())
            })
        })
        .hiding_unnamed_classes(compilation.hide_unnamed_classes)
        .by_representatives(compilation.by_representatives);
        let cap = compilation.cap.max(1);
        let revision = snapshot.revision();
        let started = std::time::Instant::now();
        let updated = self
            .base_for(revision)
            .and_then(|(from, earlier, touched)| {
                earlier.updated(&program, snapshot, from, &touched, cap)
            });
        let c = &self.counters;
        let (sets, counter, time) = match updated {
            Some(sets) => (sets, &c.updated, &c.updating),
            None => (
                SupportSets::compute(&program, snapshot, cap),
                &c.computed,
                &c.computing,
            ),
        };
        let elapsed = started.elapsed();
        Counters::add(counter, 1);
        Counters::add(time, elapsed.as_micros() as u64);
        if sets.since.is_none() {
            let (facts, total, at_cap) = sets.len(cap);
            tracing::info!(
                revision,
                facts,
                sets = total,
                at_cap,
                elapsed_ms = elapsed.as_millis() as u64,
                "support graph sets computed"
            );
            if at_cap > 0 {
                tracing::warn!(
                    at_cap,
                    cap,
                    "statements have as many support graph sets as reasoner.support_sets \
                     keeps; readers may miss some of them"
                );
            }
        }
        let sets = Arc::new(sets);
        let mut state = self.state();
        if state.computed.as_ref().is_none_or(|(at, _)| *at < revision) {
            state.computed = Some((revision, Arc::clone(&sets)));
            state.commits.retain(|&(_, to, _)| to > revision);
        }
        Some(sets)
    }

    /// The last sets computed, their revision, and the facts the reported commits
    /// touched from there to `revision`, if the commits lead there.
    fn base_for(&self, revision: u64) -> Option<(u64, Arc<SupportSets>, Vec<Triple>)> {
        let state = self.state();
        let (from, sets) = state.computed.clone()?;
        let mut at = from;
        let mut touched: Vec<Triple> = Vec::new();
        while at < revision {
            let (_, to, facts) = state.commits.iter().find(|(start, ..)| *start == at)?;
            touched.extend(facts.iter().copied());
            at = *to;
        }
        if at != revision {
            return None;
        }
        touched.sort_unstable();
        touched.dedup();
        Some((from, sets, touched))
    }
}
