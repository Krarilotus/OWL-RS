//! The Eq rule: equality for at-most-one clauses (functional and inverse-functional
//! properties) in the Horn stage. These are Kazakov's `≤`-rules for Horn-SHIQ (IJCAI 2009)
//! in the context core's terms, the way Konclude's saturation merges successors under a
//! functional `≤ 1` (Steigmiller's thesis, §6.3.1).
//!
//! Where an at-most-one clause `C(x) ∧ R(x, z₀) ∧ R(x, z₁) → z₀ ≈ z₁` finds two
//! R-neighbours `t₀ ≠ t₁` of `x` (successors `f(x)`, or the predecessor `y`), the two are
//! one element:
//! - every clause `Γ → A(t₀)` gives `Γ ∪ Δ → A(t₁)`, and back, with `Δ` the bodies of the
//!   premises that made them equal;
//! - a merged successor gets the other's atoms as triggers (Succ), so its context derives
//!   what the one element has;
//! - atoms copied onto `y` go to the predecessor. Pred passes them on because every `A(y)`
//!   is a predecessor trigger, and the constraints' roles are triggers both ways
//!   (`Program::index`).
//!
//! The copies stay few:
//! - unconditional merges are closed under composition and copy onto the class's least
//!   term only (`y` where it is in the class); a copy along one isn't copied on;
//! - conditional ones copy both ways, and their copies are copied on (composing two of
//!   them would multiply their bodies).
//!
//! Off by default ([`super::Options::equality`]): on ore_ont_9724 (Full-GALEN, 674
//! functional properties) a successor's merges rest on its possible atoms, and the
//! conditional copies outgrow 9 GB where the part without the at-most-one clauses
//! saturates in 25 s.
//!
//! Not merged: `x` with a neighbour (`R(x, x)` and another R-neighbour). Such a context is
//! marked incomplete, since its saturation may miss consequences. It then gives no
//! complete answer ([`super::Saturated::complete`]) and is not exact for the driver.

use super::atoms::{Atom, CTerm, Kind, union_into};
use super::rules::Worker;
use super::state::{ClauseId, ClauseRef, Rule};

/// `a ≈ b` under `body` (sorted), by an at-most-one clause from premises.
#[derive(Debug)]
pub struct Merge {
    pub a: CTerm,
    pub b: CTerm,
    pub body: Box<[Atom]>,
    /// The at-most-one clauses it rests on (indexes into `Program::at_most_one`; more
    /// than one where merges were composed).
    pub by: Box<[u32]>,
    pub premises: Box<[ClauseRef]>,
}

