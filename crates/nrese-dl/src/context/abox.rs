//! Assertions in the Horn stage: consistency of class and property assertions without
//! nominals.
//!
//! Each individual gets a context whose core is what is known of it, so its anonymous
//! part (successors, their successors, what they send back) is the context core's as for
//! any class. Between named individuals the only links are the asserted edges and those
//! the clauses derive; a DL-clause with a role body atom is evaluated on them directly
//! (each such clause has one neighbour, so the edge binds it), and what it derives for an
//! individual joins its core. Cores grow until nothing changes: then the ontology is
//! consistent unless some individual's context or some clause on an edge derived `⊥`.
//!
//! What a named individual knows only conditionally on a predecessor `y` doesn't apply:
//! a named individual has no anonymous predecessor, and its named neighbours are seen by
//! the clauses on the edges.

use std::collections::{BTreeSet, HashMap, HashSet};

use nrese_owl::{Normalised, Term};

use super::atoms::{Atom, CTerm, ConceptId, RoleId};
use super::compile::{Compiler, Unsupported};
use super::engine::{Engine, lock};
use super::program::{BodyPat, KindPat, TermPat, Var};
use super::state::ContextId;

/// The assertions, over dense individuals.
#[derive(Debug, Clone, Default)]
pub struct Abox {
    /// Per individual, the concepts asserted of it, sorted.
    pub types: Vec<Vec<ConceptId>>,
    /// `(role, subject, object)`.
    pub edges: Vec<(RoleId, u32, u32)>,
    /// For a DL-clause with a role body atom and a head about a successor: the concept
    /// `E` with `E(x) → head`, added to an individual where the clause fires.
    pub existential: HashMap<u32, ConceptId>,
}

impl Compiler {
    /// The assertions over individuals, `SameIndividual` merged: their concepts, the edges
    /// between them, and, for each DL-clause that a named edge can fire and whose head is
    /// about a successor, a fresh name `E` with `E(x) → head` (the individual's context
    /// then builds the successor).
    pub(super) fn abox(&mut self, n: &Normalised) -> Result<Abox, Unsupported> {
        let facts = &n.facts;
        let mut ids: HashMap<Term, usize> = HashMap::new();
        let mut parent: Vec<usize> = Vec::new();
        let mut id = |t: Term, parent: &mut Vec<usize>| {
            *ids.entry(t).or_insert_with(|| {
                parent.push(parent.len());
                parent.len() - 1
            })
        };
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        for &(a, b, _) in &facts.same {
            let (a, b) = (id(a, &mut parent), id(b, &mut parent));
            let (a, b) = (find(&mut parent, a), find(&mut parent, b));
            parent[a] = b;
        }
        let mut abox = Abox::default();
        let mut dense: HashMap<usize, u32> = HashMap::new();
        let mut individual = |t: Term, parent: &mut Vec<usize>, abox: &mut Abox| {
            let i = id(t, parent);
            let root = find(parent, i);
            *dense.entry(root).or_insert_with(|| {
                abox.types.push(Vec::new());
                abox.types.len() as u32 - 1
            })
        };
        for &(c, a, _) in &facts.concepts {
            let i = individual(a, &mut parent, &mut abox);
            let (cid, flipped) = self.concept(c);
            let cid = if flipped { self.negation(cid) } else { cid };
            abox.types[i as usize].push(cid);
        }
        for &(r, a, b, _) in &facts.roles {
            let (a, b) = (
                individual(a, &mut parent, &mut abox),
                individual(b, &mut parent, &mut abox),
            );
            let r = self.role(r);
            abox.edges.push((r, a, b));
        }
        for types in &mut abox.types {
            types.sort_unstable();
            types.dedup();
        }
        abox.edges.sort_unstable();
        abox.edges.dedup();
        if abox.edges.is_empty() {
            return Ok(abox);
        }
        // Clauses a named edge can fire: one neighbour at most, and a fresh name for heads
        // about a successor.
        let fired: Vec<usize> = (0..self.program.clauses.len())
            .filter(|&i| {
                self.program.clauses[i]
                    .body
                    .iter()
                    .any(|b| !matches!(b, BodyPat::Concept(_)))
            })
            .collect();
        for i in fired {
            let clause = self.program.clauses[i].clone();
            let two = clause
                .body
                .iter()
                .any(|b| matches!(b, BodyPat::Out(_, Var::Z(z)) | BodyPat::In(_, z) if *z > 0));
            if two {
                return Err(Unsupported::Assertions(
                    "property assertions beside a clause with two neighbours",
                ));
            }
            if let Some(h) = clause.head
                && matches!(h.term, TermPat::Func(_))
            {
                let e = self.internal();
                self.add(vec![BodyPat::Concept(e)], Some(h), &clause.sources);
                abox.existential.insert(i as u32, e);
            }
        }
        Ok(abox)
    }
}

