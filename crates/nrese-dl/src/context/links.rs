//! The rules over links between contexts (Bate et al., Table 2): Pred, run at the
//! predecessor on the clauses its successors send, and Succ, which makes the successors.

use super::atoms::{Atom, CTerm, FuncId, is_subset, union_into};
use super::engine::Strategy;
use super::rules::{Edge, Message, Scratch, Worker};
use super::state::{ClauseId, ClauseRef, ContextId, Rule};

const NONE: u32 = u32::MAX;

impl Worker<'_> {
    // Pred -------------------------------------------------------------------------------

    /// Sends clause `c` to the predecessor `u` of the edge `u →f` here.
    pub(super) fn send_pred(&mut self, c: ClauseId, u: ContextId, f: FuncId) {
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
        let from = ClauseRef {
            context: self.out.me,
            clause: c,
        };
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

    /// Pred with a new clause `c` about a successor (`head` has `f(x)`): the successors'
    /// clauses waiting for it.
    pub(super) fn pred_local(&mut self, c: ClauseId, head: Atom) {
        let Some(waiting) = self.state.remote_by_atom.get(&head) else {
            return;
        };
        let mut s = std::mem::take(&mut self.scratch);
        s.waiting.clear();
        s.waiting.extend_from_slice(waiting);
        s.found.clear();
        let waiting = std::mem::take(&mut s.waiting);
        for &r in &waiting {
            let at = self.state.remote[r as usize]
                .body
                .iter()
                .position(|&a| a == head)
                .unwrap_or(0);
            s.premises.clear();
            self.pred_join(r, Some((at, c)), 0, &[], &mut s);
        }
        s.waiting = waiting;
        self.conclude(&s.found);
        self.scratch = s;
    }

    /// Joins remote clause `r`'s body from position `i` on with this context's clauses.
    pub(super) fn pred_join(
        &self,
        r: u32,
        fixed: Option<(usize, ClauseId)>,
        i: usize,
        acc: &[Atom],
        s: &mut Scratch,
    ) {
        if self.engine.exhausted() {
            return;
        }
        let found = s.found.len();
        if self.engine.budget().max_join.is_some_and(|m| found >= m)
            || (found % 4096 == 4095 && self.engine.out_of_time())
        {
            self.engine.exhaust();
            return;
        }
        let remote = &self.state.remote[r as usize];
        if i == remote.body.len() {
            s.refs.clear();
            if self.engine.proofs {
                let me = self.out.me;
                s.refs.extend(s.premises.iter().map(|&clause| ClauseRef {
                    context: me,
                    clause,
                }));
                s.refs.push(remote.from);
                // The edge's justification (condition S2): the successor's core holds of
                // f(x) here, by these clauses.
                for &a in self.engine.core(remote.from.context) {
                    if let Some(c) = a
                        .up(remote.func)
                        .and_then(|h| self.state.clauses.unconditional(h))
                    {
                        s.refs.push(ClauseRef {
                            context: me,
                            clause: c,
                        });
                    }
                }
            }
            s.found.push(acc, remote.head, Rule::Pred, NONE, &s.refs);
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
            s.premises.push(p);
            self.pred_join(r, fixed, i + 1, next, s);
            s.premises.pop();
        }
    }

    // Succ -------------------------------------------------------------------------------

    pub(super) fn succ(&mut self, c: ClauseId, head: Atom) {
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
