//! The Horn rules of Bate et al. (JAIR 2018, Table 2) on one context: Core, Hyper, Pred
//! and Succ, with `⊥` as a head like any other (a `⊤ → ⊥` in a successor reaches its
//! predecessors through Pred, which is the `⊥` rule of the EL calculi).
//!
//! The rules run where their premises meet, so that a context's state is only ever
//! touched by the worker holding it (docs/design/owl2-dl.md §5, "Parallelism"):
//! - **Hyper** joins a DL-clause's body with the context's own clauses.
//! - **Pred** runs at the predecessor `u` of an edge `u →f v`: `v` sends each of its
//!   clauses whose head is a predecessor trigger, translated by `σ = {x ↦ f(x), y ↦ x}`,
//!   and `u` joins its body with its own clauses about `f(x)`, now and whenever a new one
//!   comes. A new edge replays `v`'s earlier clauses (Sequoia's `Eᵥ`, Algorithm 2).
//! - **Succ** runs at `u`: a clause about `f(x)` adds the atom to `K₂` (and `K₁` if
//!   unconditional); when no edge for `f` holds every atom of `K₂`, the expansion strategy
//!   picks the successor, which gets `A → A` for the atoms it lacks.

use hashbrown::HashMap;

use super::atoms::{Atom, CTerm, FuncId, Kind, is_subset, union_into};
use super::engine::Engine;
use super::program::{BodyPat, DlClause, HeadPat, Slot, TermPat, Var};
use super::state::{ClauseId, ClauseRef, Clauses, ContextId, Rule};

/// A message between contexts. Messages a context sends itself stay in its own batch.
#[derive(Debug)]
pub enum Message {
    /// Start: the Core rule and the DL-clauses with an empty body.
    Init,
    /// `A → A`: `A` may hold for this context's elements (Succ).
    Possible(Atom),
    /// `from →func` this context: a new predecessor edge.
    Link { from: ContextId, func: FuncId },
    /// A clause of a successor over the edge `func`, in this context's terms (Pred's
    /// main premise).
    Pred {
        from: ClauseRef,
        func: FuncId,
        body: Box<[Atom]>,
        head: Atom,
    },
}

/// A successor's clause received for Pred.
#[derive(Debug)]
pub struct Remote {
    pub from: ClauseRef,
    pub func: FuncId,
    pub body: Box<[Atom]>,
    pub head: Atom,
}

/// An edge `u →f v` as `u` keeps it for Succ.
#[derive(Debug)]
pub struct Edge {
    pub to: ContextId,
    pub core: Box<[Atom]>,
    /// The possible atoms sent to `to`.
    pub sent: Vec<Atom>,
    /// `to` holds every atom of `K₂` (Succ's condition).
    pub complete: bool,
}

/// What `u` knows about one successor function `f`.
#[derive(Debug, Default)]
pub struct Successor {
    /// Successor triggers that hold for `f(x)` unconditionally (`K₁`), and that may hold
    /// (`K₂`), in the successor's terms.
    pub k1: Vec<Atom>,
    pub k2: Vec<Atom>,
    pub edges: Vec<Edge>,
}

/// A context's state: its clauses and its links.
#[derive(Debug, Default)]
pub struct State {
    pub clauses: Clauses,
    pub started: bool,
    pub preds: Vec<(ContextId, FuncId)>,
    /// Processed clauses with a predecessor-trigger head: sent to every new predecessor.
    pub pr: Vec<ClauseId>,
    pub remote: Vec<Remote>,
    pub remote_by_atom: HashMap<Atom, Vec<u32>>,
    pub succ: HashMap<FuncId, Successor>,
}

/// Messages for other contexts, and for this one.
#[derive(Debug)]
pub struct Out {
    pub me: ContextId,
    pub local: Vec<Message>,
    pub sends: Vec<(ContextId, Message)>,
}

