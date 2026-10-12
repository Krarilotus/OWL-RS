//! The interpretation a clash-free, complete graph stands for, folded at the blocks: each
//! node that isn't blocked is an element, a directly blocked node is its blocker, and
//! indirectly blocked nodes are left out (JAIR 2009, Lemma 9's construction).
//!
//! For clauses without number restrictions this is a model of the clauses (the tests
//! check it against the ontology); with number restrictions under pairwise blocking the
//! calculus's model is the unravelling, which may be infinite, so the folded one is only
//! a candidate there.
//!
//! Beside it, [`Model::unravelled`]: the graph unravelled one step at each block. A
//! directly blocked node stays an element of its own, with its blocker's label and its
//! blocker's edges but those with the blocker's parent; so a parent keeps as many
//! successors as its graph has, where the fold merges them into the blocker (a certificate
//! for `⊤ ⊑ ≥ 2 R.A` needs that). Both are candidates; `crate::certificate` checks them.

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
    /// The graph unravelled one step at each block (the module docs), where the fold
    /// isn't that already.
    pub unravelled: Option<Box<Model>>,
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
        let unravelled = self.unravelled_model();
        if unravelled.size != m.size {
            m.unravelled = Some(Box::new(unravelled));
        }
        m
    }

    /// The graph unravelled one step at each block (the module docs; after [`Self::model`]
    /// recomputed blocking). O(nodes + edges + labels).
    fn unravelled_model(&self) -> Model {
        use super::graph::flag;
        let len = self.g.nodes.len();
        let directly = |n: usize| self.g.nodes[n].flags & flag::DIRECTLY_BLOCKED != 0;
        let mut element = vec![NONE; len];
        let mut size = 0usize;
        for (n, slot) in element.iter_mut().enumerate() {
            let node = &self.g.nodes[n];
            if node.live() && !self.indirectly_blocked(n as u32) && node.flags & flag::CONCRETE == 0
            {
                *slot = size as u32;
                size += 1;
            }
        }
        let mut m = Model {
            size,
            ..Model::default()
        };
        for (n, &e) in element.iter().enumerate() {
            if e == NONE {
                continue;
            }
            let source = if directly(n) {
                self.g.nodes[n].blocker
            } else {
                n as u32
            };
            for f in self.g.labels(source) {
                if let ConceptName::Clause(c) = self.p.concepts[f.concept as usize] {
                    m.concepts.push((c, e as usize));
                }
            }
        }
        // The blocked nodes standing in for each blocker.
        let mut copies: hashbrown::HashMap<u32, Vec<u32>> = hashbrown::HashMap::new();
        for (n, &e) in element.iter().enumerate() {
            if e != NONE && directly(n) {
                copies
                    .entry(self.g.nodes[n].blocker)
                    .or_default()
                    .push(n as u32);
            }
        }
        // An edge of a blocker is also one of each node it blocks, but those with the
        // blocker's parent: a blocked node has its own with its parent, labelled alike
        // (pairwise blocking).
        let parent = |x: u32| self.g.nodes[x as usize].parent;
        for e in &self.g.edges {
            let (from, to) = (e.from as usize, e.to as usize);
            if element[from] == NONE || element[to] == NONE {
                continue;
            }
            let role = self.p.roles[e.role as usize];
            m.roles
                .push((role, element[from] as usize, element[to] as usize));
            if let Some(ys) = copies.get(&e.from)
                && e.to != parent(e.from)
            {
                for &y in ys {
                    m.roles
                        .push((role, element[y as usize] as usize, element[to] as usize));
                }
            }
            if let Some(ys) = copies.get(&e.to)
                && e.from != parent(e.to)
            {
                for &y in ys {
                    m.roles
                        .push((role, element[from] as usize, element[y as usize] as usize));
                }
            }
        }
        for (i, &term) in self.p.individuals.iter().enumerate() {
            let root = self.root(i as u32) as usize;
            if element[root] != NONE {
                m.individuals.push((term, element[root] as usize));
            }
        }
        m.concepts.sort_unstable();
        m.concepts.dedup();
        m.roles.sort_unstable();
        m.roles.dedup();
        m
    }
}
