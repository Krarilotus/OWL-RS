//! A snapshot's counted reads ([`CountedSnapshot`]; their contract: [`crate::visit`]).
//!
//! Each read here is the twin of a [`Snapshot`] read, its scans, probes, equality copies and
//! class builds counted on the reader's visitor where the work is done (REAS-3, design note
//! `2026-11-09-p2-reas3-design.md` §1). The uncounted reads are left as they were: what an
//! uncounted reader runs is not this code.

use std::sync::Arc;

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

    /// The quads of `[s, p, o]` in any graph, as [`Self::quads`] answers them, read on one of
    /// three paths chosen from one ask of whether the read expands equality classes (design
    /// note `2026-11-14-d16-543-point-reads-design.md`):
    /// - where it does, [`Self::quads`]'s expanded read of those classes (Asserted reads never
    ///   expand);
    /// - else, where making the scans could count more than [`nrese_exec::POLL_UNITS`]
    ///   (`Snapshot::point_cost`), [`Self::quads`]'s counted read of the stacks;
    /// - else the point read: each stack's scan made as the one before ends, what making it
    ///   counts asked of `visit` in one visit before it is made, uncounted, and its merge steps
    ///   counted on a ledger of its own (so no units one visit allowed reach past the next).
    ///
    /// Every refusal answers [`Stopped`].
    pub fn point_quads<'a>(
        &'a self,
        model: ReadModel,
        [s, p, o]: [TermId; 3],
        visit: Visitor<'a>,
    ) -> impl Iterator<Item = Result<EncodedQuad, Stopped>> + use<'a> {
        let snapshot = &self.snapshot;
        let pattern = QuadPattern {
            subject: Some(s),
            predicate: Some(p),
            object: Some(o),
            graph: GraphSelector::Any,
        };
        let pattern = *snapshot.read_pattern(model, &pattern);
        let plan = AccessPlan::for_pattern(&pattern);
        let (costs, refused) = match snapshot.expanding_with(model, &pattern, visit) {
            Ok(Some(classes)) => {
                let expanded = snapshot.expanded_visited(classes, model, &pattern, visit);
                return Path::Read(Ended::new(Either::Left(expanded), visit));
            }
            Ok(None) => match snapshot.point_cost(model, &plan) {
                Some(costs) => {
                    #[cfg(any(test, all(feature = "witness", debug_assertions)))]
                    crate::witness::add(crate::witness::Site::PointReads, 1);
                    (costs, false)
                }
                None => {
                    let stacks = snapshot.stacks_visited(model, pattern, visit);
                    return Path::Read(Ended::new(Either::Right(stacks), visit));
                }
            },
            Err(Stopped) => ([0; 2], true),
        };
        Path::Point(PointQuads {
            data: snapshot.data(model),
            model,
            plan,
            stacks: Stack::ALL.into_iter().zip(costs),
            refused,
            visit,
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

    /// [`Self::count`], asked only up to `limit`: at least `min(count, limit)`, never more
    /// than the count, and [`Count::AtLeast`] where an expanded count stopped at `limit` (a
    /// held count answers exactly). For a caller that needs only whether the count reaches
    /// `limit`.
    pub fn count_up_to(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        limit: u64,
        visit: Visitor<'_>,
    ) -> Result<Count, Stopped> {
        self.snapshot
            .count_with(model, pattern, limit, visit)
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

/// [`CountedSnapshot::point_quads`]'s paths: one of [`CountedSnapshot::quads`]'s reads, or the
/// point read.
enum Path<R, P> {
    Read(R),
    Point(P),
}

impl<R, P> Iterator for Path<R, P>
where
    R: Iterator<Item = Result<EncodedQuad, Stopped>>,
    P: Iterator<Item = Result<EncodedQuad, Stopped>>,
{
    type Item = Result<EncodedQuad, Stopped>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Read(quads) => quads.next(),
            Self::Point(quads) => quads.next(),
        }
    }
}

/// [`CountedSnapshot::point_quads`]'s point read: each stack's scan made as the one before
/// ends, what making it counts asked first in one visit, then stepped on a ledger of its own.
struct PointQuads<'a> {
    data: &'a Version,
    model: ReadModel,
    plan: AccessPlan,
    /// The stacks still to read, with what making each one's scan counts.
    stacks: std::iter::Zip<std::array::IntoIter<Stack, 2>, std::array::IntoIter<u64, 2>>,
    /// Whether the class build stopped the read before it began.
    refused: bool,
    visit: Visitor<'a>,
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
        if self.refused {
            self.done = true;
            return Some(Err(Stopped));
        }
        loop {
            if self.scan.is_none() {
                let model = self.model;
                let Some((stack, cost)) = self.stacks.find(|&(stack, _)| model.includes(stack))
                else {
                    self.done = true;
                    return None;
                };
                if !(self.visit)(cost) {
                    self.done = true;
                    return Some(Err(Stopped));
                }
                self.ledger = Ledger::new(self.visit);
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
            return Either::Left(self.expanded_visited(classes, model, &pattern, visit));
        }
        Either::Right(self.stacks_visited(model, pattern, visit))
    }

    /// [`Self::quads_visited`]'s expanded read of `classes`, in its access plan's permutation.
    fn expanded_visited<'a>(
        &'a self,
        classes: Arc<EqualityClasses>,
        model: ReadModel,
        pattern: &QuadPattern,
        visit: Visitor<'a>,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        let permutation = AccessPlan::for_pattern(&classes.normalised(pattern)).permutation;
        self.expanded_sorted_visited(classes, model, pattern, permutation, visit)
            .expect("every layout keeps the permutation of a pattern's access plan")
    }

    /// [`Self::quads_visited`]'s read of the stacks, a counted scan each.
    fn stacks_visited<'a>(
        &'a self,
        model: ReadModel,
        pattern: QuadPattern,
        visit: Visitor<'a>,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        let data = self.data(model);
        Stack::ALL
            .into_iter()
            .filter(move |&stack| model.includes(stack))
            .flat_map(move |stack| data.stack(stack).scan_counted(&pattern, visit))
    }

    /// The most making `plan`'s scan of each stack in `model` counts on
    /// [`Self::stacks_visited`]'s path, in [`Stack::ALL`]'s order (0 for a stack the model
    /// leaves out), worked out from the runs' sizes alone ([`IndexVersion::scan_cost`]): `None`
    /// once their sum is over [`nrese_exec::POLL_UNITS`], the runs after that left unread.
    pub(super) fn point_cost(&self, model: ReadModel, plan: &AccessPlan) -> Option<[u64; 2]> {
        let data = self.data(model);
        let mut costs = [0; 2];
        let mut most = u64::from(nrese_exec::POLL_UNITS);
        for (stack, cost) in Stack::ALL.into_iter().zip(&mut costs) {
            if model.includes(stack) {
                *cost = data.stack(stack).scan_cost(plan, most)?;
                most -= *cost;
            }
        }
        Some(costs)
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
