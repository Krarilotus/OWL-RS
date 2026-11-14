//! A snapshot's counted reads ([`CountedSnapshot`]; their contract: [`crate::visit`]).
//!
//! Each read here is the twin of a [`Snapshot`] read, its scans, probes, equality copies and
//! class builds counted on the reader's visitor where the work is done (REAS-3, design note
//! `2026-11-09-p2-reas3-design.md` §1). The uncounted reads are left as they were: what an
//! uncounted reader runs is not this code.

use std::sync::{Arc, OnceLock};

use super::{CopyVisibility, Either, Snapshot};
use crate::engine::equality::{Classes as EqualityClasses, Expand, Merged, OnMeter, Read, Step};
use crate::engine::{ReadModel, Stack, Version};
use crate::index::{IndexVersion, QuadScan};
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};
use crate::visit::{Account, Ledger, Stopped, Visitor};
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
    /// What making a point read's scans counts, by read model ([`Self::point_quads`]): worked
    /// out at the first point read in that model.
    point_costs: [OnceLock<u64>; 3],
}

impl Snapshot {
    /// This snapshot, read only counted.
    pub fn counted(&self) -> CountedSnapshot {
        CountedSnapshot {
            snapshot: self.clone(),
            point_costs: Default::default(),
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

    /// The quads of `[s, p, o]` in any graph, as [`Self::quads`] reads them, with the making of
    /// their scans charged up front: before the first quad, one visit of what
    /// [`Self::quads`] would count making them at most (each run's range search and its cursor
    /// in a merge whose every range is taken), worked out once per read model. Each merge step
    /// is then counted on one [`Ledger`] as in [`Self::quads`], and every refusal answers
    /// [`Stopped`].
    ///
    /// `None` where that charge is over [`nrese_exec::POLL_UNITS`] or the read expands
    /// equality classes: read with [`Self::quads`] instead (design note
    /// `2026-11-14-d16-543-point-reads-design.md`).
    pub fn point_quads<'a>(
        &'a self,
        model: ReadModel,
        [s, p, o]: [TermId; 3],
        visit: Visitor<'a>,
    ) -> Option<impl Iterator<Item = Result<EncodedQuad, Stopped>> + use<'a>> {
        let snapshot = &self.snapshot;
        let pattern = QuadPattern {
            subject: Some(s),
            predicate: Some(p),
            object: Some(o),
            graph: GraphSelector::Any,
        };
        let pattern = *snapshot.read_pattern(model, &pattern);
        let plan = AccessPlan::for_pattern(&pattern);
        let data = snapshot.data(model);
        // Every point plan reads one permutation, so its cost is the model's.
        let cost = *self.point_costs[model as usize].get_or_init(|| {
            Stack::ALL
                .into_iter()
                .filter(|&stack| model.includes(stack))
                .map(|stack| data.stack(stack).scan_cost(&plan))
                .sum()
        });
        if cost > u64::from(nrese_exec::POLL_UNITS) {
            return None;
        }
        let charge = match snapshot.expanding_with(model, &pattern, visit) {
            Ok(Some(_)) => return None,
            Ok(None) => Ok(cost),
            Err(Stopped) => Err(Stopped),
        };
        Some(PointQuads {
            data,
            model,
            plan,
            charge: Some(charge),
            visit,
            stacks: Stack::ALL.into_iter(),
            scan: None,
            ledger: Ledger::new(visit),
            done: false,
        })
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

/// [`CountedSnapshot::point_quads`]'s quads: its charge asked at the first `next`, then each
/// stack's scan made as the one before ends, uncounted as charged, and stepped on the read's
/// one ledger.
struct PointQuads<'a> {
    data: &'a Version,
    model: ReadModel,
    plan: AccessPlan,
    /// The charge still to ask, or the class build's refusal; `None` once asked.
    charge: Option<Result<u64, Stopped>>,
    visit: Visitor<'a>,
    stacks: std::array::IntoIter<Stack, 2>,
    scan: Option<QuadScan<'a>>,
    ledger: Ledger<'a>,
    done: bool,
}

impl Iterator for PointQuads<'_> {
    type Item = Result<EncodedQuad, Stopped>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.charge.take() {
            Some(Ok(cost)) if (self.visit)(cost) => {}
            None => {}
            Some(_) => {
                self.done = true;
                return Some(Err(Stopped));
            }
        }
        loop {
            if self.scan.is_none() {
                let model = self.model;
                let Some(stack) = self.stacks.find(|&stack| model.includes(stack)) else {
                    self.done = true;
                    return None;
                };
                self.scan = Some(self.data.stack(stack).scan_plan(&self.plan));
            }
            let scan = self.scan.as_mut().expect("made above");
            match scan.step_with(&mut self.ledger) {
                Ok(Some(quad)) => return Some(Ok(quad)),
                Ok(None) => self.scan = None,
                Err(Stopped) => {
                    self.done = true;
                    return Some(Err(Stopped));
                }
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
        // One account for both stacks' scans, which the merge interleaves (fix check r2 G1's
        // sibling: a ledger each allowed a run each after the same yes).
        let mut account = Ledger::new(visit);
        let mut scan = |stack: Stack| {
            model
                .includes(stack)
                .then(|| scan_on(self.data(model).stack(stack), &plan, &mut account))
        };
        let step = Merged::new(scan(Stack::Asserted), scan(Stack::Inferred), permutation);
        Some(Either::Right(Read { step, account }))
    }

    /// `Snapshot::expanded_sorted`'s work, counted: its stored scans, their merge, its copies,
    /// its visibility probes and its named graphs' scan on the read's one account (REAS-3,
    /// sweeps SW013–SW015; fix check r2 G1's sibling: the scans' ledgers and the expansion's
    /// credit allowed a run each after the same yes).
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
        let mut account = OnMeter::new(visit);
        let stored = Merged::new(
            scan_on(&data.asserted, &plan, &mut account),
            scan_on(&data.inferred, &plan, &mut account),
            permutation,
        );
        let expanded = Expand::of(
            stored,
            classes,
            *pattern,
            permutation,
            named && !renamed,
            model,
            data,
        )
        .charging(self.reads.clone());
        let named =
            NamedGraphs(named_plan.map(|plan| scan_on(&data.asserted, &plan, &mut account)));
        let step = Merged::new(expanded, named, permutation);
        Some(Read { step, account })
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
                    &mut OnMeter::new(visit),
                )?;
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

/// A scan of `index` for `plan` on a read's one account: nothing where the read stopped while it
/// was being made. The account's refusal is final, so the read's next step answers [`Stopped`]
/// too, whatever the account had left (fix check r3 H2).
fn scan_on<'a>(
    index: &'a IndexVersion,
    plan: &AccessPlan,
    account: &mut impl Account,
) -> QuadScan<'a> {
    index
        .scan_plan_with(plan, account)
        .unwrap_or_else(|Stopped| QuadScan::empty(plan.permutation))
}

/// An expanded read's named graphs where its classes rename the pattern's constants: read as
/// stored, with the pattern's own constants, their default graph left out.
struct NamedGraphs<'a>(Option<QuadScan<'a>>);

impl Step for NamedGraphs<'_> {
    fn step(&mut self, account: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped> {
        loop {
            match self.0.step(account)? {
                Some(quad) if quad.graph.is_default_graph() => continue,
                other => return Ok(other),
            }
        }
    }
}
