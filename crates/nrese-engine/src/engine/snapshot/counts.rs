//! Exact grouped reads with request-owned scratch and the existing scalar memo.

use std::sync::atomic::{AtomicBool, Ordering};

use nrese_exec::{Budget, Stop, Ticker};
use parking_lot::Mutex;

use super::{ReadBudget, ReadModel, Snapshot, Stack};
use crate::index::groups::Groups;
use crate::quad::{AccessPlan, GraphSelector, Permutation, QuadPattern};
use crate::term::TermId;

#[cfg(test)]
mod tests;

struct Counts<'a, F: Fn() -> bool + Sync + ?Sized> {
    groups: Groups<'a, F>,
    reads: Option<ReadBudget>,
    done: bool,
}

impl<F: Fn() -> bool + Sync + ?Sized> Iterator for Counts<'_, F> {
    type Item = Result<(TermId, u64), Stop>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if let Some(refused) = self.reads.as_ref().and_then(|r| r.refused.get()).copied() {
            self.groups.discard();
            self.done = true;
            return Some(Err(Stop::Memory(refused)));
        }
        let next = self.groups.next();
        if let Some(refused) = self.reads.as_ref().and_then(|r| r.refused.get()).copied() {
            self.groups.discard();
            self.done = true;
            return Some(Err(Stop::Memory(refused)));
        }
        self.done = !matches!(next, Some(Ok(_)));
        next
    }
}

impl Snapshot {
    fn check_count_read(&self) -> Result<(), Stop> {
        self.read_refused().map_or(Ok(()), |e| Err(Stop::Memory(e)))
    }