/// The individuals' contexts and what is known of them, to a fixpoint.
pub struct Individuals {
    types: Vec<BTreeSet<ConceptId>>,
    edges: HashSet<(RoleId, u32, u32)>,
    pub contexts: Vec<ContextId>,
    pub consistent: bool,
}

impl Individuals {
    /// Saturates the individuals' contexts on `engine` and evaluates the clauses on the
    /// edges until nothing changes.
    pub fn saturate(engine: &Engine, abox: &Abox, threads: usize) -> Self {
        let mut me = Self {
            types: abox
                .types
                .iter()
                .map(|t| t.iter().copied().collect())
                .collect(),
            edges: abox.edges.iter().copied().collect(),
            contexts: Vec::new(),
            consistent: true,
        };
        loop {
            let mut seeds = Vec::new();
            me.contexts = me
                .types
                .iter()
                .map(|types| {
                    let core: Vec<Atom> =
                        types.iter().map(|&c| Atom::concept(c, CTerm::X)).collect();
                    let (id, created) = engine.context_for(&core);
                    if created {
                        seeds.push(id);
                    }
                    id
                })
                .collect();
            engine.run(&seeds, threads);
            let mut changed = false;
            for (a, &context) in me.contexts.iter().enumerate() {
                let state = lock(&engine.context(context).state);
                if state.clauses.unsat {
                    me.consistent = false;
                    return me;
                }
                for s in state.clauses.subsumers() {
                    changed |= me.types[a].insert(s);
                }
                for r in state.clauses.self_loops() {
                    changed |= me.edges.insert((r, a as u32, a as u32));
                }
            }
            match me.edges_round(engine, abox) {
                None => {
                    me.consistent = false;
                    return me;
                }
                Some(more) => changed |= more,
            }
            if !changed {
                return me;
            }
        }
    }

    /// One pass of the clauses over the edges; `None` if one derives `⊥`, else whether
    /// anything was new.
    fn edges_round(&mut self, engine: &Engine, abox: &Abox) -> Option<bool> {
        let program = &engine.program;
        let mut changed = false;
        let mut todo: Vec<(RoleId, u32, u32)> = self.edges.iter().copied().collect();
        while let Some((r, a, b)) = todo.pop() {
            let slots = program.by_out[r as usize]
                .iter()
                .map(|&s| (s, a, b))
                .chain(program.by_in[r as usize].iter().map(|&s| (s, b, a)));
            for ((clause, pos), x, z) in slots.collect::<Vec<_>>() {
                let dl = &program.clauses[clause as usize];
                // The slot binds the neighbour: S(x, z) from x's side, S(z, x) from z's.
                let fits = match dl.body[pos as usize] {
                    BodyPat::Out(_, Var::X) | BodyPat::In(..) if a == b => true,
                    BodyPat::Out(_, Var::X) => false,
                    _ => true,
                };
                if !fits || !self.body_holds(&dl.body, x, z) {
                    continue;
                }
                // A `⊥` head: the assertions have no model.
                let h = dl.head?;
                let (at, other) = match h.term {
                    TermPat::Var(Var::X) => (x, x),
                    TermPat::Var(Var::Z(_)) => (z, z),
                    TermPat::Func(_) => {
                        let e = abox.existential[&clause];
                        changed |= self.types[x as usize].insert(e);
                        continue;
                    }
                };
                match h.kind {
                    KindPat::Concept => changed |= self.types[at as usize].insert(h.pred),
                    KindPat::Out | KindPat::In => {
                        // S(x, v) or S(v, x) with v the head's term.
                        let edge = match (h.kind, at == x) {
                            (KindPat::Out, true) => (h.pred, x, x),
                            (KindPat::Out, false) => (h.pred, x, other),
                            (_, true) => (h.pred, x, x),
                            (_, false) => (h.pred, other, x),
                        };
                        if self.edges.insert(edge) {
                            changed = true;
                            todo.push(edge);
                        }
                    }
                }
            }
        }
        Some(changed)
    }

    /// Whether every body atom holds at the individual `x` with neighbour `z`.
    fn body_holds(&self, body: &[BodyPat], x: u32, z: u32) -> bool {
        body.iter().all(|b| match *b {
            BodyPat::Concept(c) => self.types[x as usize].contains(&c),
            BodyPat::Out(r, Var::X) => self.edges.contains(&(r, x, x)),
            BodyPat::Out(r, Var::Z(_)) => self.edges.contains(&(r, x, z)),
            BodyPat::In(r, _) => self.edges.contains(&(r, z, x)),
        })
    }
}
