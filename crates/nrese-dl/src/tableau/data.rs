//! The hypertableau's datatype stage (docs/design/owl2-dl.md §6, "queues by kind":
//! after deterministic saturation, before the ≥-rule): data values are concrete nodes,
//! leaves of the completion graph, with data ranges as their concepts (`¬` ones in the
//! negative table, from semantic branching), inequalities between them and merges by
//! equality. The theory (`crate::datatypes`) checks each component of concrete nodes
//! connected by inequalities whose facts changed since its last check; a clash backtracks
//! with the dependency sets of the facts it used.
//!
//! Concrete nodes are never blocked nor blockers, no clause applies at them as a centre,
//! and the NI rule leaves them alone: they stand for data values, not individuals.
//!
//! **The choice for at-most data restrictions:** `≤ n p.R` counts the `p`-values in `R`;
//! a value with neither `R` nor its complement as a fact is first put into one of them
//! (a choice), so that the count is over decided values (HermiT's way for qualified
//! number restrictions on data).

use std::time::Instant;

use hashbrown::HashSet;

use super::depset::DepSetId;
use super::engine::{Engine, Lit, Step, Stop};
use super::graph::{NONE, flag};
use super::program::{ConceptName, Filler, Number};
use crate::datatypes::{DataVar, Verdict};

/// How far the datatype stage has read the fact tables.
#[derive(Debug, Clone, Copy, Default)]
pub struct DataDone {
    pub unary: u32,
    pub negatives: u32,
    pub inequalities: u32,
}

impl Engine<'_> {
    pub fn concrete(&self, node: u32) -> bool {
        self.g.nodes[node as usize].flags & flag::CONCRETE != 0
    }

    fn range_of(&self, concept: u32) -> Option<nrese_owl::RangeId> {
        match self.p.concepts[concept as usize] {
            ConceptName::Range(r) => Some(r),
            _ => None,
        }
    }

    /// Checks the components of concrete nodes whose facts changed since the last check.
    pub fn check_data(&mut self) -> Step<()> {
        if self.p.data.is_none() {
            return Ok(());
        }
        let started = Instant::now();
        let out = self.check_data_inner();
        self.stats.datatypes += started.elapsed();
        out
    }

    fn check_data_inner(&mut self) -> Step<()> {
        let mut dirty: Vec<u32> = Vec::new();
        let done = self.data_done;
        for f in &self.g.unary[done.unary as usize..] {
            if self.range_of(f.concept).is_some() {
                dirty.push(f.node);
            }
        }
        for f in &self.g.negatives[done.negatives as usize..] {
            if self.range_of(f.concept).is_some() {
                dirty.push(f.node);
            }
        }
        for i in &self.g.inequalities[done.inequalities as usize..] {
            dirty.extend([i.a, i.b]);
        }
        self.data_done = DataDone {
            unary: self.g.unary.len() as u32,
            negatives: self.g.negatives.len() as u32,
            inequalities: self.g.inequalities.len() as u32,
        };
        dirty.retain(|&n| self.g.live(n) && self.concrete(n));
        if dirty.is_empty() {
            return Ok(());
        }
        dirty.sort_unstable();
        dirty.dedup();
        let mut seen: HashSet<u32> = HashSet::new();
        for start in dirty {
            if !seen.insert(start) {
                continue;
            }
            // The component: concrete nodes connected by inequalities.
            let mut component = vec![start];
            let mut at = 0;
            while at < component.len() {
                let n = component[at];
                at += 1;
                let others: Vec<u32> = self
                    .g
                    .inequality_facts(n)
                    .map(|i| if i.a == n { i.b } else { i.a })
                    .filter(|&o| self.g.live(o))
                    .collect();
                for o in others {
                    if seen.insert(o) {
                        component.push(o);
                    }
                }
            }
            self.check_component(&component)?;
        }
        Ok(())
    }

    fn check_component(&mut self, nodes: &[u32]) -> Step<()> {
        self.stats.data_checks += 1;
        let mut theory = std::mem::take(&mut self.theory);
        theory.clear();
        let vars: Vec<DataVar> = nodes.iter().map(|_| theory.var()).collect();
        let index: hashbrown::HashMap<u32, usize> =
            nodes.iter().enumerate().map(|(i, &n)| (n, i)).collect();
        let var = |n: u32| index.get(&n).map(|&i| vars[i]);
        for (i, &n) in nodes.iter().enumerate() {
            for f in self.g.labels(n) {
                if let Some(r) = self.range_of(f.concept) {
                    theory.add_range(vars[i], r, true, f.dep);
                }
            }
            for f in self.g.negative_facts(n) {
                if let Some(r) = self.range_of(f.concept) {
                    theory.add_range(vars[i], r, false, f.dep);
                }
            }
            for e in self.g.inequality_facts(n) {
                let other = if e.a == n { e.b } else { e.a };
                // Each inequality once, from its lower end.
                if other > n
                    && let Some(o) = var(other)
                {
                    theory.add_not_equal(vars[i], o, e.dep);
                }
            }
        }
        let ranges = &self.p.data.as_ref().expect("a program with data").ranges;
        let verdict = theory.check(&vars, ranges, &mut self.deps);
        self.theory = theory;
        match verdict {
            Verdict::Sat { approximate } => {
                if let Some(why) = approximate {
                    self.data_approximate.get_or_insert(why);
                }
                Ok(())
            }
            Verdict::Clash(dep) => {
                self.stats.clashes += 1;
                Err(Stop::Clash(dep))
            }
        }
    }

    /// The choice for an at-most data restriction at `s`: a `p`-value with neither its
    /// range nor the complement decided gets one of them; whether one was made.
    pub fn data_choice(&mut self, s: u32, number: &Number, premise: DepSetId) -> Step<bool> {
        let Filler::Is(c) = number.filler else {
            return Ok(false);
        };
        let Some(&not_c) = self.p.data.as_ref().and_then(|d| d.complement.get(&c)) else {
            return Ok(false);
        };
        let open: Option<(u32, DepSetId)> = self
            .g
            .out_edges(s)
            .filter(|(_, e)| e.role == number.role.role && self.g.live(e.to))
            .map(|(_, e)| (e.to, e.dep))
            .find(|&(v, _)| {
                self.g.concept(v, c).is_none()
                    && self.g.negative(v, c).is_none()
                    && self.g.concept(v, not_c).is_none()
            });
        let Some((v, d)) = open else {
            return Ok(false);
        };
        let premise = self.deps.union(premise, d);
        self.branch(vec![Lit::Concept(c, v), Lit::Concept(not_c, v)], premise)?;
        Ok(true)
    }

    /// The literal nodes: a concrete root per literal of the data assertions.
    pub fn literal_nodes(&mut self) -> Step<Vec<u32>> {
        let Some(data) = self.p.data.as_ref() else {
            return Ok(Vec::new());
        };
        let concepts: Vec<u32> = data.literals.clone();
        let mut out = Vec::with_capacity(concepts.len());
        for c in concepts {
            let n = self.new_node(NONE, NONE)?;
            self.g.nodes[n as usize].flags |= flag::CONCRETE;
            self.assert(
                Lit::Concept(c, n),
                DepSetId::EMPTY,
                super::engine::proof::ASSERTED,
            )?;
            out.push(n);
        }
        Ok(out)
    }
}
