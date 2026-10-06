//! Probe cursors: a snapshot's index read one pattern after another, each seek starting
//! where the last one landed ([`crate::index::cursor`]).
//!
//! Index joins, sideways probes, a worst-case-optimal join's checks and membership tests
//! read many small ranges of one shape, mostly in ascending key order. A [`ProbeCursor`]
//! answers each like [`Snapshot::quads_for_pattern_in`] does, without a search from the
//! root per run and pattern and without allocating per probe; [`ProbeCursor::seek`] is
//! the leapfrog step, reporting the next value a component takes where the pattern
//! doesn't match.
//!
//! Reads that expand equality classes, or that ask for the named graphs only, are
//! answered by the snapshot's own scans (each counted as a search from the root).

use super::Snapshot;
use crate::engine::{ReadModel, Stack};
use crate::index::cursor::{SeekStats, StackCursor};
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};

/// The outcome of a [`ProbeCursor::seek`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seek {
    /// The pattern matches.
    Found,
    /// It doesn't; the smallest value above the asked one that the component takes
    /// among the matches of the pattern without it is this (raw id). Every value in
    /// between can be skipped.
    Next(u64),
    /// It doesn't, and no larger value of the component matches either.
    Exhausted,
    /// It doesn't; no next value is known (no index order ends with the component).
    Missing,
}

/// Which positions of a pattern are bound (subject, predicate, object, graph), and the
/// component a leapfrog step asks for.
type Shape = ([bool; 4], usize);

/// A snapshot's index in one read model, read probe after probe ([module docs](self)).
pub struct ProbeCursor<'a> {
    snapshot: &'a Snapshot,
    model: ReadModel,
    /// Per stack (asserted, inferred): a cursor in the permutation the last probe read.
    stacks: [Option<StackCursor<'a>>; 2],
    /// The order the last leapfrog step read, for its pattern's shape (which positions
    /// are bound, the component, whether the graph is): shapes repeat probe after probe.
    leapfrog: Option<(Shape, Option<Permutation>)>,
    stats: SeekStats,
}

impl Snapshot {
    /// A cursor reading this snapshot's matches in `model` one pattern after another
    /// ([`ProbeCursor`]).
    pub fn probe_cursor(&self, model: ReadModel) -> ProbeCursor<'_> {
        ProbeCursor {
            snapshot: self,
            model,
            stacks: [None, None],
            leapfrog: None,
            stats: SeekStats::default(),
        }
    }
}

impl<'a> ProbeCursor<'a> {
    /// Calls `f` with each quad matching `pattern`: what
    /// [`Snapshot::quads_for_pattern_in`] yields, asserted matches first.
    pub fn for_each(&mut self, pattern: &QuadPattern, mut f: impl FnMut(EncodedQuad)) {
        if self.unseekable(pattern) {
            self.snapshot
                .quads_for_pattern_in(self.model, pattern)
                .for_each(f);
            return;
        }
        let plan = AccessPlan::for_pattern(pattern);
        self.scan(&plan, |quad| {
            f(quad);
            true
        });
    }

    /// Whether some quad matches `pattern`: [`Snapshot::exists_in`].
    pub fn exists(&mut self, pattern: &QuadPattern) -> bool {
        if self.unseekable(pattern) {
            return self.snapshot.exists_in(self.model, pattern);
        }
        let plan = AccessPlan::for_pattern(pattern);
        if plan.exclude_default_graph {
            let mut found = false;
            self.scan(&plan, |_| {
                found = true;
                false
            });
            return found;
        }
        let mut found = false;
        self.first_each(&plan, |adapted| adapted.high, |_, _| found = true);
        found
    }

    /// The matches of `pattern` for planning: [`Snapshot::estimate_in`] (exact where no
    /// equality class is expanded), each run's range found from its finger.
    pub fn count(&mut self, pattern: &QuadPattern) -> u64 {
        if self.unseekable(pattern) {
            return self.snapshot.estimate_in(self.model, pattern);
        }
        let plan = AccessPlan::for_pattern(pattern);
        if plan.exclude_default_graph {
            return self.snapshot.estimate_in(self.model, pattern);
        }
        let mut count = 0;
        for (k, stack) in Stack::ALL.into_iter().enumerate() {
            if !self.model.includes(stack) {
                continue;
            }
            let index = self.snapshot.version().stack(stack);
            let Some(adapted) = index.layout().adapt(&plan) else {
                continue;
            };
            let cursor = cursor(&mut self.stacks[k], index, adapted.permutation);
            count += cursor.count(&adapted.low, &adapted.high, &mut self.stats);
        }
        count
    }

