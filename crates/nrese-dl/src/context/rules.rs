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
//! - **Eq** ([`super::equality`]) merges neighbours under at-most-one clauses.

use std::cell::Cell;

use hashbrown::HashMap;

mod hyper;
use hyper::instantiate;

use super::atoms::{Atom, CTerm, FuncId, Kind};
use super::engine::Engine;
use super::program::Slot;
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

impl Message {
    pub(super) fn heap_bytes(&self) -> usize {
        match self {
            Self::Pred { body, .. } => std::mem::size_of_val(&**body),
            _ => 0,
        }
    }
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
    /// The capacity of the remote clauses, successors and merges below their tables
    /// (kept up to date where they grow).
    pub(super) remote_bytes: usize,
    pub(super) successor_bytes: usize,
    pub(super) merge_bytes: usize,
    pub clauses: Clauses,
    pub started: bool,
    pub preds: Vec<(ContextId, FuncId)>,
    /// Processed clauses with a predecessor-trigger head: sent to every new predecessor.
    pub pr: Vec<ClauseId>,
    pub remote: Vec<Remote>,
    pub remote_by_atom: HashMap<Atom, Vec<u32>>,
    pub succ: HashMap<FuncId, Successor>,
    /// The Eq rule's merges of neighbour terms, by term and by pair (the smaller first).
    pub merges: Vec<super::equality::Merge>,
    pub merges_by_term: HashMap<CTerm, Vec<u32>>,
    pub merges_by_pair: HashMap<(CTerm, CTerm), Vec<u32>>,
    /// Why the saturation here may miss consequences, if it may: what it derived still
    /// holds, but the context is no complete answer and not exact.
    pub incomplete: Option<Incomplete>,
}

impl State {
    pub(super) fn bytes(&self) -> usize {
        use super::memory::{map, vec};
        self.clauses.bytes()
            + vec(&self.preds)
            + vec(&self.pr)
            + vec(&self.remote)
            + vec(&self.merges)
            + map(&self.remote_by_atom)
            + map(&self.succ)
            + map(&self.merges_by_term)
            + map(&self.merges_by_pair)
            + self.remote_bytes
            + self.successor_bytes
            + self.merge_bytes
    }
}

/// Why a context's saturation may miss consequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incomplete {
    /// A merge of `x` with a neighbour was due and not made ([`super::equality`]).
    Merge,
    /// A Pred join went past `Budget::max_join_steps` and was left ([`super::links`]).
    Join,
}

/// Messages for other contexts, and for this one.
#[derive(Debug)]
pub struct Out {
    pub me: ContextId,
    pub local: Vec<Message>,
    pub sends: Vec<(ContextId, Message)>,
    pub(super) payload_bytes: usize,
}

impl Out {
    pub fn new(me: ContextId) -> Self {
        Self {
            me,
            local: Vec::new(),
            sends: Vec::new(),
            payload_bytes: 0,
        }
    }

    pub(super) fn send(&mut self, to: ContextId, message: Message) {
        self.payload_bytes += message.heap_bytes();
        if to == self.me {
            self.local.push(message);
        } else {
            self.sends.push((to, message));
        }
    }

    pub(super) fn bytes(&self) -> usize {
        super::memory::vec(&self.local) + super::memory::vec(&self.sends) + self.payload_bytes
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
    fn bytes(&self) -> usize {
        use super::memory::vec;
        vec(&self.items) + vec(&self.atoms) + vec(&self.premises)
    }
    /// Conclusions so far.
    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

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
    /// The bodies Pred found in this batch, per head (they are derived after the joins,
    /// so the context's own redundancy check doesn't see them yet).
    pub(super) batch: HashMap<Atom, super::settrie::SetTrie>,
    pub(super) batch_bytes: usize,
    /// The current Pred join's steps (calls), and whether it went past its budget.
    pub(super) steps: usize,
    pub(super) left: bool,
}

impl Scratch {
    pub(super) fn bytes(&self) -> usize {
        use super::memory::{map, vec};
        self.found.bytes()
            + vec(&self.slots)
            + vec(&self.bind)
            + vec(&self.premises)
            + vec(&self.refs)
            + vec(&self.body)
            + vec(&self.waiting)
            + vec(&self.preds)
            + map(&self.batch)
            + self.batch_bytes
    }