impl Out {
    pub fn new(me: ContextId) -> Self {
        Self {
            me,
            local: Vec::new(),
            sends: Vec::new(),
        }
    }

    pub(super) fn send(&mut self, to: ContextId, message: Message) {
        if to == self.me {
            self.local.push(message);
        } else {
            self.sends.push((to, message));
        }
    }
}

/// Conclusions waiting to be derived (the joins only read the state), flat, in buffers
/// reused across inferences: no allocation per conclusion.
#[derive(Debug, Default)]
pub struct Found {
    items: Vec<Item>,
    atoms: Vec<Atom>,
    premises: Vec<ClauseRef>,
}

#[derive(Debug, Clone, Copy)]
struct Item {
    head: Atom,
    rule: Rule,
    dl: u32,
    body: (u32, u32),
    premises: (u32, u32),
}

impl Found {
    pub(super) fn clear(&mut self) {
        self.items.clear();
        self.atoms.clear();
        self.premises.clear();
    }

    /// A conclusion: `body → head` by `rule` (and DL-clause `dl`) from `premises`.
    pub(super) fn push(
        &mut self,
        body: &[Atom],
        head: Atom,
        rule: Rule,
        dl: u32,
        premises: &[ClauseRef],
    ) {
        let b = (self.atoms.len() as u32, body.len() as u32);
        self.atoms.extend_from_slice(body);
        let p = (self.premises.len() as u32, premises.len() as u32);
        self.premises.extend_from_slice(premises);
        self.items.push(Item {
            head,
            rule,
            dl,
            body: b,
            premises: p,
        });
    }
}

/// A worker's buffers, reused across inferences.
#[derive(Debug, Default)]
pub struct Scratch {
    pub(super) slots: Vec<Slot>,
    pub(super) found: Found,
    pub(super) bind: Vec<Option<CTerm>>,
    pub(super) premises: Vec<ClauseId>,
    pub(super) refs: Vec<ClauseRef>,
    pub(super) body: Vec<Atom>,
    pub(super) waiting: Vec<u32>,
    pub(super) preds: Vec<(ContextId, FuncId)>,
}

/// The rules on one context, by the worker holding it.
pub struct Worker<'w> {
    pub engine: &'w Engine,
    pub state: &'w mut State,
    pub out: &'w mut Out,
    pub scratch: Scratch,
}

const NONE: u32 = u32::MAX;

