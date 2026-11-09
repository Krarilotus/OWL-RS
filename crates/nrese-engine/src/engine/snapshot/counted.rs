//! A snapshot's counted reads ([`CountedSnapshot`]; their contract: [`crate::visit`]).
//!
//! Each read here is the twin of a [`Snapshot`] read, its scans, probes, equality copies and
//! class builds counted on the reader's visitor where the work is done (REAS-3, design note
//! `2026-11-09-p2-reas3-design.md` §1). The uncounted reads are left as they were: what an
//! uncounted reader runs is not this code.

use std::sync::Arc;

use super::{CopyVisibility, Either, Snapshot, SortedMerge};
use crate::engine::equality::{Classes as EqualityClasses, Expand};
use crate::engine::{ReadModel, Stack};
use crate::index::CountedScan;
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};
use crate::visit::{Stopped, Visitor};
use crate::{Count, SnapshotIdentity, TermId};

/// A snapshot whose data is read only counted: each read takes its reader's visitor and
/// counts the work it does on it ([`crate::visit`]), and answers [`Stopped`] once the
/// visitor has stopped. Readers whose stop must see their reads (the reasoner's and the
/// store's adapters) hold one instead of a [`Snapshot`], so an uncounted read of its data
/// doesn't compile there; [`Self::uncounted`] is the one way back, for the callers
/// `clippy.toml` names (REAS-3, master 9 Nov 01:37).
#[derive(Clone)]
pub struct CountedSnapshot {
    snapshot: Snapshot,
}

impl Snapshot {
    /// This snapshot, read only counted.
    pub fn counted(&self) -> CountedSnapshot {
        CountedSnapshot {
            snapshot: self.clone(),
        }
    }
}

impl CountedSnapshot {
    /// The snapshot itself, its reads uncounted: only for a reader that runs to its end or
    /// hands the snapshot on unread (a cache's admission, a view). Disallowed by clippy outside
    /// the callers `clippy.toml` names.
    pub fn uncounted(&self) -> &Snapshot {
        &self.snapshot
    }

    /// [`Snapshot::len_in`]: O(1).
    pub fn len_in(&self, model: ReadModel) -> u64 {
        self.snapshot.len_in(model)
    }

    /// [`Snapshot::revision`].
    pub fn revision(&self) -> u64 {
        self.snapshot.revision()
    }

    /// [`Snapshot::identity`].
    pub fn identity(&self) -> SnapshotIdentity {
        self.snapshot.identity()
    }

    /// [`Snapshot::scan_bytes`]: what a scan holds while it runs.
    pub fn scan_bytes(&self, model: ReadModel) -> usize {
        self.snapshot.scan_bytes(model)
    }

    /// [`Snapshot::same_version`].
    pub fn same_version(&self, other: &Snapshot) -> bool {
        self.snapshot.same_version(other)
    }

    /// [`Snapshot::quads_for_pattern_in`], counted: its quads, then [`Stopped`] if `visit`
    /// stopped it (and nothing after that).
    pub fn quads<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'a>,
    ) -> impl Iterator<Item = Result<EncodedQuad, Stopped>> + use<'a> {
        Ended::new(self.snapshot.quads_visited(model, pattern, visit), visit)
    }

    /// [`Snapshot::scan_sorted_in`], counted as [`Self::quads`]; `None` as there.
    pub fn scan_sorted<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        visit: Visitor<'a>,
    ) -> Option<impl Iterator<Item = Result<EncodedQuad, Stopped>> + use<'a>> {
        let quads = self
            .snapshot
            .scan_sorted_visited(model, pattern, permutation, visit)?;
        Some(Ended::new(quads, visit))
    }

    /// [`Snapshot::contains_in`], counted.
    pub fn contains(
        &self,
        model: ReadModel,
        quad: &EncodedQuad,
        visit: Visitor<'_>,
    ) -> Result<bool, Stopped> {
        self.snapshot.contains_visited(model, quad, visit)
    }

    /// [`Snapshot::exists_in`], counted.
    pub fn exists(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'_>,
    ) -> Result<bool, Stopped> {
        self.snapshot.exists_visited(model, pattern, visit)
    }

    /// [`Snapshot::count_in`], counted.
    pub fn count(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'_>,
    ) -> Result<Count, Stopped> {
        self.snapshot
            .count_visited(model, pattern, visit)
            .ok_or(Stopped)
    }

    /// [`Snapshot::estimate_in`], counted.
    pub fn estimate(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'_>,
    ) -> Result<u64, Stopped> {
        self.snapshot.estimate_visited(model, pattern, visit)
    }
}

