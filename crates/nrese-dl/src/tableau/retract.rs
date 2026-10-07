//! Dynamic backtracking (docs/design/owl2-dl-dynamic-backtracking.md): a clash whose
//! culprit level `k` lies below the top retracts `k` alone (every fact, node and pending
//! disjunction whose dependency set contains it) and re-decides `k`'s disjunction on top,
//! with a fresh level, instead of cutting away every level above `k`. On the W3C cases
//! DL-662 to 664, 99 % of the branch points rebuilt levels a backjump had discarded, and
//! none of those depended on the culprit.
//!
//! The frames above that depend on `k` are retracted with it (and their disjunctions
//! decided afresh). Truncation (`search.rs`) still does it where retraction would have to
//! undo a merge or an NI choice made since `k` (ADR-0011's rollback backend, step 3 of the
//! note).

use super::depset::DepSetId;
use super::engine::{Engine, Step};
use super::graph::{NONE, Undo, flag};

impl Engine<'_> {
    /// For a clash with `dep` whose culprit `k` is below the top: `k` retracted with the
    /// frames above that depend on it (through their premise, or through the clashes that
    /// failed their earlier alternatives, Ginsberg's eliminating explanations), and `k`'s
    /// disjunction's next alternative taken on top; with none left, the clash of its
    /// disjunction returned (to go further back, retracting again). The other retracted
    /// frames' disjunctions are open again, all their alternatives with them. `None`:
    /// truncation must do it.
    pub(super) fn retract_culprit(&mut self, dep: DepSetId, k: u32) -> Option<Step<()>> {
        let at = self.frame_of(k)?;
        let frame = &self.frames[at];
        if self.ni.pending.len() as u32 > frame.ni_pending
            || self.g.trail[frame.mark.trail as usize..]
                .iter()
                .any(|u| matches!(u, Undo::Representative { .. }))
        {
            return None;
        }
        if self.restart_point.is_none() {
            let first = self.frames.partition_point(|f| f.id <= self.floor);
            self.restart_point = self.frames.get(first).cloned();
        }
        // The closure: the frames above whose premise or failures name a retracted level.
        let mut levels = vec![k];
        let mut dependent = Vec::new();
        for (i, f) in self.frames.iter().enumerate().skip(at + 1) {
            let names = |d: DepSetId| levels.iter().any(|&l| self.deps.contains(d, l));
            if names(f.premise) || names(f.failed) {
                levels.push(f.id);
                dependent.push(i);
            }
        }
        let exhausted = frame.next + 1 >= frame.alternatives.len();
        let (mark, pending, pending_open) = (frame.mark, frame.pending, frame.pending_open);
        for &i in dependent.iter().rev() {
            self.frames.remove(i);
        }
        let mut frame = self.frames.remove(at);
        self.retract(&levels, mark, pending, pending_open);
        self.last_retraction = self.next_level;
        let mut rest = dep;
        for &l in &levels {
            rest = self.deps.without(rest, l);
        }
        frame.failed = self.deps.union(frame.failed, rest);
        if frame.clause != NONE {
            let (clause, head) = (frame.clause as usize, frame.heads[frame.next]);
            if let Some(count) = self
                .failures
                .get_mut(clause)
                .and_then(|c| c.get_mut(head as usize))
            {
                *count = count.saturating_add(1);
            }
        }
        self.stats.retractions += levels.len() as u64;
        if self.config.check_retraction {
            for &l in &levels {
                self.check_retracted(l);
            }
        }
        if exhausted {
            // Every alternative failed: the disjunction's own clash, without `k`.
            let failed = self.deps.union(frame.premise, frame.failed);
            return Some(Err(super::engine::Stop::Clash(failed)));
        }
        frame.next += 1;
        // On top, as the most recent decision: a fresh level, and the state as it is now
        // for a later truncation to it.
        let point = self.checkpoint();
        frame.id = self.next_level;
        self.next_level += 1;
        frame.mark = point.mark;
        frame.pending = point.pending;
        frame.bindings = point.bindings;
        frame.pending_open = point.pending_open;
        frame.ni_pending = point.ni_pending;
        frame.ni_open = point.ni_open;
        self.frames.push(frame);
        Some(self.take_alternative())
    }

    /// Retracts `levels` (the lowest's frame opened at `mark`, with `pending` disjunctions
    /// queued and the scan at `pending_open`): what since `mark` depends on one of them
    /// dies, and what that leaves to redo is queued.
    fn retract(
        &mut self,
        levels: &[u32],
        mark: super::graph::Mark,
        pending: u32,
        pending_open: u32,
    ) {
        let mut touched: Vec<u32> = Vec::new();
        let deps = &self.deps;
        let has = |d: DepSetId| levels.iter().any(|&l| deps.contains(d, l));
        let g = &mut self.g;
        for i in mark.unary..g.unary.len() as u32 {
            if !g.dead.unary[i as usize] && has(g.unary[i as usize].dep) {
                touched.push(g.unary[i as usize].node);
                g.kill_unary(i);
            }
        }
        for i in mark.negatives..g.negatives.len() as u32 {
            if !g.dead.negatives[i as usize] && has(g.negatives[i as usize].dep) {
                touched.push(g.negatives[i as usize].node);
                g.kill_negative(i);
            }
        }
        for i in mark.edges..g.edges.len() as u32 {
            let e = g.edges[i as usize];
            if !g.dead.edges[i as usize] && has(e.dep) {
                touched.extend([e.from, e.to]);
                g.kill_edge(i);
            }
        }
        for i in mark.numbers..g.numbers.len() as u32 {
            if !g.dead.numbers[i as usize] && has(g.numbers[i as usize].dep) {
                touched.push(g.numbers[i as usize].node);
                g.kill_number(i);
            }
        }
        for i in mark.inequalities..g.inequalities.len() as u32 {
            let f = g.inequalities[i as usize];
            if !g.dead.inequalities[i as usize] && has(f.dep) {
                touched.extend([f.a, f.b]);
                g.kill_inequality(i);
            }
        }
        for i in mark.equalities..g.equalities.len() as u32 {
            if !g.equality_dead(i) && has(g.equalities[i as usize].dep) {
                g.kill_equality(i);
            }
        }
        // A node born on a retracted level dies, and its subtree with it: a successor's
        // existence depends on its parent's, though facts there made by clauses that apply
        // to every node depend on nothing (parents come before their children).
        for n in mark.nodes..g.nodes.len() as u32 {
            let parent = g.nodes[n as usize].parent;
            let orphan = parent != NONE && g.nodes[parent as usize].flags & flag::RETRACTED != 0;
            if g.nodes[n as usize].flags & flag::RETRACTED == 0
                && (orphan || has(g.born[n as usize]))
            {
                g.kill_node(n);
                if parent != NONE {
                    touched.push(parent);
                }
            }
        }
        for i in pending as usize..self.pending.len() {
            if !self.pending_dead[i] && has(self.pending[i].dep) {
                self.pending_dead[i] = true;
            }
        }
        touched.sort_unstable();
        touched.dedup();
        // A disjunction a retracted fact satisfied may be open again: only one queued since
        // `k`'s frame was opened, already passed by the scan, and binding a touched node.
        for i in pending_open..self.pending_open.min(self.pending.len() as u32) {
            let p = self.pending[i as usize];
            if self.pending_dead[i as usize] {
                continue;
            }
            let vars = self.p.clauses[p.clause as usize].vars as usize;
            let bind = &self.bindings[p.bind as usize..p.bind as usize + vars];
            if bind.iter().any(|n| touched.binary_search(n).is_ok()) {
                self.reopen.push(i);
            }
        }
        touched.retain(|&n| self.g.live(n));
        // Conclusions a retracted fact made redundant are made again: every clause
        // instance has a premise at its centre, the node or a neighbour of it.
        let mut refire = touched.clone();
        for &n in &touched {
            refire.extend(self.g.out_edges(n).map(|(_, e)| e.to));
            refire.extend(self.g.in_edges(n).map(|(_, e)| e.from));
        }
        refire.sort_unstable();
        refire.dedup();
        self.refire.extend(refire);
        // The ≥-rule and blocking look at the touched nodes again (a list's contents
        // changed under the same head: the caches keyed by heads are stale).
        self.blocking.expand.extend(touched.iter().copied());
        if let Some(&low) = touched.first() {
            self.g.full_from = self.g.full_from.min(low);
        }
        self.g.generation = self.g.generation.wrapping_add(1);
    }

    /// The invariants after retracting `k` (`Config::check_retraction`): nothing alive
    /// still depends on `k`, and blocking redoes every node a retraction touched.
    fn check_retracted(&self, k: u32) {
        let g = &self.g;
        let deps = &self.deps;
        let live = |dead: &[bool], i: usize| !dead[i];
        for (i, f) in g.unary.iter().enumerate() {
            assert!(
                !live(&g.dead.unary, i) || !g.live(f.node) || !deps.contains(f.dep, k),
                "a fact on level {k} survived its retraction"
            );
        }
        for (i, e) in g.edges.iter().enumerate() {
            assert!(
                !live(&g.dead.edges, i) || !deps.contains(e.dep, k),
                "an edge on level {k} survived its retraction"
            );
        }
        for (i, f) in g.inequalities.iter().enumerate() {
            assert!(
                !live(&g.dead.inequalities, i) || !deps.contains(f.dep, k),
                "an inequality on level {k} survived its retraction"
            );
        }
        for (i, p) in self.pending.iter().enumerate() {
            assert!(
                self.pending_dead[i] || !deps.contains(p.dep, k),
                "a pending disjunction on level {k} survived its retraction"
            );
        }
        assert!(
            self.frames
                .iter()
                .all(|f| !deps.contains(f.premise, k) && !deps.contains(f.failed, k)),
            "a frame depends on retracted level {k}"
        );
        if let Some(&low) = self.g.touched.iter().min() {
            assert!(
                self.g.full_from <= low || self.g.touched.contains(&low),
                "a touched node escapes the blocking pass"
            );
        }
    }

    /// Joins again the live facts of the nodes a retraction queued (`refire`).
    pub(super) fn refire_one(&mut self) -> Option<Step<()>> {
        let n = self.refire.pop()?;
        if !self.g.live(n) {
            return Some(Ok(()));
        }
        let (facts, edges) = self.g.live_ids(n);
        for i in facts {
            super::hyper::join_concept(
                self.p,
                &self.g,
                &mut self.deps,
                i,
                &mut self.firings,
                &mut self.stats.plans_tried,
            );
            if let Err(stop) = self.apply_firings() {
                return Some(Err(stop));
            }
        }
        for i in edges {
            super::hyper::join_edge(
                self.p,
                &self.g,
                &mut self.deps,
                i,
                &mut self.firings,
                &mut self.stats.plans_tried,
            );
            if let Err(stop) = self.apply_firings() {
                return Some(Err(stop));
            }
        }
        Some(Ok(()))
    }
}
