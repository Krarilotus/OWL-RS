//! The search (docs/design/owl2-dl.md §6, "search"): one step is saturation, then the
//! ≥-rule, then at-most merges, then a disjunctive choice; a clash backtracks.
//!
//! - **Branch points** save the tables' lengths; a backtrack cuts back to them and undoes
//!   the trail. Nothing is copied.
//! - **Dependency-directed backjumping:** a clash jumps to the newest branch point its
//!   dependency set holds; the levels in between are skipped. Switched off, every clash
//!   goes to the newest branch point (chronological backtracking).
//! - **Semantic branching:** after an alternative failed, the later ones are taken with
//!   its negation, by what its clash depended on besides this branch point.
//! - **Boolean constraint propagation:** a disjunct whose negation holds is dropped; a
//!   disjunction with one disjunct left is asserted without a branch point.

use std::time::Instant;

use super::depset::DepSetId;
use super::engine::{Engine, Lit, Step, Stop, proof};
use super::graph::{Annot, NONE};

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// A clash-free graph no rule applies to.
    Model,
    /// Every branch clashed.
    Refuted,
    GaveUp(String),
}

impl Engine<'_> {
    /// Roots for the individuals and the assertions, at level 0.
    pub fn init(&mut self, test: Option<u32>) -> Step<()> {
        for i in 0..self.p.individuals.len() as u32 {
            let n = self.new_node(NONE, i)?;
            self.roots.push(n);
        }
        if let Some(concept) = test {
            let n = self.new_node(NONE, NONE)?;
            self.assert(Lit::Concept(concept, n), DepSetId::EMPTY, proof::ASSERTED)?;
        } else if self.p.individuals.is_empty() {
            // A model has at least one element (the calculus's non-empty ABox).
            self.new_node(NONE, NONE)?;
        }
        let a = &self.p.assertions;
        for &(c, i) in &a.concepts {
            let lit = Lit::Concept(c, self.roots[i as usize]);
            self.assert(lit, DepSetId::EMPTY, proof::ASSERTED)?;
        }
        for &(r, i, j) in &a.roles {
            let lit = Lit::Role(r, self.roots[i as usize], self.roots[j as usize]);
            self.assert(lit, DepSetId::EMPTY, proof::ASSERTED)?;
        }
        for &(i, j) in &a.different {
            let (x, y) = (self.roots[i as usize], self.roots[j as usize]);
            self.add_inequality(x, y, DepSetId::EMPTY, proof::ASSERTED)?;
        }
        for &(i, j) in &a.same {
            let lit = Lit::Equal(self.roots[i as usize], self.roots[j as usize], Annot::NONE);
            self.assert(lit, DepSetId::EMPTY, proof::ASSERTED)?;
        }
        Ok(())
    }

    /// Runs to a model, a refutation or a stop.
    pub fn run(&mut self, test: Option<u32>) -> End {
        if let Err(stop) = self.init(test) {
            return match stop {
                Stop::Clash(_) => End::Refuted,
                Stop::GaveUp(why) => End::GaveUp(why),
            };
        }
        loop {
            let stop = match self.step() {
                Ok(true) => continue,
                Ok(false) => return End::Model,
                Err(stop) => stop,
            };
            match stop {
                Stop::GaveUp(why) => return End::GaveUp(why),
                Stop::Clash(dep) => match self.backtrack(dep) {
                    Ok(true) => {}
                    Ok(false) => return End::Refuted,
                    Err(why) => return End::GaveUp(why),
                },
            }
        }
    }

    /// One round; `false` when nothing is left to do.
    fn step(&mut self) -> Step<bool> {
        self.check_time()?;
        self.check_memory()?;
        self.saturate()?;
        if self.apply_ni()? {
            return Ok(true);
        }
        if !self.config.disjunctions_first && self.expand_at_least()? {
            return Ok(true);
        }
        if self.at_most()? {
            return Ok(true);
        }
        let started = Instant::now();
        let chose = self.choose();
        self.stats.search += started.elapsed();
        if chose? {
            return Ok(true);
        }
        if self.config.disjunctions_first {
            return self.expand_at_least();
        }
        Ok(false)
    }

    /// The first open disjunction: asserted if one disjunct is left, else a branch point.
    fn choose(&mut self) -> Step<bool> {
        let mut at = self.pending_open as usize;
        while at < self.pending.len() {
            let pend = self.pending[at];
            at += 1;
            let clause = &self.p.clauses[pend.clause as usize];
            let bind =
                &self.bindings[pend.bind as usize..pend.bind as usize + clause.vars as usize];
            if bind.iter().any(|&n| !self.g.live(n)) {
                // A merged node's facts were copied, and fire again at the target.
                self.pending_open = at as u32;
                continue;
            }
            let bind: Vec<u32> = bind.to_vec();
            let mut dep = pend.dep;
            let annotation = clause.annotation;
            let mut open = Vec::new();
            let mut satisfied = false;
            for &h in &clause.head {
                let lit = self.lit(h, &bind, annotation);
                match self.holds(lit) {
                    Ok(true) => {
                        satisfied = true;
                        break;
                    }
                    Ok(false) => open.push(lit),
                    Err(refuted) => dep = self.deps.union(dep, refuted),
                }
            }
            if satisfied {
                self.pending_open = at as u32;
                continue;
            }
            return match open.len() {
                0 => Err(self.clash(dep, DepSetId::EMPTY)),
                1 => self.assert(open[0], dep, clause.source).map(|()| true),
                _ => self.branch(open, dep).map(|()| true),
            };
        }
        Ok(false)
    }

    /// Takes the next alternative of the newest branch point.
    pub fn take_alternative(&mut self) -> Step<()> {
        let level = self.frames.len() as u32;
        let Some(frame) = self.frames.last() else {
            return Ok(());
        };
        let lit = frame.alternatives[frame.next];
        let (premise, failed) = (frame.premise, frame.failed);
        let tried: Vec<Lit> = frame.alternatives[..frame.next].to_vec();
        let semantic = frame.semantic;
        let single = self.deps.single(level);
        let dep = self.deps.union(premise, single);
        self.assert(lit, dep, proof::CHOICE)?;
        if self.config.semantic_branching && semantic {
            for t in tried {
                self.negate(t, failed)?;
            }
        }
        Ok(())
    }

    /// Backtracks from a clash with `dep`; `false` if no branch point is left to try.
    fn backtrack(&mut self, mut dep: DepSetId) -> Result<bool, String> {
        let started = Instant::now();
        let out = loop {
            let top = self.frames.len() as u32;
            if top == 0 {
                break Ok(false);
            }
            let k = if self.config.backjumping {
                match self.deps.max(dep) {
                    Some(k) => k.min(top),
                    None => break Ok(false),
                }
            } else {
                top
            };
            if k < top {
                self.stats.backjumps += 1;
                self.stats.levels_skipped += u64::from(top - k);
            }
            self.frames.truncate(k as usize);
            let rest = if self.config.backjumping {
                self.deps.without(dep, k)
            } else {
                self.deps.prefix(k - 1)
            };
            let Some(frame) = self.frames.last_mut() else {
                break Ok(false);
            };
            let failed = self.deps.union(frame.failed, rest);
            frame.failed = failed;
            frame.next += 1;
            let frame = frame.clone();
            self.restore(&frame);
            if frame.next < frame.alternatives.len() {
                match self.take_alternative() {
                    Ok(()) => break Ok(true),
                    Err(Stop::Clash(d)) => {
                        dep = d;
                        continue;
                    }
                    Err(Stop::GaveUp(why)) => break Err(why),
                }
            }
            // Every alternative failed: the clash is the disjunction's.
            dep = if self.config.backjumping {
                self.deps.union(frame.premise, frame.failed)
            } else {
                self.deps.prefix(k - 1)
            };
            self.frames.pop();
        };
        self.stats.search += started.elapsed();
        out
    }

    /// Cuts everything back to the state the branch point was opened in.
    fn restore(&mut self, frame: &super::engine::Frame) {
        self.g.cut(&frame.mark);
        self.pending.truncate(frame.pending as usize);
        self.bindings.truncate(frame.bindings as usize);
        self.pending_open = frame.pending_open;
        self.ni.pending.truncate(frame.ni_pending as usize);
        self.ni.open = frame.ni_open;
        // The queues were empty when the branch point was opened.
        self.done.nodes = frame.mark.nodes;
        self.done.unary = frame.mark.unary;
        self.done.edges = frame.mark.edges;
        self.done.equalities = frame.mark.equalities;
    }
}