/// A counted read's quads: [`Stopped`] where its producers ended because the visitor had
/// stopped, never the end of the data (REAS-2 attack list B9).
struct Ended<'v, I> {
    quads: I,
    visit: Visitor<'v>,
    done: bool,
}

impl<'v, I> Ended<'v, I> {
    fn new(quads: I, visit: Visitor<'v>) -> Self {
        Self {
            quads,
            visit,
            done: false,
        }
    }
}

impl<I: Iterator<Item = EncodedQuad>> Iterator for Ended<'_, I> {
    type Item = Result<EncodedQuad, Stopped>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.quads.next() {
            Some(quad) => Some(Ok(quad)),
            None => {
                self.done = true;
                crate::visit::stopped(self.visit).then_some(Err(Stopped))
            }
        }
    }
}

impl Snapshot {
    /// [`Snapshot::quads_for_pattern_in`]'s work, counted ([`Ended`] tells a stop).
    fn quads_visited<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'a>,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        let pattern = *self.read_pattern(model, pattern);
        // A class build that stopped reads nothing more: the stack scans below find their
        // visitor stopped and end at once.
        if let Ok(Some(classes)) = self.expanding_with(model, &pattern, visit) {
            let permutation = AccessPlan::for_pattern(&classes.normalised(&pattern)).permutation;
            let expanded = self
                .expanded_sorted_visited(classes, model, &pattern, permutation, visit)
                .expect("every layout keeps the permutation of a pattern's access plan");
            return Either::Left(expanded);
        }
        let data = self.data(model);
        Either::Right(
            Stack::ALL
                .into_iter()
                .filter(move |&stack| model.includes(stack))
                .flat_map(move |stack| data.stack(stack).scan_counted(&pattern, visit)),
        )
    }

    /// [`Snapshot::scan_sorted_in`]'s work, counted.
    fn scan_sorted_visited<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        visit: Visitor<'a>,
    ) -> Option<impl Iterator<Item = EncodedQuad> + use<'a>> {
        let pattern = *self.read_pattern(model, pattern);
        if let Ok(Some(classes)) = self.expanding_with(model, &pattern, visit) {
            return self
                .expanded_sorted_visited(classes, model, &pattern, permutation, visit)
                .map(Either::Left);
        }
        let plan = AccessPlan::in_permutation(&pattern, permutation)?;
        let supported = Stack::ALL.into_iter().all(|stack| {
            !model.includes(stack) || self.data(model).stack(stack).layout().supports(permutation)
        });
        if !supported {
            return None;
        }
        let scan = |stack: Stack| {
            model
                .includes(stack)
                .then(|| {
                    self.data(model)
                        .stack(stack)
                        .scan_plan_iter_counted(&plan, visit)
                })
                .into_iter()
                .flatten()
        };
        Some(Either::Right(SortedMerge {
            left: scan(Stack::Asserted).peekable(),
            right: scan(Stack::Inferred).peekable(),
            permutation,
        }))
    }

    /// `Snapshot::expanded_sorted`'s work, counted: its stored scans counted, its copies and
    /// visibility probes on the expansion's meter (REAS-3, sweeps SW013–SW015).
    fn expanded_sorted_visited<'a>(
        &'a self,
        classes: Arc<EqualityClasses>,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        visit: Visitor<'a>,
    ) -> Option<impl Iterator<Item = EncodedQuad> + use<'a>> {
        let data = self.data(model);
        let normalised = classes.normalised(pattern);
        let plan = AccessPlan::in_permutation(&normalised, permutation)?;
        let supported = Stack::ALL
            .into_iter()
            .all(|stack| data.stack(stack).layout().supports(permutation));
        if !supported {
            return None;
        }
        let named = model == ReadModel::Materialised && pattern.graph == GraphSelector::Any;
        let renamed = normalised != *pattern && !self.canonical;
        let named_plan = match named && renamed {
            true => Some(AccessPlan::in_permutation(pattern, permutation)?),
            false => None,
        };
        let stored: SortedMerge<CountedScan<'a, 'a>, CountedScan<'a, 'a>> = SortedMerge {
            left: data
                .asserted
                .scan_plan_iter_counted(&plan, visit)
                .peekable(),
            right: data
                .inferred
                .scan_plan_iter_counted(&plan, visit)
                .peekable(),
            permutation,
        };
        let expanded = Expand::metered(
            stored,
            classes,
            *pattern,
            permutation,
            named && !renamed,
            model,
            data,
            visit,
        )
        .charging(self.reads.clone());
        let named = named_plan
            .into_iter()
            .flat_map(move |plan| data.asserted.scan_plan_iter_counted(&plan, visit))
            .filter(|quad| !quad.graph.is_default_graph());
        Some(SortedMerge {
            left: expanded.peekable(),
            right: named.peekable(),
            permutation,
        })
    }

    /// [`Snapshot::contains_in`]'s work, counted: its run probes, and under equality its
    /// class build and visibility probes.
    fn contains_visited(
        &self,
        model: ReadModel,
        quad: &EncodedQuad,
        visit: Visitor<'_>,
    ) -> Result<bool, Stopped> {
        let named = self
            .read_pattern(
                model,
                &QuadPattern {
                    subject: Some(quad.subject),
                    predicate: Some(quad.predicate),
                    object: Some(quad.object),
                    graph: GraphSelector::Exact(quad.graph),
                },
            )
            .into_owned();
        let GraphSelector::Exact(graph) = named.graph else {
            unreachable!("an exact graph stays exact")
        };
        let quad = &EncodedQuad::new(
            named.subject.unwrap_or(quad.subject),
            named.predicate.unwrap_or(quad.predicate),
            named.object.unwrap_or(quad.object),
            graph,
        );
        if quad.graph.is_default_graph()
            && let Some(classes) =
                self.expanding_with(model, &QuadPattern::in_graph(TermId::DEFAULT_GRAPH), visit)?
        {
            let canonical = EncodedQuad::new(
                classes.representative(quad.subject),
                classes.representative(quad.predicate),
                classes.representative(quad.object),
                quad.graph,
            );
            let mut stored = false;
            for stack in Stack::ALL {
                stored = stored
                    || self
                        .data(model)
                        .stack(stack)
                        .contains_counted(&canonical, visit)?;
            }
            let shown = stored
                && CopyVisibility::new(self.data(model), model).shows_with(
                    quad,
                    classes.touches(&canonical),
                    visit,
                );
            // A visibility probe that saw the stop answered for nothing.
            if crate::visit::stopped(visit) {
                return Err(Stopped);
            }
            return Ok(shown);
        }
        for stack in Stack::ALL {
            if model.includes(stack)
                && self
                    .data(model)
                    .stack(stack)
                    .contains_counted(quad, visit)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// [`Snapshot::exists_in`]'s work, counted.
    fn exists_visited(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'_>,
    ) -> Result<bool, Stopped> {
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        if let Some(classes) = self.expanding_with(model, pattern, visit)? {
            let count = self
                .expanded_count(&classes, model, pattern, 1, visit)
                .ok_or(Stopped)?;
            return Ok(count.at_least() > 0);
        }
        if pattern.graph == GraphSelector::AnyNamed {
            let count = self.count_visited(model, pattern, visit).ok_or(Stopped)?;
            return Ok(count.at_least() > 0);
        }
        let plan = AccessPlan::for_pattern(pattern);
        for stack in Stack::ALL {
            if model.includes(stack)
                && self
                    .data(model)
                    .stack(stack)
                    .any_plan_counted(&plan, visit)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// [`Snapshot::estimate_in`]'s work, counted.
    fn estimate_visited(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'_>,
    ) -> Result<u64, Stopped> {
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        let Some(classes) = self.expanding_with(model, pattern, visit)? else {
            let count = self.count_visited(model, pattern, visit).ok_or(Stopped)?;
            return Ok(count.at_least());
        };
        let stored = Snapshot {
            version: Arc::clone(self.data(model)),
            ..self.stored()
        };
        let normalised = classes.normalised(pattern);
        let mut estimate = stored
            .count_visited(ReadModel::Materialised, &normalised, visit)
            .ok_or(Stopped)?
            .at_least();
        if normalised != *pattern && pattern.graph == GraphSelector::Any && !self.canonical {
            estimate += stored
                .count_visited(ReadModel::Asserted, pattern, visit)
                .ok_or(Stopped)?
                .at_least();
        }
        Ok(estimate)
    }
}