impl Worker<'_> {
    /// Eq for the processed clause `c` with `head`: the merges it is a premise of, and
    /// its copies onto the terms its term is merged with.
    pub(super) fn equality(&mut self, c: ClauseId, head: Atom) {
        let engine = self.engine;
        let program = &engine.program;
        if program.at_most_one.is_empty() || head.is_bottom() {
            return;
        }
        let t = head.term();
        match head.kind() {
            Kind::Concept if t == CTerm::X => {
                for &k in program
                    .at_most_one_by_guard
                    .get(&head.pred())
                    .into_iter()
                    .flatten()
                {
                    self.merges(k, c, None);
                }
            }
            Kind::Concept => {}
            kind => {
                // `S(x, x)` is an atom of both kinds.
                let both = [Kind::Out, Kind::In];
                let kinds = if t == CTerm::X {
                    &both[..]
                } else {
                    &[kind][..]
                };
                for &kind in kinds {
                    let key = (kind, head.pred());
                    for &k in program.at_most_one_by_role.get(&key).into_iter().flatten() {
                        self.merges(k, c, Some(t));
                    }
                }
            }
        }
        if t != CTerm::X {
            self.copies(c, head);
        }
    }

    /// The merges by at-most-one clause `k` that `c` is a premise of: of its term `at`
    /// with the other neighbours, or (a guard, `None`) of every pair of neighbours.
    fn merges(&mut self, k: u32, c: ClauseId, at: Option<CTerm>) {
        if self.engine.task_memory_exhausted() {
            return;
        }
        let engine = self.engine;
        let a = &engine.program.at_most_one[k as usize];
        let clauses = &self.state.clauses;
        let terms = match a.kind {
            Kind::In => clauses.in_terms.get(&a.role),
            _ => clauses.out_terms.get(&a.role),
        };
        let Some(terms) = terms else {
            return;
        };
        let others: Vec<CTerm> = terms.iter().copied().filter(|&t| t != CTerm::X).collect();
        if others.len() < terms.len() && !others.is_empty() {
            self.state
                .incomplete
                .get_or_insert(super::rules::Incomplete::Merge);
        }
        let mut pairs = Vec::new();
        match at {
            Some(CTerm::X) => return,
            Some(t) => pairs.extend(others.iter().filter(|&&u| u != t).map(|&u| (t, u))),
            None => {
                for (i, &t) in others.iter().enumerate() {
                    pairs.extend(others[i + 1..].iter().map(|&u| (t, u)));
                }
            }
        }
        let neighbour = |t: CTerm| match a.kind {
            Kind::In => Atom::into(a.role, t),
            _ => Atom::out(a.role, t),
        };
        let mut scratch_memory = engine.memory_charge();
        if !scratch_memory.set(super::memory::vec(&others) + super::memory::vec(&pairs)) {
            return;
        }
        let mut found = Vec::new();
        let mut found_memory = engine.memory_charge();
        let mut found_payload = 0;
        for (t0, t1) in pairs {
            let mut atoms = vec![neighbour(t0), neighbour(t1)];
            atoms.extend(a.guard.iter().map(|&g| Atom::concept(g, CTerm::X)));
            let head = self.state.clauses.recs[c as usize].head;
            let fixed = atoms.iter().position(|&x| x == head);
            let mut premises = Vec::with_capacity(atoms.len());
            let mut memory = engine.memory_charge();
            if !memory.set(super::memory::vec(&atoms) + super::memory::vec(&premises)) {
                return;
            }
            join(
                engine,
                &self.state.clauses,
                (&atoms, fixed, c),
                0,
                &[],
                &mut premises,
                &mut |body, premises| {
                    let body = body.to_vec();
                    let premises = premises.to_vec();
                    found_payload += super::memory::vec(&body) + super::memory::vec(&premises);
                    found.push((t0, t1, body, premises));
                    found_memory.set(super::memory::vec(&found) + found_payload);
                },
            );
        }
        let me = self.out.me;
        let found_capacity = super::memory::vec(&found);
        for (t0, t1, body, premises) in found {
            found_payload -= super::memory::vec(&body) + super::memory::vec(&premises);
            found_memory.set(found_capacity + found_payload);
            let premises = premises
                .iter()
                .map(|&clause| ClauseRef {
                    context: me,
                    clause,
                })
                .collect();
            self.merge(t0, t1, body, vec![k], premises);
        }
    }

    /// Records `t0 ≈ t1` under `body` (unless a merge with a subset of it is there),
    /// composed with the merges already there, and copies every processed clause about
    /// either term onto the other.
    fn merge(
        &mut self,
        t0: CTerm,
        t1: CTerm,
        body: Vec<Atom>,
        by: Vec<u32>,
        premises: Vec<ClauseRef>,
    ) {
        let mut payload_memory = self.engine.memory_charge();
        if !payload_memory.set(
            super::memory::vec(&body) + super::memory::vec(&by) + super::memory::vec(&premises),
        ) {
            return;
        }
        if t0 == t1 {
            return;
        }
        let key = (t0.min(t1), t0.max(t1));
        let merges = &self.state.merges;
        let subsumed = self.state.merges_by_pair.get(&key).is_some_and(|ids| {
            ids.iter().any(|&m| {
                let m = &merges[m as usize];
                m.body.iter().all(|a| body.binary_search(a).is_ok())
            })
        });
        if subsumed {
            return;
        }
        let id = self.state.merges.len() as u32;
        let earlier: Vec<u32> = [t0, t1]
            .iter()
            .flat_map(|t| {
                self.state
                    .merges_by_term
                    .get(t)
                    .into_iter()
                    .flatten()
                    .copied()
            })
            .collect();
        let mut scratch_memory = self.engine.memory_charge();
        if !scratch_memory.set(super::memory::vec(&earlier)) {
            return;
        }
        self.state.merge_bytes += std::mem::size_of_val(body.as_slice())
            + std::mem::size_of_val(by.as_slice())
            + std::mem::size_of_val(premises.as_slice());
        self.state.merges.push(Merge {
            a: t0,
            b: t1,
            body: body.into_boxed_slice(),
            by: by.into_boxed_slice(),
            premises: premises.into_boxed_slice(),
        });
        drop(payload_memory); // Persistent merge state takes over the payload charge.
        self.state.clauses.counters.merges += 1;
        for t in [t0, t1] {
            let list = self.state.merges_by_term.entry(t).or_default();
            let before = super::memory::vec(list);
            list.push(id);
            self.state.merge_bytes += super::memory::vec(list) - before;
        }
        let list = self.state.merges_by_pair.entry(key).or_default();
        let before = super::memory::vec(list);
        list.push(id);
        self.state.merge_bytes += super::memory::vec(list) - before;
        if !self.state.check_memory() {
            return;
        }
        let clauses = &self.state.clauses;
        let about: Vec<ClauseId> = clauses
            .heads
            .iter()
            .filter(|(h, _)| !h.is_bottom() && (h.term() == t0 || h.term() == t1))
            .flat_map(|(_, ids)| ids.iter().copied())
            .filter(|&c| {
                let rec = &clauses.recs[c as usize];
                rec.live && rec.processed && !rec.copy
            })
            .collect();
        if !scratch_memory.set(super::memory::vec(&earlier) + super::memory::vec(&about)) {
            return;
        }
        for c in about {
            self.copy(c, id);
        }
        scratch_memory.set(super::memory::vec(&earlier));
        // `t0 ≈ t1` and `t1 ≈ u` give `t0 ≈ u` where one of the two is unconditional (the
        // body stays the other's, so bodies never multiply): the unconditional merges stay
        // closed, and a copy along one isn't copied on. Copies along conditional merges
        // are, which carries an atom along a chain of them.
        for m in earlier {
            let (new, old) = (
                &self.state.merges[id as usize],
                &self.state.merges[m as usize],
            );
            if !new.body.is_empty() && !old.body.is_empty() {
                continue;
            }
            let (shared, end) = if old.a == t0 || old.a == t1 {
                (old.a, old.b)
            } else {
                (old.b, old.a)
            };
            let start = if shared == t0 { t1 } else { t0 };
            let mut body = Vec::new();
            union_into(&new.body, &old.body, &mut body);
            let mut by: Vec<u32> = new.by.iter().chain(old.by.iter()).copied().collect();
            by.sort_unstable();
            by.dedup();
            let premises: Vec<ClauseRef> = new
                .premises
                .iter()
                .chain(old.premises.iter())
                .copied()
                .collect();
            self.merge(start, end, body, by, premises);
        }
    }

    /// Clause `c` (about a term other than `x`, not a copy along an unconditional merge)
    /// onto the terms merged with its term.
    fn copies(&mut self, c: ClauseId, head: Atom) {
        if self.state.clauses.recs[c as usize].copy {
            return;
        }
        let Some(ids) = self.state.merges_by_term.get(&head.term()) else {
            return;
        };
        let count = ids.len();
        for at in 0..count {
            let m = self.state.merges_by_term[&head.term()][at];
            self.copy(c, m);
        }
    }

    /// Clause `c`, about one term of merge `m`, onto the other term.
    fn copy(&mut self, c: ClauseId, m: u32) {
        let head = self.state.clauses.recs[c as usize].head;
        let merge = &self.state.merges[m as usize];
        let to = if head.term() == merge.a {
            merge.b
        } else {
            merge.a
        };
        // An unconditional class needs its atoms on one term only: its least (`y` where it
        // is in it), from which they reach `x`, the predecessor and the successor's context.
        // What its other terms would derive from their part of the atoms, it derives too.
        if merge.body.is_empty() && self.least(to) != to {
            return;
        }
        let target = Atom::of(head.kind(), head.pred(), to);
        let mut body = Vec::new();
        union_into(self.state.clauses.body(c), &merge.body, &mut body);
        let mut premises = Vec::new();
        if self.engine.proofs {
            premises.push(ClauseRef {
                context: self.out.me,
                clause: c,
            });
            premises.extend_from_slice(&merge.premises);
        }
        let closed = merge.body.is_empty();
        let mut memory = self.engine.memory_charge();
        if !memory.set(super::memory::vec(&body) + super::memory::vec(&premises)) {
            return;
        }
        self.state.clauses.counters.eq += 1;
        let id =
            self.state
                .clauses
                .derive(&body, target, (Rule::Eq, m, &premises), self.engine.proofs);
        if let Some(id) = id {
            self.state.clauses.recs[id as usize].copy = closed;
        }
    }
}

