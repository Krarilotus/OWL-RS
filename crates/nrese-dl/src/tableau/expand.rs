//! The number rules (JAIR 2009, Table 5):
//! - **≥-rule:** an at-least restriction `≥ n R.F(s)` at a node that isn't blocked, and
//!   not already met by `n` pairwise unequal `R`-neighbours in `F` (each a successor of
//!   `s` or not blocked), gets `n` fresh successors with `F`, pairwise unequal;
//! - **≤ (at-most atoms):** `≤ n R.F(s)` with more than `n` neighbours in `F` is the
//!   Hyp-rule on the clause `n + 1` successors would spell out: over the first `n + 1`
//!   neighbours, a choice of two to merge, or a clash if all are unequal.

use std::time::Instant;

use super::depset::DepSetId;
use super::engine::{Engine, Frame, Lit, Step, proof};
use super::graph::{Edge, NONE, flag};
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

    /// The ≥-rule on every node that isn't blocked; whether anything was added.
    pub fn expand_at_least(&mut self) -> Step<bool> {
        // What changes from here on lowers the floor again for the next pass.
        let floor = std::mem::replace(&mut self.g.dirty, NONE);
        self.compute_blocking(floor);
        let started = Instant::now();
        let out = self.expand_at_least_inner(floor);
        self.stats.expand += started.elapsed();
        out
    }

    fn expand_at_least_inner(&mut self, floor: u32) -> Step<bool> {
        let mut changed = false;
        let len = self.g.nodes.len() as u32;
        for s in floor.min(len)..len {
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
                let mut fresh = Vec::with_capacity(number.n as usize);
                for _ in 0..number.n {
                    let t = self.new_node(s, NONE)?;
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
                let mut found = self.neighbours(s, &number);
                if found.len() <= number.n as usize {
                    continue;
                }
                found.truncate(number.n as usize + 1);
                let mut premise = dep;
                for &(_, d) in &found {
                    premise = self.deps.union(premise, d);
                }
                let root = if self.g.nodes[s as usize].flags & flag::ROOT != 0 {
                    s
                } else {
                    NONE
                };
                let mut alternatives = Vec::new();
                for (i, &(a, _)) in found.iter().enumerate() {
                    for &(b, _) in &found[i + 1..] {
                        match self.g.unequal(a, b) {
                            Some(k) => {
                                let d = self.g.inequalities[k as usize].dep;
                                premise = self.deps.union(premise, d);
                            }
                            None => alternatives.push(Lit::Equal(a, b, root)),
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
        self.stats.branch_points += 1;
        self.frames.push(Frame {
            mark: self.g.mark(),
            pending: self.pending.len() as u32,
            bindings: self.bindings.len() as u32,
            pending_open: self.pending_open,
            alternatives,
            next: 0,
            premise,
            failed: DepSetId::EMPTY,
        });
        self.take_alternative()
    }
}