    /// Appends the values `component` (subject 0, predicate 1, object 2) takes in the
    /// matches of `pattern`, ascending and distinct, where it is the pattern's only
    /// unbound position (the graph bound); `false`, nothing appended, otherwise.
    pub fn values(&mut self, pattern: &QuadPattern, component: usize, out: &mut Vec<u64>) -> bool {
        let GraphSelector::Exact(_) = pattern.graph else {
            return false;
        };
        let bound = [pattern.subject, pattern.predicate, pattern.object].map(|t| t.is_some());
        if bound[component] || (0..3).any(|c| c != component && !bound[c]) {
            return false;
        }
        // A graph-first order with `component` last.
        let permutation = match component {
            0 => Permutation::Gpos,
            1 => Permutation::Gosp,
            _ => Permutation::Gspo,
        };
        if self.unseekable(pattern) {
            return false;
        }
        let Some(plan) = AccessPlan::in_permutation(pattern, permutation) else {
            return false;
        };
        let start = out.len();
        let mut stacks = 0;
        for (k, stack) in Stack::ALL.into_iter().enumerate() {
            if !self.model.includes(stack) {
                continue;
            }
            let index = self.snapshot.version().stack(stack);
            let Some(adapted) = index.layout().adapt(&plan) else {
                continue;
            };
            let before = out.len();
            let cursor = cursor(&mut self.stacks[k], index, adapted.permutation);
            cursor.scan(&adapted.low, &adapted.high, &mut self.stats, |key| {
                out.push(adapted.permutation.key_to_quad(key).components()[component]);
                true
            });
            stacks += usize::from(out.len() > before);
        }
        // Each stack's values ascend; the stacks are disjoint, so a value is in one.
        if stacks > 1 {
            out[start..].sort_unstable();
        }
        true
    }

    /// The leapfrog step: whether `pattern` matches, where `component` (subject 0,
    /// predicate 1, object 2) is bound in it; if not, the next value `component` takes
    /// among the matches with every other bound position as in `pattern`. Read in an
    /// index order whose bound prefix ends with `component`, where the layout has one.
    pub fn seek(&mut self, pattern: &QuadPattern, component: usize) -> Seek {
        let plan = match self.unseekable(pattern) {
            true => None,
            false => self.leapfrog_plan(pattern, component),
        };
        let Some(plan) = plan else {
            return match self.exists(pattern) {
                true => Seek::Found,
                false => Seek::Missing,
            };
        };
        // From the pattern's key up to the end of the prefix before `component`: the
        // first key there matches, or names the next value.
        let position = |permutation: Permutation| {
            permutation
                .order()
                .iter()
                .position(|&c| c == component)
                .expect("every component has a key position")
        };
        let (mut found, mut next) = (false, None::<u64>);
        let until = |adapted: &AccessPlan| {
            let mut high = adapted.low;
            high[position(adapted.permutation)..].fill(u64::MAX);
            high
        };
        self.first_each(&plan, until, |key, adapted| {
            if key <= adapted.high {
                found = true;
            } else {
                let value = key[position(adapted.permutation)];
                next = Some(next.map_or(value, |n| n.min(value)));
            }
        });
        match (found, next) {
            (true, _) => Seek::Found,
            (false, Some(value)) => Seek::Next(value),
            (false, None) => Seek::Exhausted,
        }
    }

    /// Calls `f` with each stack's first visible key in `[low, until(adapted)]` of
    /// `plan` adapted to the stack's layout (the stacks are disjoint, and their layouts
    /// may read in different orders, so keys of two stacks aren't compared).
    fn first_each(
        &mut self,
        plan: &AccessPlan,
        until: impl Fn(&AccessPlan) -> crate::quad::Key,
        mut f: impl FnMut(crate::quad::Key, &AccessPlan),
    ) {
        for (k, stack) in Stack::ALL.into_iter().enumerate() {
            if !self.model.includes(stack) {
                continue;
            }
            let index = self.snapshot.version().stack(stack);
            let Some(adapted) = index.layout().adapt(plan) else {
                continue;
            };
            let high = until(&adapted);
            let cursor = cursor(&mut self.stacks[k], index, adapted.permutation);
            if let Some(key) = cursor.first(&adapted.low, &high, &mut self.stats) {
                f(key, &adapted);
            }
        }
    }