impl Worker<'_> {
    /// The least term merged with `t` unconditionally (`t` itself if none is less).
    fn least(&self, t: CTerm) -> CTerm {
        let merges = &self.state.merges;
        self.state
            .merges_by_term
            .get(&t)
            .into_iter()
            .flatten()
            .map(|&m| &merges[m as usize])
            .filter(|m| m.body.is_empty())
            .map(|m| m.a.min(m.b))
            .fold(t, CTerm::min)
    }
}

/// The premises for `atoms` from position `i` on (at `fixed` only `trigger`; elsewhere
/// any live clause with that head that is processed or the trigger), with the union of
/// their bodies: each combination to `emit`.
fn join(
    engine: &super::engine::Engine,
    clauses: &super::state::Clauses,
    want: (&[Atom], Option<usize>, ClauseId),
    i: usize,
    acc: &[Atom],
    premises: &mut Vec<ClauseId>,
    emit: &mut dyn FnMut(&[Atom], &[ClauseId]),
) {
    if engine.task_memory_exhausted() {
        return;
    }
    let (atoms, fixed, trigger) = want;
    if i == atoms.len() {
        emit(acc, premises);
        return;
    }
    let mut union = Vec::new();
    let candidates: Vec<ClauseId> = if fixed == Some(i) {
        vec![trigger]
    } else {
        clauses.premises_for(atoms[i], trigger).collect()
    };
    let capacity = super::memory::vec(&candidates);
    let mut memory = engine.memory_charge();
    if !memory.set(capacity) {
        return;
    }
    for p in candidates {
        union_into(acc, clauses.body(p), &mut union);
        if !memory.set(capacity + super::memory::vec(&union)) {
            return;
        }
        premises.push(p);
        join(engine, clauses, want, i + 1, &union, premises, emit);
        premises.pop();
    }
}