    pub(super) fn clear_batch(&mut self) {
        self.batch.clear();
        self.batch_bytes = 0;
    }
}

/// The rules on one context, by the worker holding it.
pub struct Worker<'w> {
    pub engine: &'w Engine,
    pub state: &'w mut State,
    pub out: &'w mut Out,
    pub scratch: Scratch,
    /// The payloads of the batch's messages not yet handled (observed capacity).
    pub(super) input_bytes: usize,
    /// This batch's budget checks and capacity publications (into the counters at its end).
    checks: Cell<u64>,
    observations: Cell<u64>,
}

const NONE: u32 = u32::MAX;

impl<'w> Worker<'w> {
    /// A batch's worker; its take was the batch's first budget check.
    pub(super) fn new(
        engine: &'w Engine,
        state: &'w mut State,
        out: &'w mut Out,
        input_bytes: usize,
    ) -> Self {
        Self {
            engine,
            state,
            out,
            scratch: Scratch::default(),
            input_bytes,
            checks: Cell::new(1),
            observations: Cell::new(0),
        }
    }

    /// A work checkpoint: publishes this context's capacity, `detached` scratch included,
    /// and checks the budget.
    pub(super) fn checkpoint(&self, detached: usize) -> bool {
        self.publish(detached);
        self.checks.set(self.checks.get() + 1);
        !self.engine.out_of_budget()
    }

    /// Publishes this context's capacity where it is observed: one store, no other
    /// context's lock or shared ledger. Returns the state's part.
    fn publish(&self, detached: usize) -> usize {
        if !self.engine.observing() {
            return 0;
        }
        self.observations.set(self.observations.get() + 1);
        let state = self.state.bytes();
        let bytes = state + self.out.bytes() + self.scratch.bytes() + detached + self.input_bytes;
        self.engine.publish(self.me(), bytes);
        state
    }

    /// Ends the batch: one publication, and the batch's counts. Returns the state's
    /// capacity, which stays published once the batch's buffers are gone.
    pub(super) fn finish(self) -> usize {
        let state = self.publish(0);
        let counters = &mut self.state.clauses.counters;
        counters.batches += 1;
        counters.checks += self.checks.get();
        counters.observations += self.observations.get();
        state
    }

    fn me(&self) -> ContextId {
        self.out.me
    }

    /// Handles a message, then the agenda and the messages to itself, to a fixpoint.
    pub fn handle(&mut self, message: Message) {
        self.state.clauses.counters.messages += 1;
        self.message(message);
        let mut given = 0u32;
        loop {
            while let Some(c) = self.state.clauses.next_given() {
                self.process(c);
                given = given.wrapping_add(1);
                // One context's agenda can run for minutes (its redundancy checks grow with
                // its clauses): the budget is checked inside it too. Stopping leaves the
                // run exhausted, which ends it as `Unsupported::Budget`, never an answer.
                if given.is_multiple_of(256) && !self.checkpoint(0) {
                    return;
                }
            }
            match self.out.local.pop() {
                Some(m) => {
                    self.out.payload_bytes -= m.heap_bytes();
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
                for &a in self.engine.core(self.me()) {
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
                    let list = self.state.remote_by_atom.entry(a).or_default();
                    super::memory::push(list, id, &mut self.state.remote_bytes);
                }
                self.state.remote_bytes += std::mem::size_of_val(&*body);
                self.state.remote.push(Remote {
                    from,
                    func,
                    body,
                    head,
                });
                let mut s = std::mem::take(&mut self.scratch);
                s.found.clear();
                s.clear_batch();
                s.premises.clear();
                s.steps = 0;
                s.left = false;
                self.pred_join(id, None, 0, &[], true, &mut s);
                if s.left {
                    self.state.incomplete.get_or_insert(Incomplete::Join);
                }
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
        self.equality(c, head);
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
}
