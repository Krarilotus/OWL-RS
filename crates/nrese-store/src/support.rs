//! Inferred statements under graph access (`inferred = "supported"` in the access
//! policy; docs/design/reasoner-provenance.md). A user who may read the graphs `R` sees
//! an inferred statement when one of its support graph sets lies in `R`: one of its
//! derivations uses statements of readable graphs alone, so the user could have derived
//! it.
//!
//! - **The sets** ([`SupportSets`]) are computed per committed revision, on the first
//!   read that needs them, from the stack and the ground program
//!   ([`nrese_reasoner::v2::graph_sets`]); readers of that revision wait for one
//!   computation.
//! - **A reader's view** is the snapshot with its invisible inferred statements removed
//!   ([`nrese_engine::Snapshot::with_inferred_subset`]): every read path (counts, sorted
//!   scans, equality, the query cache keyed by access) sees the subset alone. Views are
//!   kept per revision and access set.
//! - **Not computable** (no reasoner v2 rules registered, or the stack over
//!   representatives of `reasoner.equality = "compact"`): the reader sees no inferred
//!   statement, the safe side.
//!
//! The cap on sets per statement (`reasoner.support_sets`) can only hide a statement from
//! a reader who could have seen it, never show one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use nrese_engine::quad::Permutation;
use nrese_engine::{EncodedQuad, InferredSubset, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_reasoner::RuleProgram;
use nrese_reasoner::v2::graph_sets::{GraphSet, support_sets};
use nrese_reasoner::v2::ir::Triple;
use nrese_sparql::GraphAccess;

use crate::reasoning::Program;

/// Restricted views kept per revision.
const VIEWS: usize = 32;

/// The support graph sets of one revision's inferred statements.
#[derive(Debug)]
pub struct SupportSets {
    /// The graph of each set member: the default graph, then the named graphs.
    graphs: Vec<TermId>,
    /// The distinct sets.
    sets: Vec<GraphSet>,
    /// Each inferred statement with one of its sets (an index into `sets`), sorted: a
    /// statement's sets are adjacent. A statement without sets is in none.
    entries: Vec<(Triple, u32)>,
}

impl SupportSets {
    /// The sets of `snapshot`'s inferred statements under `program`, at most `cap` per
    /// statement.
    pub fn compute(program: &Program, snapshot: &Snapshot, cap: usize) -> Self {
        let graphs: Vec<TermId> = std::iter::once(TermId::DEFAULT_GRAPH)
            .chain(snapshot.named_graphs())
            .collect();
        let index: HashMap<TermId, u32> = graphs
            .iter()
            .enumerate()
            .map(|(i, &graph)| (graph, i as u32))
            .collect();
        let scan = |model: ReadModel| -> Vec<EncodedQuad> {
            match snapshot.scan_sorted_in(model, &QuadPattern::all(), Permutation::Spog) {
                Some(quads) => quads.collect(),
                None => {
                    let mut quads: Vec<EncodedQuad> = snapshot
                        .quads_for_pattern_in(model, &QuadPattern::all())
                        .collect();
                    quads.sort_unstable_by_key(|q| (q.subject, q.predicate, q.object, q.graph));
                    quads
                }
            }
        };
        // Each asserted statement with a graph that holds it, sorted.
        let asserted: Vec<(Triple, u32)> = scan(ReadModel::Asserted)
            .into_iter()
            .map(|quad| (triple(quad), index[&quad.graph]))
            .collect();
        let inferred: Vec<Triple> = scan(ReadModel::Inferred).into_iter().map(triple).collect();
        let axioms = program.axioms();
        let mut facts: Vec<Triple> = asserted.iter().map(|&(fact, _)| fact).collect();
        facts.extend_from_slice(&inferred);
        facts.extend_from_slice(axioms);
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
        let mut by_fact = support_sets(&facts, &ground, axioms, &graphs_of, cap);
        drop(ground);
        let mut interned: HashMap<GraphSet, u32> = HashMap::new();
        let mut sets: Vec<GraphSet> = Vec::new();
        let mut entries: Vec<(Triple, u32)> = Vec::with_capacity(inferred.len());
        for fact in inferred {
            for set in by_fact.remove(&fact).unwrap_or_default() {
                let id = *interned.entry(set).or_insert_with_key(|set| {
                    sets.push(set.clone());
                    (sets.len() - 1) as u32
                });
                entries.push((fact, id));
            }
        }
        Self {
            graphs,
            sets,
            entries,
        }
    }

    /// The inferred statements of `snapshot` (the revision the sets are of) that a reader
    /// with `access` sees, as the smaller side of the split.
    pub fn subset(&self, snapshot: &Snapshot, access: &GraphAccess) -> InferredSubset {
        let readable: Vec<bool> = self
            .graphs
            .iter()
            .map(|&graph| access.allows_id(snapshot, graph))
            .collect();
        let visible_sets: Vec<bool> = self
            .sets
            .iter()
            .map(|set| set.graphs().all(|graph| readable[graph as usize]))
            .collect();
        let (mut shown, mut hidden) = (Vec::new(), Vec::new());
        let mut last: Option<(Triple, bool)> = None;
        for quad in snapshot.quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all()) {
            let fact = triple(quad);
            let visible = match last {
                Some((seen, visible)) if seen == fact => visible,
                _ => {
                    // The entries are sorted; the scan's order may differ, so seek.
                    let start = self.entries.partition_point(|&(f, _)| f < fact);
                    let visible = self.entries[start..]
                        .iter()
                        .take_while(|&&(f, _)| f == fact)
                        .any(|&(_, set)| visible_sets[set as usize]);
                    last = Some((fact, visible));
                    visible
                }
            };
            match visible {
                true => shown.push(quad),
                false => hidden.push(quad),
            }
        }
        match shown.len() <= hidden.len() {
            true => InferredSubset::Only(shown),
            false => InferredSubset::Without(hidden),
        }
    }

    /// How many inferred statements have sets, and how many sets they have in all.
    pub fn len(&self) -> (usize, usize) {
        let mut statements = 0;
        let mut previous = None;
        for &(fact, _) in &self.entries {
            if previous != Some(fact) {
                statements += 1;
                previous = Some(fact);
            }
        }
        (statements, self.entries.len())
    }
}

