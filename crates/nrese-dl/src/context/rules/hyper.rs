//! Hyperresolution joins over one context, using its worker-owned scratch buffers.

use super::super::atoms::{Atom, CTerm, Kind, RoleId, is_subset, union_into};
use super::super::program::{BodyPat, DlClause, HeadPat, Slot, TermPat, Var};
use super::super::state::{ClauseId, ClauseRef, Rule};
use super::{Found, Item, Worker};

impl Worker<'_> {
    // Hyper ------------------------------------------------------------------------------

    pub(super) fn hyper(&mut self, c: ClauseId, head: Atom) {
        let program = &self.engine.program;
        let mut s = std::mem::take(&mut self.scratch);
        program.slots(head, &self.state.clauses.concepts, &mut s.slots);
        if s.slots.is_empty() {
            self.scratch = s;
            return;
        }
        s.found.clear();
        s.premises.clear();
        s.premises.push(c);
        s.body.clear();
        s.body.extend_from_slice(self.state.clauses.body(c));
        self.state.clauses.counters.slots += s.slots.len() as u64;
        let present = &self.state.clauses.present;
        // What the join's checkpoints publish of the scratch, apart from what it finds.
        let detached = if self.engine.observing() {
            s.bytes() - s.found.bytes()
        } else {
            0
        };
        for at in 0..s.slots.len() {
            let Slot {
                clause: dl,
                guard,
                pos,
            } = s.slots[at];
            if guard != Slot::NO_GUARD && !present.contains(&guard) {
                continue;
            }
            let clause = &program.clauses[dl as usize];
            // The other concept atoms must be there: a probe each, before any join.
            let missing = clause.body.iter().enumerate().any(|(i, b)| {
                i != pos as usize && matches!(*b, BodyPat::Concept(k) if !present.contains(&k))
            });
            if missing {
                continue;
            }
            s.bind.clear();
            s.bind.resize(program.vars[dl as usize] as usize, None);
            if !unify(clause.body[pos as usize], head, &mut s.bind) {
                continue;
            }
            if !self.hyper_join(
                (clause, dl),
                (pos as usize, c),
                0,
                &mut s.bind,
                &s.body,
                &mut s.premises,
                &mut s.found,
                detached,
            ) {
                break;
            }
        }
        self.conclude(&s.found);
        self.scratch = s;
    }

    /// Joins the body atoms of `dl` from position `i` on with the context's clauses;
    /// `fixed` is the trigger's position and clause. False where a checkpoint found the
    /// budget spent: the join stops.
    #[expect(clippy::too_many_arguments, reason = "the join's recursion state")]
    fn hyper_join(
        &self,
        dl: (&DlClause, u32),
        fixed: (usize, ClauseId),
        i: usize,
        bind: &mut [Option<CTerm>],
        acc: &[Atom],
        premises: &mut Vec<ClauseId>,
        found: &mut Found,
        detached: usize,
    ) -> bool {
        let (clause, id) = dl;
        if i == clause.body.len() {
            if let Some(head) = instantiate(clause.head, bind) {
                let b = (found.atoms.len() as u32, acc.len() as u32);
                found.atoms.extend_from_slice(acc);
                let p = found.premises.len() as u32;
                if self.engine.proofs {
                    let me = self.me();
                    found
                        .premises
                        .extend(premises.iter().map(|&clause| ClauseRef {
                            context: me,
                            clause,
                        }));
                }
                found.items.push(Item {
                    head,
                    rule: Rule::Hyper,
                    dl: id,
                    body: b,
                    premises: (p, found.premises.len() as u32 - p),
                });
                if found.len().is_multiple_of(4096) {
                    return self.checkpoint(detached + found.bytes());
                }
            }
            return true;
        }
        if i == fixed.0 {
            return self.hyper_join(dl, fixed, i + 1, bind, acc, premises, found, detached);
        }
        let clauses = &self.state.clauses;
        let mut each = |atom: Atom, bind: &mut [Option<CTerm>]| {
            let mut union = Vec::new();
            for p in clauses.premises_for(atom, fixed.1) {
                let body = clauses.body(p);
                let next: &[Atom] = if body.is_empty() || is_subset(body, acc) {
                    acc
                } else {
                    union_into(acc, body, &mut union);
                    &union
                };
                premises.push(p);
                let go = self.hyper_join(dl, fixed, i + 1, bind, next, premises, found, detached);
                premises.pop();
                if !go {
                    return false;
                }
            }
            true
        };
        let (terms, r, z, atom): (_, _, _, fn(RoleId, CTerm) -> Atom) = match clause.body[i] {
            BodyPat::Concept(b) => return each(Atom::concept(b, CTerm::X), bind),
            BodyPat::Out(r, Var::X) => return each(Atom::out(r, CTerm::X), bind),
            BodyPat::Out(r, Var::Z(z)) => (&clauses.out_terms, r, z, Atom::out),
            BodyPat::In(r, z) => (&clauses.in_terms, r, z, Atom::into),
        };
        if let Some(t) = bind[z as usize] {
            return each(atom(r, t), bind);
        }
        let mut go = true;
        for &t in terms.get(&r).into_iter().flatten() {
            bind[z as usize] = Some(t);
            go = each(atom(r, t), bind);
            if !go {
                break;
            }
        }
        bind[z as usize] = None;
        go
    }
}

