//! The number rules (JAIR 2009, Table 5):
//! - **≥-rule:** an at-least restriction `≥ n R.F(s)` at a node that isn't blocked, and
//!   not already met by `n` pairwise unequal `R`-neighbours in `F` (each a successor of
//!   `s` or not blocked), gets `n` fresh successors with `F`, pairwise unequal;
//! - **≤ (at-most atoms):** `≤ n R.F(s)` with more than `n` neighbours in `F` is the
//!   Hyp-rule on the clause `n + 1` successors would spell out: over the first `n + 1`
//!   neighbours, a choice of two to merge, or a clash if all are unequal.

use std::time::Instant;

use super::BRANCH_BUDGET;
use super::depset::DepSetId;
use super::engine::{Engine, Frame, Lit, Step, Stop, proof};
use super::graph::{Annot, Edge, NONE, flag};
use super::program::{Filler, Number};

impl Engine<'_> {
    /// The live neighbours of `s` along the number's role whose filler holds, each with
    /// the dependencies of its edge and filler.
    fn neighbours(&mut self, s: u32, number: &Number) -> Vec<(u32, DepSetId)> {
        let along = |e: &Edge| -> Option<u32> {
            if e.role != number.role.role {
                return None;
            }
            Some(if number.role.inverse { e.from } else { e.to })
        };
        let edges: Vec<(u32, DepSetId)> = if number.role.inverse {
            self.g
                .in_edges(s)
                .filter_map(|(_, e)| along(e).map(|n| (n, e.dep)))
                .collect()
        } else {
            self.g
                .out_edges(s)
                .filter_map(|(_, e)| along(e).map(|n| (n, e.dep)))
                .collect()
        };
        let mut out: Vec<(u32, DepSetId)> = Vec::new();
        for (n, d) in edges {
            if !self.g.live(n) || out.iter().any(|&(m, _)| m == n) {
                continue;
            }
            let filler = match number.filler {
                Filler::Top => Some(DepSetId::EMPTY),
                Filler::Is(c) => self.g.concept(n, c).map(|i| self.g.unary[i as usize].dep),
                Filler::Not(c) => self
                    .g
                    .negative(n, c)
                    .map(|i| self.g.negatives[i as usize].dep),
            };
            if let Some(f) = filler {
                let d = self.deps.union(d, f);
                out.push((n, d));
            }
        }
        out.sort_unstable_by_key(|&(n, _)| n);
        out
    }

    /// Why merging `a` and `b` would clash at once (the dependencies of the facts that
    /// clash), if it would: a concept of one and its negation at the other, or two
    /// concepts no node may have together ([`super::program::Program::disjoint`]). Such
    /// a merge is no alternative of the ≤-rule: trying it would only fail.
    fn unmergeable(&mut self, a: u32, b: u32) -> Option<DepSetId> {
        let la: Vec<(u32, DepSetId)> = self.g.labels(a).map(|f| (f.concept, f.dep)).collect();
        let lb: Vec<(u32, DepSetId)> = self.g.labels(b).map(|f| (f.concept, f.dep)).collect();
        for &(c, dc) in &la {
            if let Some(i) = self.g.negative(b, c) {
                let d = self.g.negatives[i as usize].dep;
                return Some(self.deps.union(dc, d));
            }
        }
        for &(c, dc) in &lb {
            if let Some(i) = self.g.negative(a, c) {
                let d = self.g.negatives[i as usize].dep;
                return Some(self.deps.union(dc, d));
            }
        }
        if self.p.disjoint.is_empty() {
            return None;
        }
        for &(c, dc) in &la {
            for &(e, de) in &lb {
                if self.p.disjoint.contains(&(c, e)) {
                    return Some(self.deps.union(dc, de));
                }
            }
        }
        None
    }

    /// Whether `k` of `candidates` are pairwise unequal.
    fn distinct(&self, candidates: &[u32], k: usize, chosen: &mut Vec<u32>, from: usize) -> bool {
        if chosen.len() == k {
            return true;
        }
        for i in from..candidates.len() {
            let c = candidates[i];
            if chosen.iter().all(|&o| self.g.unequal(o, c).is_some()) {
                chosen.push(c);
                if self.distinct(candidates, k, chosen, i + 1) {
                    return true;
                }
                chosen.pop();
            }
        }
        false
    }

    /// The ≥-rule on every node that isn't blocked; whether anything was added. It looks
    /// at the nodes that changed since its last pass (the blocking pass collects them).
    pub fn expand_at_least(&mut self) -> Step<bool> {
        self.update_blocking();
        let len = self.g.nodes.len() as u32;
        let from = std::mem::replace(&mut self.blocking.expand_from, NONE).min(len);
        let mut nodes = std::mem::take(&mut self.blocking.expand);
        nodes.retain(|&n| n < from);
        nodes.sort_unstable();
        nodes.dedup();
        nodes.extend(from..len);
        let started = Instant::now();
        let out = self.expand_at_least_inner(&nodes);
        self.stats.expand += started.elapsed();
        if out.is_err() {
            // A stop leaves them unexpanded: they are looked at again after it.
            self.blocking.expand.extend(nodes);
        }
        out
    }

    fn expand_at_least_inner(&mut self, nodes: &[u32]) -> Step<bool> {
        let mut changed = false;
        let len = self.g.nodes.len() as u32;
        for &s in nodes.iter().filter(|&&s| s < len) {
            let node = &self.g.nodes[s as usize];
            if node.numbers == NONE || !node.live() || self.blocked(s) {
                continue;
            }
            let facts: Vec<(u32, DepSetId)> = self
                .g
                .number_facts(s)
                .filter(|f| !f.at_most)
                .map(|f| (f.number, f.dep))
                .collect();
            for (index, dep) in facts {
                let number = self.p.at_least[index as usize];
                let candidates: Vec<u32> = self
                    .neighbours(s, &number)
                    .into_iter()
                    .map(|(n, _)| n)
                    .filter(|&n| self.g.nodes[n as usize].parent == s || !self.blocked(n))
                    .collect();
                let met = number.n as usize <= candidates.len()
                    && (number.n <= 1
                        || self.distinct(&candidates, number.n as usize, &mut Vec::new(), 0));
                if met {
                    continue;
                }
                self.check_time()?;
                self.check_memory()?;
                // n successors are pairwise unequal: n² / 2 inequalities, each a fact and
                // an index entry. Give up before a budget can't hold them.
                let n = u64::from(number.n);
                // Saturating: a cardinality near u32::MAX overflows the product.
                let need = (n.saturating_mul(n.saturating_sub(1)) / 2)
                    .saturating_mul(48)
                    .saturating_add(n * 256);
                if (self.bytes() as u64).saturating_add(need) > self.config.max_memory as u64 {
                    return Err(Stop::Abandon(
                        dep,
                        format!(
                            "≥ {} successors with their inequalities exceed the memory budget",
                            number.n
                        ),
                    ));
                }
                let data = self.p.data_roles.get(number.role.role as usize) == Some(&true);
                let mut fresh = Vec::with_capacity(number.n as usize);
                for _ in 0..number.n {
                    let t = self.new_node(s, NONE)?;
                    self.g.born[t as usize] = dep;
                    if data {
                        // A data value: a leaf of its own kind.
                        self.g.nodes[t as usize].flags |= flag::CONCRETE;
                    }
                    let (a, b) = if number.role.inverse { (t, s) } else { (s, t) };
                    self.assert(Lit::Role(number.role.role, a, b), dep, proof::EXPANSION)?;
                    match number.filler {
                        Filler::Top => {}
                        Filler::Is(c) => self.assert(Lit::Concept(c, t), dep, proof::EXPANSION)?,
                        Filler::Not(c) => self.add_negative(t, c, dep, proof::EXPANSION)?,
                    }
                    for &u in &fresh {
                        self.add_inequality(u, t, dep, proof::EXPANSION)?;
                    }
                    fresh.push(t);
                    if fresh.len().is_multiple_of(256) {
                        self.check_time()?;
                    }
                }
                changed = true;
            }
        }
        Ok(changed)
    }

    /// The at-most atoms: whether a merge choice was made (or forced).
    pub fn at_most(&mut self) -> Step<bool> {
        if self.p.at_most.is_empty() {
            return Ok(false);
        }
        let started = Instant::now();
        let out = self.at_most_inner();
        self.stats.expand += started.elapsed();
        out
    }

    fn at_most_inner(&mut self) -> Step<bool> {
        for s in 0..self.g.nodes.len() as u32 {
            let node = &self.g.nodes[s as usize];
            if node.numbers == NONE || !node.live() || self.indirectly_blocked(s) {
                continue;
            }
            let facts: Vec<(u32, DepSetId)> = self
                .g
                .number_facts(s)
                .filter(|f| f.at_most)
                .map(|f| (f.number, f.dep))
                .collect();
            for (index, dep) in facts {
                let number = self.p.at_most[index as usize];
                if self.p.data_roles.get(number.role.role as usize) == Some(&true)
                    && self.data_choice(s, &number, dep)?
                {
                    return Ok(true);
                }
                let mut found = self.neighbours(s, &number);
                let annot = Annot {
                    root: if self.g.nodes[s as usize].flags & flag::ROOT != 0 {
                        s
                    } else {
                        NONE
                    },
                    number: self.p.at_most_annotation[index as usize],
                };
                // The clause this atom stands for binds its successors to any neighbours,
                // one node twice included: a blockable non-successor of a root then
                // raises `u ≈ u` for the NI rule.
                if annot.root != NONE {
                    let mut raised = false;
                    for &(u, d) in &found {
                        if self.needs_ni(Lit::Equal(u, u, annot)) {
                            let d = self.deps.union(dep, d);
                            raised |= self.ni_defer(u, u, d, annot);
                        }
                    }
                    if raised {
                        return Ok(true);
                    }
                }
                if found.len() <= number.n as usize {
                    continue;
                }
                found.truncate(number.n as usize + 1);
                let mut premise = dep;
                for &(_, d) in &found {
                    premise = self.deps.union(premise, d);
                }
                // A merge the NI rule has pending already satisfies the atom's clause.
                let pending = found.iter().enumerate().any(|(i, &(a, _))| {
                    found[i + 1..]
                        .iter()
                        .any(|&(b, _)| self.ni_pending(a, b, annot))
                });
                if pending {
                    continue;
                }
                let mut alternatives = Vec::new();
                for (i, &(a, _)) in found.iter().enumerate() {
                    for &(b, _) in &found[i + 1..] {
                        match self.g.unequal(a, b) {
                            Some(k) => {
                                let d = self.g.inequalities[k as usize].dep;
                                premise = self.deps.union(premise, d);
                            }
                            None if self.config.merge_filter => {
                                // A merge that would clash at once is no alternative: its
                                // clash's reasons join the premise, as an inequality's do.
                                match self.unmergeable(a, b) {
                                    Some(d) => premise = self.deps.union(premise, d),
                                    None => alternatives.push(Lit::Equal(a, b, annot)),
                                }
                            }
                            None => alternatives.push(Lit::Equal(a, b, annot)),
                        }
                    }
                }
                return match alternatives.len() {
                    0 => Err(self.clash(premise, DepSetId::EMPTY)),
                    1 => self
                        .assert(alternatives[0], premise, proof::CHOICE)
                        .map(|()| true),
                    _ => self.branch(alternatives, premise).map(|()| true),
                };
            }
        }
        Ok(false)
    }

    /// Opens a branch point over `alternatives` and takes the first.
    pub fn branch(&mut self, alternatives: Vec<Lit>, premise: DepSetId) -> Step<()> {
        self.branch_with(alternatives, premise, true)
    }

    /// Opens a branch point over the disjuncts of `clause` (`heads`: each alternative's
    /// head atom) and takes the first.
    pub fn branch_on_clause(
        &mut self,
        alternatives: Vec<Lit>,
        premise: DepSetId,
        clause: u32,
        heads: Vec<u8>,
    ) -> Step<()> {
        self.branch_with(alternatives, premise, true)?;
        if let Some(frame) = self.frames.last_mut() {
            frame.clause = clause;
            frame.heads = heads;
        }
        Ok(())
    }

    /// Opens a branch point; `semantic`: later alternatives are taken with the failed
    /// ones' negations (where semantic branching is on).
    pub fn branch_with(
        &mut self,
        alternatives: Vec<Lit>,
        premise: DepSetId,
        semantic: bool,
    ) -> Step<()> {
        if self
            .config
            .max_branch_points
            .is_some_and(|m| self.stats.branch_points >= m)
        {
            return Err(Stop::GaveUp(format!(
                "{BRANCH_BUDGET} ({})",
                self.stats.branch_points
            )));
        }
        self.stats.branch_points += 1;
        let id = self.next_level;
        self.next_level += 1;
        self.frames.push(Frame {
            id,
            mark: self.g.mark(),
            pending: self.pending.len() as u32,
            bindings: self.bindings.len() as u32,
            pending_open: self.pending_open,
            ni_pending: self.ni.pending.len() as u32,
            ni_open: self.ni.open,
            semantic,
            alternatives,
            clause: NONE,
            heads: Vec::new(),
            next: 0,
            premise,
            failed: DepSetId::EMPTY,
            reopen: self.reopen.clone(),
            refire: self.refire.clone(),
        });
        self.take_alternative()
    }
}