impl Worker<'_> {
    fn me(&self) -> ContextId {
        self.out.me
    }

    /// Handles a message, then the agenda and the messages to itself, to a fixpoint.
    pub fn handle(&mut self, message: Message) {
        self.state.clauses.counters.messages += 1;
        self.message(message);
        loop {
            while let Some(c) = self.state.clauses.next_given() {
                self.process(c);
            }
            match self.out.local.pop() {
                Some(m) => {
                    self.state.clauses.counters.messages += 1;
                    self.message(m);
                }
                None => break,
            }
        }
    }

    fn message(&mut self, message: Message) {
        let proofs = self.engine.proofs;
        match message {
            Message::Init => {
                if std::mem::replace(&mut self.state.started, true) {
                    return;
                }
                let core = self.engine.core(self.me()).to_vec();
                for a in core {
                    self.state
                        .clauses
                        .derive(&[], a, (Rule::Core, NONE, &[]), proofs);
                }
                let program = &self.engine.program;
                for &dl in &program.facts {
                    let head = instantiate(program.clauses[dl as usize].head, &[]);
                    if let Some(head) = head {
                        self.state
                            .clauses
                            .derive(&[], head, (Rule::Hyper, dl, &[]), proofs);
                    }
                }
            }
            Message::Possible(a) => {
                self.state
                    .clauses
                    .derive(&[a], a, (Rule::Succ, NONE, &[]), proofs);
            }
            Message::Link { from, func } => {
                if self.state.preds.contains(&(from, func)) {
                    return;
                }
                self.state.preds.push((from, func));
                let replay: Vec<ClauseId> = self
                    .state
                    .pr
                    .iter()
                    .copied()
                    .filter(|&c| self.state.clauses.recs[c as usize].live)
                    .collect();
                for c in replay {
                    self.send_pred(c, from, func);
                }
            }
            Message::Pred {
                from,
                func,
                body,
                head,
            } => {
                if self.state.clauses.unsat {
                    return;
                }
                let id = self.state.remote.len() as u32;
                for &a in body.iter() {
                    self.state.remote_by_atom.entry(a).or_default().push(id);
                }
                self.state.remote.push(Remote {
                    from,
                    func,
                    body,
                    head,
                });
                let mut s = std::mem::take(&mut self.scratch);
                s.found.clear();
                s.premises.clear();
                self.pred_join(id, None, 0, &[], &mut s);
                self.conclude(&s.found);
                self.scratch = s;
            }
        }
    }

    /// The given-clause step: the clause becomes a premise, and every rule runs on it.
    fn process(&mut self, c: ClauseId) {
        self.state.clauses.recs[c as usize].processed = true;
        let head = self.state.clauses.recs[c as usize].head;
        if !head.is_bottom() && head.kind() == Kind::Concept && head.term() == CTerm::X {
            self.state.clauses.present.insert(head.pred());
        }
        if !head.is_bottom() {
            self.hyper(c, head);
        }
        if self.engine.program.is_pr(head) {
            self.state.pr.push(c);
            let mut s = std::mem::take(&mut self.scratch);
            s.preds.clear();
            s.preds.extend_from_slice(&self.state.preds);
            for &(u, f) in &s.preds {
                self.send_pred(c, u, f);
            }
            self.scratch = s;
        }
        if head.func().is_some() {
            self.pred_local(c, head);
            self.succ(c, head);
        }
    }

    pub(super) fn conclude(&mut self, found: &Found) {
        let proofs = self.engine.proofs;
        for item in &found.items {
            let counters = &mut self.state.clauses.counters;
            match item.rule {
                Rule::Pred => counters.pred += 1,
                _ => counters.hyper += 1,
            }
            let (b, n) = item.body;
            let (p, m) = item.premises;
            self.state.clauses.derive(
                &found.atoms[b as usize..(b + n) as usize],
                item.head,
                (
                    item.rule,
                    item.dl,
                    &found.premises[p as usize..(p + m) as usize],
                ),
                proofs,
            );
        }
    }

    // Hyper ------------------------------------------------------------------------------

    fn hyper(&mut self, c: ClauseId, head: Atom) {
        let program = &self.engine.program;
        let mut s = std::mem::take(&mut self.scratch);
        let Scratch {
            slots,
            found,
            bind,
            premises,
            body,
            ..
        } = &mut s;
        program.slots(head, &self.state.clauses.concepts, slots);
        if slots.is_empty() {
            self.scratch = s;
            return;
        }
        found.clear();
        premises.clear();
        premises.push(c);
        body.clear();
        body.extend_from_slice(self.state.clauses.body(c));
        self.state.clauses.counters.slots += slots.len() as u64;
        let present = &self.state.clauses.present;
        for &Slot {
            clause: dl,
            guard,
            pos,
        } in slots.iter()
        {
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
            bind.clear();
            bind.resize(program.vars[dl as usize] as usize, None);
            if !unify(clause.body[pos as usize], head, bind) {
                continue;
            }
            self.hyper_join(
                (clause, dl),
                (pos as usize, c),
                0,
                bind,
                body,
                premises,
                found,
            );
        }
        self.conclude(found);
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
            }
            return;
        }
        if i == fixed.0 {
            return self.hyper_join(dl, fixed, i + 1, bind, acc, premises, found);
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
fn instantiate(head: Option<HeadPat>, bind: &[Option<CTerm>]) -> Option<Atom> {
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