/// Whether the context atom `atom` matches the DL-clause body atom `pat` with `σ(x) = x`,
/// extending `bind`.
fn unify(pat: BodyPat, atom: Atom, bind: &mut [Option<CTerm>]) -> bool {
    let (kind, pred, t) = (atom.kind(), atom.pred(), atom.term());
    let mut set = |z: u8, t: CTerm| match bind[z as usize] {
        Some(b) => b == t,
        None => {
            bind[z as usize] = Some(t);
            true
        }
    };
    match pat {
        BodyPat::Concept(b) => atom == Atom::concept(b, CTerm::X),
        BodyPat::Out(r, Var::X) => atom == Atom::out(r, CTerm::X),
        BodyPat::Out(r, Var::Z(z)) => kind == Kind::Out && pred == r && set(z, t),
        BodyPat::In(r, z) => match kind {
            Kind::In if pred == r => set(z, t),
            Kind::Out if pred == r && t == CTerm::X => set(z, t),
            _ => false,
        },
    }
}

/// The head atom of a DL-clause under `bind` (`⊥` for no head); `None` if a variable is
/// unbound (no DL-clause has one).
pub(super) fn instantiate(head: Option<HeadPat>, bind: &[Option<CTerm>]) -> Option<Atom> {
    let Some(h) = head else {
        return Some(Atom::BOTTOM);
    };
    let t = match h.term {
        TermPat::Var(Var::X) => CTerm::X,
        TermPat::Var(Var::Z(z)) => (*bind.get(z as usize)?)?,
        TermPat::Func(f) => CTerm::func(f),
    };
    Some(Atom::of(h.kind.kind(), h.pred, t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        engine::{Budget, Engine, Strategy, lock},
        program::Program,
        rules::{Out, Scratch},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn hyper_join_stops_at_its_checkpoint_and_observes_detached_scratch() {
        // 66 independent premises for each atom yield 4356 conclusions. Cancellation
        // or memory pressure is first observed inside the join, after 4096 conclusions.
        for stop in 0..3 {
            let flag = Arc::new(AtomicBool::new(false));
            let budget = Budget {
                task_memory: (stop == 2).then_some(4 << 20),
                ..Budget::default()
            };
            let engine = Engine::new(Program::default(), Strategy::Cautious, false, budget)
                .with_cancel(Some(crate::tableau::Cancel::from_flag(flag.clone())));
            let (root, _) = engine.context_for(&[]);
            let mut state = lock(&engine.context(root).state);
            for head in 0..2 {
                for i in 0..66 {
                    let id = state
                        .clauses
                        .derive(
                            &[Atom::concept(10 + head * 66 + i, CTerm::X)],
                            Atom::concept(head, CTerm::X),
                            (Rule::Core, 0, &[]),
                            false,
                        )
                        .unwrap();
                    state.clauses.recs[id as usize].processed = true;
                }
            }
            let clause = DlClause {
                body: vec![BodyPat::Concept(0), BodyPat::Concept(1)].into_boxed_slice(),
                head: None,
                sources: Box::default(),
            };
            let mut out = Out::new(root);
            let worker = Worker::new(&engine, &mut state, &mut out, 0);
            let mut scratch = Scratch::default();
            scratch.waiting.reserve(2 << 20);
            let detached = scratch.bytes() - scratch.found.bytes();
            if stop == 1 {
                flag.store(true, Ordering::Release);
            }
            let finished = worker.hyper_join(
                (&clause, 0),
                (usize::MAX, u32::MAX),
                0,
                &mut [],
                &[],
                &mut scratch.premises,
                &mut scratch.found,
                detached,
            );
            assert_eq!(finished, stop == 0);
            assert_eq!(scratch.found.len(), if stop == 0 { 4356 } else { 4096 });
            assert_eq!(engine.exhausted(), stop != 0);
            if stop == 2 {
                assert!(engine.task_memory_bytes().1 >= detached);
            }
        }
    }
}