fn triple(quad: EncodedQuad) -> Triple {
    [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()]
}

/// One revision's sets, computed once.
type Cell = Arc<OnceLock<Option<Arc<SupportSets>>>>;

/// A reader's view of one revision, computed once.
type View = (Arc<GraphAccess>, Arc<OnceLock<Snapshot>>);

/// The store's support graph sets and restricted views, for the latest revision read.
#[derive(Debug, Default)]
pub(crate) struct Supports {
    /// The rules the inferred stack is maintained with, and how
    /// ([`crate::StoreService::use_reasoning_rules`]).
    rules: Mutex<Option<RuleProgram>>,
    latest: Mutex<Option<(Snapshot, Cell, Vec<View>)>>,
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
        // Views of other rules' sets are stale.
        *self.latest.lock().unwrap_or_else(|p| p.into_inner()) = None;
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
            let mut latest = self.latest.lock().unwrap_or_else(|p| p.into_inner());
            if !latest
                .as_ref()
                .is_some_and(|(seen, ..)| seen.same_version(snapshot))
            {
                *latest = Some((snapshot.clone(), Cell::default(), Vec::new()));
            }
            let (_, cell, views) = latest.as_mut().expect("set above");
            let view = match views.iter().find(|(seen, _)| **seen == **access) {
                Some((_, view)) => Arc::clone(view),
                None => {
                    if views.len() >= VIEWS {
                        views.remove(0);
                    }
                    let view = Arc::<OnceLock<Snapshot>>::default();
                    views.push((Arc::clone(access), Arc::clone(&view)));
                    view
                }
            };
            (Arc::clone(cell), view)
        };
        view.get_or_init(|| {
            let sets = cell.get_or_init(|| self.compute(snapshot, compilation));
            match sets {
                Some(sets) => snapshot.with_inferred_subset(sets.subset(snapshot, access)),
                None => snapshot.with_inferred_subset(InferredSubset::Only(Vec::new())),
            }
        })
        .clone()
    }

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
        let started = std::time::Instant::now();
        let sets = SupportSets::compute(&program, snapshot, compilation.cap.max(1));
        let (statements, total) = sets.len();
        tracing::info!(
            revision = snapshot.revision(),
            statements,
            sets = total,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "support graph sets computed"
        );
        Some(Arc::new(sets))
    }
}
