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
use super::engine::{Engine, Strategy};
use super::program::{BodyPat, DlClause, HeadPat, TermPat, Var};
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

    fn send(&mut self, to: ContextId, message: Message) {
        if to == self.me {
            self.local.push(message);
        } else {
            self.sends.push((to, message));
        }
    }
}

/// A conclusion waiting to be derived (the joins only read the state).
struct Pending {
    body: Vec<Atom>,
    head: Atom,
    rule: Rule,
    dl: u32,
    premises: Vec<ClauseRef>,
}

/// The rules on one context, by the worker holding it.
pub struct Worker<'w> {
    pub engine: &'w Engine,
    pub state: &'w mut State,
    pub out: &'w mut Out,
}

const NONE: u32 = u32::MAX;

impl Worker<'_> {
    fn me(&self) -> ContextId {
        self.out.me
    }

    fn local(&self, clause: ClauseId) -> ClauseRef {
        ClauseRef {
            context: self.me(),
            clause,
        }
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
                let mut found = Vec::new();
                self.pred_join(id, None, 0, &[], &mut Vec::new(), &mut found);
                self.conclude(found);
            }
        }
    }

    /// The given-clause step: the clause becomes a premise, and every rule runs on it.
    fn process(&mut self, c: ClauseId) {
        self.state.clauses.recs[c as usize].processed = true;
        let head = self.state.clauses.recs[c as usize].head;
        if !head.is_bottom() {
            self.hyper(c, head);
        }
        if self.engine.program.is_pr(head) {
            self.state.pr.push(c);
            let preds = self.state.preds.clone();
            for (u, f) in preds {
                self.send_pred(c, u, f);
            }
        }
        if head.func().is_some() {
            if let Some(waiting) = self.state.remote_by_atom.get(&head) {
                let waiting = waiting.clone();
                let mut found = Vec::new();
                for r in waiting {
                    let at = self.state.remote[r as usize]
                        .body
                        .iter()
                        .position(|&a| a == head)
                        .unwrap_or(0);
                    self.pred_join(r, Some((at, c)), 0, &[], &mut Vec::new(), &mut found);
                }
                self.conclude(found);
            }
            self.succ(c, head);
        }
    }

    fn conclude(&mut self, found: Vec<Pending>) {
        let proofs = self.engine.proofs;
        for p in found {
            let counters = &mut self.state.clauses.counters;
            match p.rule {
                Rule::Pred => counters.pred += 1,
                _ => counters.hyper += 1,
            }
            self.state
                .clauses
                .derive(&p.body, p.head, (p.rule, p.dl, &p.premises), proofs);
        }
    }

    // Hyper ------------------------------------------------------------------------------

    fn hyper(&mut self, c: ClauseId, head: Atom) {
        let program = &self.engine.program;
        let (first, second) = program.slots(head);
        if first.is_empty() && second.is_empty() {
            return;
        }
        let mut found = Vec::new();
        let mut bind = Vec::new();
        let mut premises = vec![c];
        let body = self.state.clauses.body(c).to_vec();
        for &(dl, pos) in first.iter().chain(second) {
            let clause = &program.clauses[dl as usize];
            bind.clear();
            bind.resize(program.vars[dl as usize] as usize, None);
            if !unify(clause.body[pos as usize], head, &mut bind) {
                continue;
            }
            self.hyper_join(
                (clause, dl),
                (pos as usize, c),
                0,
                &mut bind,
                &body,
                &mut premises,
                &mut found,
            );
        }
        self.conclude(found);
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
        found: &mut Vec<Pending>,
    ) {
        let (clause, id) = dl;
        if i == clause.body.len() {
            if let Some(head) = instantiate(clause.head, bind) {
                found.push(Pending {
                    body: acc.to_vec(),
                    head,
                    rule: Rule::Hyper,
                    dl: id,
                    premises: self.refs(premises),
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

    fn refs(&self, premises: &[ClauseId]) -> Vec<ClauseRef> {
        if !self.engine.proofs {
            return Vec::new();
        }
        premises.iter().map(|&c| self.local(c)).collect()
    }

    // Pred -------------------------------------------------------------------------------

    /// Sends clause `c` to the predecessor `u` of the edge `u →f` here.
    fn send_pred(&mut self, c: ClauseId, u: ContextId, f: FuncId) {
        let rec = self.state.clauses.recs[c as usize];
        let Some(head) = rec.head.up(f) else {
            return;
        };
        let mut body = Vec::with_capacity(self.state.clauses.body(c).len());
        for &a in self.state.clauses.body(c) {
            match a.up(f) {
                Some(b) => body.push(b),
                // S(x, x) has no image: the clause says nothing the predecessor can use.
                None => return,
            }
        }
        body.sort_unstable();
        let from = self.local(c);
        self.out.send(
            u,
            Message::Pred {
                from,
                func: f,
                body: body.into_boxed_slice(),
                head,
            },
        );
    }

    /// Joins remote clause `r`'s body from position `i` on with this context's clauses.
    fn pred_join(
        &self,
        r: u32,
        fixed: Option<(usize, ClauseId)>,
        i: usize,
        acc: &[Atom],
        premises: &mut Vec<ClauseId>,
        found: &mut Vec<Pending>,
    ) {
        let remote = &self.state.remote[r as usize];
        if i == remote.body.len() {
            let mut refs = self.refs(premises);
            if self.engine.proofs {
                refs.push(remote.from);
                // The edge's justification (condition S2): the successor's core holds of
                // f(x) here, by these clauses.
                for &a in self.engine.core(remote.from.context) {
                    if let Some(c) = a
                        .up(remote.func)
                        .and_then(|h| self.state.clauses.unconditional(h))
                    {
                        refs.push(self.local(c));
                    }
                }
            }
            found.push(Pending {
                body: acc.to_vec(),
                head: remote.head,
                rule: Rule::Pred,
                dl: NONE,
                premises: refs,
            });
            return;
        }
        let clauses = &self.state.clauses;
        let trigger = fixed.map_or(NONE, |(_, c)| c);
        let only = fixed.filter(|&(at, _)| at == i).map(|(_, c)| c);
        let mut union = Vec::new();
        for p in clauses.premises_for(remote.body[i], trigger) {
            if only.is_some_and(|c| c != p) {
                continue;
            }
            let body = clauses.body(p);
            let next: &[Atom] = if body.is_empty() || is_subset(body, acc) {
                acc
            } else {
                union_into(acc, body, &mut union);
                &union
            };
            premises.push(p);
            self.pred_join(r, fixed, i + 1, next, premises, found);
            premises.pop();
        }
    }

    // Succ -------------------------------------------------------------------------------

    fn succ(&mut self, c: ClauseId, head: Atom) {
        let Some(f) = head.func() else {
            return;
        };
        let program = &self.engine.program;
        let unconditional = self.state.clauses.recs[c as usize].body == 0;
        let entry = self.state.succ.entry(f).or_default();
        let mut grew = false;
        if let Some(a) = head.down().filter(|&a| program.is_su(a)) {
            if !entry.k2.contains(&a) {
                entry.k2.push(a);
                grew = true;
                for e in &mut entry.edges {
                    if !e.core.contains(&a) && !e.sent.contains(&a) {
                        e.complete = false;
                    }
                }
            }
            if unconditional && !entry.k1.contains(&a) {
                entry.k1.push(a);
            }
        }
        if (!grew && !entry.edges.is_empty()) || entry.edges.iter().any(|e| e.complete) {
            return;
        }
        // No edge holds K₂: the strategy picks the successor.
        let core: Vec<Atom> = match self.engine.strategy {
            Strategy::Cautious => program.funcs[f as usize]
                .filler
                .map(|b| Atom::concept(b, CTerm::X))
                .filter(|a| entry.k1.contains(a))
                .into_iter()
                .collect(),
            Strategy::Eager => {
                let mut k1 = entry.k1.clone();
                k1.sort_unstable();
                k1
            }
        };
        let (to, created) = self.engine.context_for(&core);
        if created {
            self.out.send(to, Message::Init);
        }
        let at = match entry.edges.iter().position(|e| e.to == to) {
            Some(at) => at,
            None => {
                entry.edges.push(Edge {
                    to,
                    core: core.into_boxed_slice(),
                    sent: Vec::new(),
                    complete: false,
                });
                self.state.clauses.counters.edges += 1;
                self.out.send(
                    to,
                    Message::Link {
                        from: self.out.me,
                        func: f,
                    },
                );
                entry.edges.len() - 1
            }
        };
        let edge = &mut entry.edges[at];
        for &a in &entry.k2 {
            if !edge.core.contains(&a) && !edge.sent.contains(&a) {
                edge.sent.push(a);
                self.out.send(to, Message::Possible(a));
            }
        }
        edge.complete = true;
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