    /// [`Self::group_counts_in`] with admitted parallel parts and fallible sorted emission.
    /// `None` means an unsupported shape/layout/equality scope, not a refused read. Empty
    /// supported input yields an empty iterator. Preparation finishes before this returns;
    /// the iterator owns its temporary parts and borrows only `budget` and `stop`.
    #[expect(
        clippy::type_complexity,
        reason = "keep the fallible iterator's owner private"
    )]
    pub fn group_counts_in_within<'a, F>(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        budget: &'a Budget,
        stop: &'a F,
    ) -> Result<Option<impl Iterator<Item = Result<(TermId, u64), Stop>> + use<'a, F>>, Stop>
    where
        F: Fn() -> bool + Sync + ?Sized,
    {
        self.check_count_read()?;
        let mut ticker =
            Ticker::new(stop).map_err(|e| self.read_refused().map_or(e, Stop::Memory))?;
        self.check_count_read()?;
        let pattern = self.read_pattern(model, pattern);
        if self.expanding(model, &pattern).is_some() {
            return Ok(None);
        }
        let Some(plan) = AccessPlan::in_permutation(&pattern, permutation) else {
            return Ok(None);
        };
        let bound = plan
            .low
            .iter()
            .zip(&plan.high)
            .take_while(|(l, h)| l == h)
            .count();
        let data = self.data(model);
        if plan.exclude_default_graph
            || bound >= 4
            || Stack::ALL.into_iter().any(|stack| {
                model.includes(stack) && !data.stack(stack).layout().supports(permutation)
            })
        {
            return Ok(None);
        }
        let runs = Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack))
            .try_fold(0usize, |n, stack| {
                n.checked_add(data.stack(stack).runs().len())
            })
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        let mut groups = Groups::new(budget, stop, runs)?;
        #[cfg(test)]
        self.statistics
            .group_preparations
            .fetch_add(1, Ordering::Relaxed);
        for stack in Stack::ALL {
            if model.includes(stack) {
                let prepared = groups.prepare(data.stack(stack), &plan, bound, stop);
                self.check_count_read()?;
                prepared?;
            }
        }
        self.check_count_read()?;
        ticker.finish()?;
        Ok(Some(Counts {
            groups,
            reads: self.reads.clone(),
            done: false,
        }))
    }

    /// Exact distinct subject/object union for one graph or every graph. A warm memo hit
    /// prepares no groups. Unsupported scopes return `None`; memory/cancellation errors
    /// propagate and never publish a partial scalar to the version's memo.
    pub fn node_count_in_within<F>(
        &self,
        model: ReadModel,
        graphs: GraphSelector,
        budget: &Budget,
        stop: &F,
    ) -> Result<Option<u64>, Stop>
    where
        F: Fn() -> bool + Sync + ?Sized,
    {
        self.check_count_read()?;
        let mut ticker =
            Ticker::new(stop).map_err(|e| self.read_refused().map_or(e, Stop::Memory))?;
        self.check_count_read()?;
        // The old scalar API's wildcard arm includes default quads for AnyNamed.
        // This additive API only implements its documented exact/all graph scopes.
        if graphs == GraphSelector::AnyNamed {
            return Ok(None);
        }
        let pattern = self
            .read_pattern(
                model,
                &QuadPattern {
                    graph: graphs,
                    ..QuadPattern::all()
                },
            )
            .into_owned();
        if self.expanding(model, &pattern).is_some()
            || (self.canonical_data.is_some() && model != ReadModel::Asserted)
        {
            return Ok(None);
        }
        let graphs = pattern.graph;
        // The cold callback's closing checks precede publication. No fallible work may
        // follow a success: a later stop must not make a published answer a failed call.
        // Warm hits were checked at entry and prepare no groups.
        self.statistics
            .try_node_count(&self.version, model, graphs, || {
                let (pattern, by_subject, by_object) = match graphs {
                    GraphSelector::Exact(graph) => (
                        QuadPattern::in_graph(graph),
                        Permutation::Gspo,
                        Permutation::Gosp,
                    ),
                    GraphSelector::Any => {
                        (QuadPattern::all(), Permutation::Spog, Permutation::Ospg)
                    }
                    GraphSelector::AnyNamed => unreachable!("rejected before the cache"),
                };
                let failed = AtomicBool::new(false);
                let error = Mutex::new(None);
                let check = || failed.load(Ordering::Relaxed) || stop();
                let prepare = |permutation| {
                    let result =
                        self.group_counts_in_within(model, &pattern, permutation, budget, &check);
                    if let Err(e) = &result {
                        error.lock().get_or_insert(*e);
                        failed.store(true, Ordering::Relaxed);
                    }
                    result
                };
                let (subjects, objects) =
                    rayon::join(|| prepare(by_subject), || prepare(by_object));
                if let Some(e) = error.into_inner() {
                    return Err(e);
                }
                let (Some(mut subjects), Some(mut objects)) = (subjects?, objects?) else {
                    return Ok(None);
                };
                let mut subject = subjects.next().transpose()?;
                let mut object = objects.next().transpose()?;
                let mut count = 0u64;
                while subject.is_some() || object.is_some() {
                    ticker.tick(1)?;
                    count = count.checked_add(1).ok_or(Stop::TooLarge(
                        nrese_exec::narrow::RepresentationLimit {
                            what: "a node count",
                            limit: u64::MAX,
                        },
                    ))?;
                    match (subject, object) {
                        (Some((s, _)), Some((o, _))) if s < o => {
                            subject = subjects.next().transpose()?
                        }
                        (Some((s, _)), Some((o, _))) if s > o => {
                            object = objects.next().transpose()?
                        }
                        (Some(_), Some(_)) => {
                            subject = subjects.next().transpose()?;
                            object = objects.next().transpose()?;
                        }
                        (Some(_), None) => subject = subjects.next().transpose()?,
                        (None, Some(_)) => object = objects.next().transpose()?,
                        (None, None) => break,
                    }
                }
                self.check_count_read()?;
                ticker.finish()?;
                Ok(Some(count))
            })
            .map_err(|e| self.read_refused().map_or(e, Stop::Memory))
    }
}
