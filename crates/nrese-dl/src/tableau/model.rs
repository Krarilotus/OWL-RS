//! The interpretation a clash-free, complete graph stands for, folded at the blocks: each
//! node that isn't blocked is an element, a directly blocked node is its blocker, and
//! indirectly blocked nodes are left out (JAIR 2009, Lemma 9's construction).
//!
//! For clauses without number restrictions this is a model of the clauses (the tests
//! check it against the ontology); with number restrictions under pairwise blocking the
//! calculus's model is the unravelling, which may be infinite, so the folded one is only
//! a candidate there.

use nrese_owl::{Concept, Term};

use super::engine::Engine;
use super::graph::NONE;
use super::program::ConceptName;

/// A finite interpretation over the elements `0..size`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Model {
    pub size: usize,
    /// `(concept, element)`: named classes and the normalisation's fresh names.
    pub concepts: Vec<(Concept, usize)>,
    /// `(property, from, to)`.
    pub roles: Vec<(Term, usize, usize)>,
    pub individuals: Vec<(Term, usize)>,
}

impl Engine<'_> {
    /// The folded model of the graph as it is (call after a run ended in a model).
    pub fn model(&mut self) -> Model {
        self.recompute_blocking();
        let len = self.g.nodes.len();
        let mut element = vec![NONE; len];
        let mut size = 0usize;
        let concrete = |n: usize| self.g.nodes[n].flags & super::graph::flag::CONCRETE != 0;
        for (n, slot) in element.iter_mut().enumerate() {
            if self.g.nodes[n].live() && !self.blocked(n as u32) && !concrete(n) {
                *slot = size as u32;
                size += 1;
            }
        }
        let at = |n: u32| -> Option<usize> {
            let node = &self.g.nodes[n as usize];
            if !node.live()
                || self.indirectly_blocked(n)
                || node.flags & super::graph::flag::CONCRETE != 0
            {
                return None;
            }
            let e = if self.blocked(n) {
                element[node.blocker as usize]
            } else {
                element[n as usize]
            };
            (e != NONE).then_some(e as usize)
        };
        let mut m = Model {
            size,
            ..Model::default()
        };
        for n in 0..len as u32 {
            if element[n as usize] != NONE {
                for f in self.g.labels(n) {
                    if let ConceptName::Clause(c) = self.p.concepts[f.concept as usize] {
                        m.concepts.push((c, element[n as usize] as usize));
                    }
                }
            }
        }
        for e in &self.g.edges {
            if let (Some(a), Some(b)) = (at(e.from), at(e.to)) {
                m.roles.push((self.p.roles[e.role as usize], a, b));
            }
        }
        for (i, &term) in self.p.individuals.iter().enumerate() {
            if let Some(e) = at(self.root(i as u32)) {
                m.individuals.push((term, e));
            }
        }
        m.concepts.sort_unstable();
        m.concepts.dedup();
        m.roles.sort_unstable();
        m.roles.dedup();
        m
    }
}
