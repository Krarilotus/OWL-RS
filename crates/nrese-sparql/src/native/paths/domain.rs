//! The query path node domain through the existing admitted engine streams.

use super::{NativeResult, PathEvaluator, PathGraph, graph_stopped, merge_union};
use nrese_engine::quad::Permutation;
use nrese_engine::{QuadPattern, TermId};
use nrese_exec::{IdTable, Ticker};

impl PathEvaluator<'_> {
    /// The existing exact/all-graph domain access orders, shared with the legacy fallback.
    pub(super) fn node_group_plan(&self) -> Option<(QuadPattern, Permutation, Permutation)> {
        Some(match self.graph {
            PathGraph::Merged(None) => (QuadPattern::all(), Permutation::Spog, Permutation::Ospg),
            PathGraph::Merged(Some(_)) => return None,
            PathGraph::Default => (
                QuadPattern::in_graph(TermId::DEFAULT_GRAPH),
                Permutation::Gspo,
                Permutation::Gosp,
            ),
            PathGraph::Named(graph) => (
                QuadPattern::in_graph(graph),
                Permutation::Gspo,
                Permutation::Gosp,
            ),
        })
    }

    /// The number of [`Self::nodes`], without collecting them (from the engine's
    /// statistics where the graph is one graph or all of them).
    pub(super) fn node_count(&self) -> NativeResult<u64> {
        if let Some((pattern, _, _)) = self.node_group_plan() {
            let cancellation = self.cancellation;
            let stop = move || cancellation.is_some_and(crate::CancellationToken::is_cancelled);
            if let Some(count) = self
                .snapshot
                .node_count_in_within(self.model, pattern.graph, self.budget, &stop)
                .map_err(graph_stopped)?
            {
                return Ok(count);
            }
        }
        let (subjects, objects) = self.subjects_and_objects();
        let mut count = 0;
        merge_union(&subjects, &objects, |_| count += 1);
        Ok(count)
    }

    /// The star diagonal's node domain, streamed into its final charged table.
    /// O(subject groups + object groups) caller merge; the engine owns preparation.
    pub(super) fn star_domain_within(&self) -> NativeResult<Option<IdTable>> {
        let Some((pattern, by_subject, by_object)) = self.node_group_plan() else {
            return Ok(None);
        };
        let cancellation = self.cancellation;
        let stop = move || cancellation.is_some_and(crate::CancellationToken::is_cancelled);
        let mut work = Ticker::new(&stop).map_err(graph_stopped)?;
        let Some(mut subjects) = self
            .snapshot
            .group_counts_in_within(self.model, &pattern, by_subject, self.budget, &stop)
            .map_err(graph_stopped)?
        else {
            return Ok(None);
        };
        let Some(mut objects) = self
            .snapshot
            .group_counts_in_within(self.model, &pattern, by_object, self.budget, &stop)
            .map_err(graph_stopped)?
        else {
            return Ok(None);
        };
        let mut subject = subjects.next().transpose().map_err(graph_stopped)?;
        let mut object = objects.next().transpose().map_err(graph_stopped)?;
        let mut out = IdTable::charged(1, 0, self.budget)?;
        while subject.is_some() || object.is_some() {
            let node = match (subject, object) {
                (Some((s, _)), Some((o, _))) if s < o => {
                    work.tick(1).map_err(graph_stopped)?;
                    subject = subjects.next().transpose().map_err(graph_stopped)?;
                    s
                }
                (Some((s, _)), Some((o, _))) if s > o => {
                    work.tick(1).map_err(graph_stopped)?;
                    object = objects.next().transpose().map_err(graph_stopped)?;
                    o
                }
                (Some((s, _)), Some(_)) => {
                    work.tick(2).map_err(graph_stopped)?;
                    subject = subjects.next().transpose().map_err(graph_stopped)?;
                    object = objects.next().transpose().map_err(graph_stopped)?;
                    s
                }
                (Some((s, _)), None) => {
                    work.tick(1).map_err(graph_stopped)?;
                    subject = subjects.next().transpose().map_err(graph_stopped)?;
                    s
                }
                (None, Some((o, _))) => {
                    work.tick(1).map_err(graph_stopped)?;
                    object = objects.next().transpose().map_err(graph_stopped)?;
                    o
                }
                (None, None) => unreachable!("the union has a head"),
            };
            // Advancing first releases consumed engine parts before output growth.
            out.push_row(&[node.raw()], self.budget)?;
        }
        work.finish().map_err(graph_stopped)?;
        Ok(Some(out))
    }
}
