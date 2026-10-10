//! Hyperresolution joins over one context, using its worker-owned scratch buffers.

use super::super::atoms::{Atom, CTerm, Kind, is_subset, union_into};
use super::super::memory;
use super::super::program::{BodyPat, DlClause, HeadPat, Slot, TermPat, Var};
use super::super::state::{ClauseId, ClauseRef, Rule};
use super::{Found, Item, Worker};

impl Worker<'_> {
    // Hyper ------------------------------------------------------------------------------

    pub(super) fn hyper(&mut self, c: ClauseId, head: Atom) {
        let program = &self.engine.program;
        let mut s = std::mem::take(&mut self.scratch);
        program.slots(head, &self.state.clauses.concepts, &mut s.slots);
        if !s.check_memory() || s.slots.is_empty() {
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
            // At most one premise per body atom, including the fixed trigger. Reserve
            // once before recursion, so the worker owns and accounts this buffer there.
            s.premises
                .reserve(clause.body.len().saturating_sub(s.premises.len()));
            if !s.check_memory() {
                break;
            }
            if !unify(clause.body[pos as usize], head, &mut s.bind) {
                continue;
            }
            self.hyper_join(
                (clause, dl),
                (pos as usize, c),
                0,
                &mut s.bind,
                &s.body,
                &mut s.premises,
                &mut s.found,
            );
        }
        self.conclude(&s.found);
        s.check_memory();
        self.scratch = s;
    }

    /// Joins the body atoms of `dl` from position `i` on with the context's clauses;
    /// `fixed` is the trigger's position and clause.
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
    ) {
        let (clause, id) = dl;
        if self.engine.task_memory_exhausted() {
            return;
        }
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
                found.check_memory();
            }
            return;
        }
        if i == fixed.0 {
            return self.hyper_join(dl, fixed, i + 1, bind, acc, premises, found);
        }
        let clauses = &self.state.clauses;
        let mut each = |atom: Atom, bind: &mut [Option<CTerm>]| {
            let mut union = Vec::new();
            let mut memory = self.engine.memory_charge();
            for p in clauses.premises_for(atom, fixed.1) {
                let body = clauses.body(p);
                let next: &[Atom] = if body.is_empty() || is_subset(body, acc) {
                    acc
                } else {
                    union_into(acc, body, &mut union);
                    &union
                };
                premises.push(p);
                if !memory.set(memory::vec(&union)) {
                    return;
                }
                self.hyper_join(dl, fixed, i + 1, bind, next, premises, found);
                premises.pop();
            }
        };
        match clause.body[i] {
            BodyPat::Concept(b) => each(Atom::concept(b, CTerm::X), bind),
            BodyPat::Out(r, Var::X) => each(Atom::out(r, CTerm::X), bind),
            BodyPat::Out(r, Var::Z(z)) => match bind[z as usize] {
                Some(t) => each(Atom::out(r, t), bind),
                None => {
                    for &t in clauses.out_terms.get(&r).into_iter().flatten() {
                        bind[z as usize] = Some(t);
                        each(Atom::out(r, t), bind);
                    }
                    bind[z as usize] = None;
                }
            },
            BodyPat::In(r, z) => match bind[z as usize] {
                Some(t) => each(Atom::into(r, t), bind),
                None => {
                    for &t in clauses.in_terms.get(&r).into_iter().flatten() {
                        bind[z as usize] = Some(t);
                        each(Atom::into(r, t), bind);
                    }
                    bind[z as usize] = None;
                }
            },
        }
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