    /// [`leapfrog_plan`] for `pattern`, its order cached for the pattern's shape.
    fn leapfrog_plan(&mut self, pattern: &QuadPattern, component: usize) -> Option<AccessPlan> {
        let shape = (
            [
                pattern.subject.is_some(),
                pattern.predicate.is_some(),
                pattern.object.is_some(),
                pattern.graph != GraphSelector::Any,
            ],
            component,
        );
        let permutation = match self.leapfrog {
            Some((cached, permutation)) if cached == shape => permutation,
            _ => {
                let permutation = leapfrog_plan(pattern, component).map(|plan| plan.permutation);
                self.leapfrog = Some((shape, permutation));
                permutation
            }
        };
        AccessPlan::in_permutation(pattern, permutation?)
    }

    /// The seeks so far and how many searched a run from the root.
    pub fn stats(&self) -> SeekStats {
        self.stats
    }

    /// Reads the cursors can't answer: equality expanded, or the named graphs only.
    fn unseekable(&mut self, pattern: &QuadPattern) -> bool {
        let unseekable = pattern.graph == GraphSelector::AnyNamed
            || self.snapshot.expanding(self.model, pattern).is_some();
        if unseekable {
            self.stats.seeks += 1;
            self.stats.root_searches += 1;
        }
        unseekable
    }

    /// Calls `f` with each quad `plan` matches in the model's stacks while it returns
    /// true.
    fn scan(&mut self, plan: &AccessPlan, mut f: impl FnMut(EncodedQuad) -> bool) {
        for (k, stack) in Stack::ALL.into_iter().enumerate() {
            if !self.model.includes(stack) {
                continue;
            }
            let index = self.snapshot.version().stack(stack);
            let Some(adapted) = index.layout().adapt(plan) else {
                continue;
            };
            let cursor = cursor(&mut self.stacks[k], index, adapted.permutation);
            let mut go = true;
            cursor.scan(&adapted.low, &adapted.high, &mut self.stats, |key| {
                let quad = adapted.permutation.key_to_quad(key);
                if adapted.exclude_default_graph && quad.graph.is_default_graph() {
                    return true;
                }
                go = f(quad);
                go
            });
            if !go {
                return;
            }
        }
    }
}

/// The stack's cursor in `permutation`, made anew where the last probe read another.
fn cursor<'a, 'c>(
    slot: &'c mut Option<StackCursor<'a>>,
    index: &'a crate::index::IndexVersion,
    permutation: Permutation,
) -> &'c mut StackCursor<'a> {
    if slot
        .as_ref()
        .is_none_or(|cursor| cursor.permutation() != permutation)
    {
        *slot = Some(StackCursor::new(index, permutation));
    }
    slot.as_mut().expect("set above")
}

/// An access plan for `pattern` in an order whose bound prefix ends with `component`
/// (then a key past the pattern's range with the same prefix before `component` names
/// the next value it takes), if one exists and both layouts can read it.
fn leapfrog_plan(pattern: &QuadPattern, component: usize) -> Option<AccessPlan> {
    const GRAPH_FIRST: [Permutation; 4] = [
        Permutation::Gspo,
        Permutation::Gpos,
        Permutation::Gosp,
        Permutation::Gpso,
    ];
    const GRAPH_LAST: [Permutation; 3] = [Permutation::Spog, Permutation::Posg, Permutation::Ospg];
    let candidates: &[Permutation] = match pattern.graph {
        GraphSelector::Exact(_) => &GRAPH_FIRST,
        GraphSelector::Any => &GRAPH_LAST,
        GraphSelector::AnyNamed => return None,
    };
    let bound = [
        pattern.subject.is_some(),
        pattern.predicate.is_some(),
        pattern.object.is_some(),
        pattern.graph != GraphSelector::Any,
    ];
    candidates.iter().find_map(|&permutation| {
        let order = permutation.order();
        let prefix = order.iter().take_while(|&&c| bound[c]).count();
        let last = prefix.checked_sub(1).map(|i| order[i]);
        if last != Some(component) {
            return None;
        }
        AccessPlan::in_permutation(pattern, permutation).filter(|plan| !plan.exclude_default_graph)
    })
}
