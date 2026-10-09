//! A context's clauses (Bate et al., Definition 6): the arena they live in, the indexes the
//! rules find premises by, the agenda, and redundancy elimination (Definition 4, the Elim
//! rule, and Sequoia's forward and backward checks of §5.2.1).
//!
//! Clauses are never removed from the arena, only marked dead, so that a derivation can
//! always name its premises (proofs) and an agenda entry can be skipped cheaply.

use hashbrown::HashMap;

use super::atoms::{Atom, CTerm, Kind, RoleId, is_subset, signature};

/// A context, by its index in the arena of contexts.
pub type ContextId = u32;
/// A clause of a context, by its index in the context's arena.
pub type ClauseId = u32;
/// A body, interned per context; `0` is the empty body.
pub type BodyId = u32;

/// A clause anywhere: its context and its index there (the facts of proofs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClauseRef {
    pub context: ContextId,
    pub clause: ClauseId,
}

/// The rule that derived a clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    Core,
    Hyper,
    Pred,
    /// `A → A` for a possible atom of a successor.
    Succ,
    /// A clause about a neighbour copied onto one merged with it (`dl`: the merge, an
    /// index into the context's merges).
    Eq,
}

/// How a clause was derived: the first derivation, recorded when proofs are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Derivation {
    pub rule: Rule,
    /// The DL-clause (Hyper), or `u32::MAX`.
    pub dl: u32,
    /// Premises: `premises[start..start + len]` of the context.
    pub start: u32,
    pub len: u32,
}

/// A clause: body, head (`Atom::BOTTOM` for `⊥`), and its state.
#[derive(Debug, Clone, Copy)]
pub struct ClauseRec {
    pub body: BodyId,
    pub head: Atom,
    pub live: bool,
    /// Taken from the agenda and run through the rules: only processed clauses are
    /// premises of Hyper and Pred (the given-clause loop), so each inference happens
    /// once, when its last premise is processed.
    pub processed: bool,
    /// The body's [`signature`].
    pub sig: u64,
    /// A copy along an unconditional merge by the Eq rule, which is not copied on (the
    /// unconditional merges are closed).
    pub copy: bool,
}

/// Bodies of one context, interned: sorted, without repeats.
#[derive(Debug, Default)]
pub struct Bodies {
    atoms: Vec<Atom>,
    spans: Vec<(u32, u32)>,
    ids: HashMap<Box<[Atom]>, BodyId>,
    keys_bytes: usize,
}

impl Bodies {
    pub fn get(&self, id: BodyId) -> &[Atom] {
        if id == 0 {
            return &[];
        }
        let (start, len) = self.spans[id as usize - 1];
        &self.atoms[start as usize..(start + len) as usize]
    }

    pub fn intern(&mut self, body: &[Atom]) -> BodyId {
        if body.is_empty() {
            return 0;
        }
        if let Some(&id) = self.ids.get(body) {
            return id;
        }
        self.spans
            .push((self.atoms.len() as u32, body.len() as u32));
        self.atoms.extend_from_slice(body);
        let id = self.spans.len() as BodyId;
        self.ids.insert(body.into(), id);
        self.keys_bytes += std::mem::size_of_val(body);
        id
    }

    fn bytes(&self) -> usize {
        use super::memory::{map, vec};
        vec(&self.atoms) + vec(&self.spans) + map(&self.ids) + self.keys_bytes
    }
}

/// What a context counted, summed into the profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Conclusions the rules produced, redundant ones included.
    pub generated: u64,
    /// ... kept as clauses.
    pub kept: u64,
    /// ... dropped by forward redundancy (a stronger clause was there).
    pub forward: u64,
    /// Clauses removed by backward redundancy (a new clause was stronger).
    pub backward: u64,
    pub hyper: u64,
    pub pred: u64,
    pub messages: u64,
    pub edges: u64,
    pub peak_agenda: u64,
    pub max_body: u64,
    /// DL-clause body slots Hyper tried (the index's hits).
    pub slots: u64,
    /// Premise lists looked up by the joins.
    pub lookups: u64,
    /// Inclusion tests of the redundancy checks: forward, one set-trie query per head
    /// looked up; backward, each candidate past its signature.
    pub subset_checks: u64,
    /// The Eq rule's merges, and its copies.
    pub merges: u64,
    pub eq: u64,
}

/// A context's clauses and indexes.
#[derive(Debug, Default)]
pub struct Clauses {
    pub(super) memory: super::memory::Charge,
    /// Capacity of nested index buffers, updated only for the head being changed.
    index_bytes: usize,
    pub recs: Vec<ClauseRec>,
    pub bodies: Bodies,
    /// Live clauses by head literal (the paper's `hyperIndex` and `predHeadIndex`, which
    /// coincide for Horn clauses); `⊥` under [`Atom::BOTTOM`].
    pub heads: HashMap<Atom, Vec<ClauseId>>,
    /// The terms `t` of heads `S(x, t)` and `S(t, x)` per role (`x` in both for
    /// `S(x, x)`), for body atoms whose neighbour isn't bound yet.
    pub out_terms: HashMap<RoleId, Vec<CTerm>>,
    pub in_terms: HashMap<RoleId, Vec<CTerm>>,
    /// The concepts `B` with a head `B(x)` (live or not), for Hyper's keyed role slots.
    pub concepts: Vec<u32>,
    /// The concepts `B` with a processed clause `… → B(x)`: Hyper skips a DL-clause whose
    /// other concept body atoms aren't all here, with a probe each (as the EL classifier's
    /// conjunction rule tests its subsumer set). It only grows: a clause removed as
    /// redundant leaves a stronger one with the same head.
    pub present: hashbrown::HashSet<u32>,
    /// To process: clauses with an empty body first (Sequoia's §5.2.2), then the rest.
    pub agenda: Vec<ClauseId>,
    pub agenda_conditional: Vec<ClauseId>,
    /// The forward redundancy index: per head, its clauses' bodies in a set-trie (the
    /// subset query walks only the paths the body has), and the heads that hold
    /// unconditionally. Dead clauses stay in it and are skipped.
    pub tries: HashMap<Atom, super::settrie::SetTrie>,
    /// The backward redundancy index: clauses with a non-empty body by `(head, atom of
    /// the body)`. A body that contains a new clause's body contains each of its atoms,
    /// so the rarest atom's list holds every candidate. Dead clauses are dropped from a
    /// list when it is walked.
    pub occurrences: HashMap<(Atom, Atom), Vec<ClauseId>>,
    /// Per head: entries of `heads` whose clause died since the list was compacted.
    pub dead: HashMap<Atom, u32>,
    pub unconditional_heads: hashbrown::HashSet<Atom>,
    /// `⊤ → ⊥` is here: every other clause is redundant.
    pub unsat: bool,
    pub derivations: Vec<Derivation>,
    pub premises: Vec<ClauseRef>,
    pub counters: Counters,
}

impl Clauses {
    fn flat_bytes(&self) -> usize {
        use super::memory::{map, set, vec};
        self.bodies.bytes()
            + vec(&self.recs)
            + vec(&self.concepts)
            + vec(&self.agenda)
            + vec(&self.agenda_conditional)
            + vec(&self.derivations)
            + vec(&self.premises)
            + map(&self.heads)
            + map(&self.out_terms)
            + map(&self.in_terms)
            + map(&self.tries)
            + map(&self.occurrences)
            + map(&self.dead)
            + set(&self.present)
            + set(&self.unconditional_heads)
    }

    /// O(1), apart from the changed head's body in `derive`: never walk all indexes
    /// just to ask whether a task has memory left.
    pub(super) fn check_memory(&mut self) -> bool {
        if !self.memory.enabled() {
            return true;
        }
        self.memory.set(self.flat_bytes() + self.index_bytes)
    }

    fn head_bytes(&self, body: &[Atom], head: Atom) -> usize {
        use super::memory::vec;
        self.heads.get(&head).map_or(0, vec)
            + self
                .tries
                .get(&head)
                .map_or(0, super::settrie::SetTrie::bytes)
            + body
                .iter()
                .map(|&a| self.occurrences.get(&(head, a)).map_or(0, vec))
                .sum::<usize>()
            + if head.is_bottom() {
                0
            } else {
                self.out_terms.get(&head.pred()).map_or(0, vec)
                    + self.in_terms.get(&head.pred()).map_or(0, vec)
            }
    }

    fn index_bytes(&self) -> usize {
        use super::memory::vec;
        self.heads.values().map(vec).sum::<usize>()
            + self
                .tries
                .values()
                .map(super::settrie::SetTrie::bytes)
                .sum::<usize>()
            + self.occurrences.values().map(vec).sum::<usize>()
            + self.out_terms.values().map(vec).sum::<usize>()
            + self.in_terms.values().map(vec).sum::<usize>()
    }

    pub fn body(&self, c: ClauseId) -> &[Atom] {
        self.bodies.get(self.recs[c as usize].body)
    }

    /// Live clauses with head `head` that may be premises: processed, or `trigger`.
    pub fn premises_for(
        &self,
        head: Atom,
        trigger: ClauseId,
    ) -> impl Iterator<Item = ClauseId> + '_ {
        self.heads
            .get(&head)
            .into_iter()
            .flatten()
            .copied()
            .filter(move |&c| {
                let rec = &self.recs[c as usize];
                rec.live && (rec.processed || c == trigger)
            })
    }

    /// Whether a live clause makes `body → head` redundant (Definition 4, case 2).
    pub(super) fn subsumed(&self, body: &[Atom], head: Atom) -> bool {
        let stronger = |key: Atom| {
            if self.unconditional_heads.contains(&key) {
                return true;
            }
            self.tries
                .get(&key)
                .is_some_and(|trie| trie.has_subset(body, &|c| self.recs[c as usize].live))
        };
        stronger(Atom::BOTTOM) || (!head.is_bottom() && stronger(head))
    }

    /// Adds `body → head` unless it is redundant (the paper's Derive, Algorithm 3);
    /// removes the clauses it makes redundant with the same head. `derivation` and
    /// `premises` are recorded when `proofs` is set. The new clause, if kept.
    pub fn derive(
        &mut self,
        body: &[Atom],
        head: Atom,
        derivation: (Rule, u32, &[ClauseRef]),
        proofs: bool,
    ) -> Option<ClauseId> {
        self.counters.generated += 1;
        self.counters.subset_checks += if head.is_bottom() { 1 } else { 2 };
        if self.unsat || self.subsumed(body, head) {
            self.counters.forward += 1;
            return None;
        }
        let before = self.memory.enabled().then(|| self.head_bytes(body, head));
        // Backward: weaker clauses with the same head (for ⊥ only when the context is
        // contradictory: then everything goes; a scan for every weaker clause of any head
        // costs more than the inferences it saves).
        let sig = signature(body);
        let removed = if body.is_empty() {
            // Every clause with this head is weaker.
            let mut n = 0u32;
            if let Some(list) = self.heads.get_mut(&head) {
                for &c in list.iter() {
                    let rec = &mut self.recs[c as usize];
                    if rec.live {
                        rec.live = false;
                        n += 1;
                    }
                }
                list.clear();
            }
            self.dead.remove(&head);
            n
        } else {
            self.remove_weaker(body, sig, head)
        };
        self.counters.backward += u64::from(removed);
        let id = self.recs.len() as ClauseId;
        let body_id = self.bodies.intern(body);
        self.recs.push(ClauseRec {
            body: body_id,
            head,
            live: true,
            processed: false,
            sig,
            copy: false,
        });
        for &a in body {
            self.occurrences.entry((head, a)).or_default().push(id);
        }
        self.counters.kept += 1;
        self.counters.max_body = self.counters.max_body.max(body.len() as u64);
        if proofs {
            let (rule, dl, premises) = derivation;
            self.derivations.push(Derivation {
                rule,
                dl,
                start: self.premises.len() as u32,
                len: premises.len() as u32,
            });
            self.premises.extend_from_slice(premises);
        }
        // A head seen for the first time: its role term, if any, is new too.
        if !head.is_bottom() && !self.heads.contains_key(&head) {
            self.index_role(head);
        }
        self.heads.entry(head).or_default().push(id);
        if body.is_empty() {
            self.unconditional_heads.insert(head);
        } else {
            self.tries.entry(head).or_default().insert(body, id);
        }
        if head.is_bottom() && body.is_empty() {
            self.unsat = true;
            self.agenda.clear();
            self.agenda_conditional.clear();
            for rec in &mut self.recs {
                rec.live = false;
            }
            self.recs[id as usize].live = true;
            self.heads.clear();
            self.heads.insert(Atom::BOTTOM, vec![id]);
            self.tries.clear();
            self.occurrences.clear();
            self.dead.clear();
            self.unconditional_heads.clear();
            self.unconditional_heads.insert(Atom::BOTTOM);
        }
        if body.is_empty() {
            self.agenda.push(id);
        } else {
            self.agenda_conditional.push(id);
        }
        let waiting = (self.agenda.len() + self.agenda_conditional.len()) as u64;
        self.counters.peak_agenda = self.counters.peak_agenda.max(waiting);
        if let Some(before) = before {
            self.index_bytes = if self.unsat {
                self.index_bytes()
            } else {
                self.index_bytes + self.head_bytes(body, head) - before
            };
            self.check_memory();
        }
        Some(id)
    }

    /// Marks dead the clauses with `head` whose body contains `body` (non-empty, with
    /// signature `sig`); how many. The head's list drops them once half of it is dead.
    fn remove_weaker(&mut self, body: &[Atom], sig: u64, head: Atom) -> u32 {
        let Some(&rarest) = body
            .iter()
            .min_by_key(|&&a| self.occurrences.get(&(head, a)).map_or(0, Vec::len))
        else {
            return 0;
        };
        let Some(list) = self.occurrences.get_mut(&(head, rarest)) else {
            return 0;
        };
        let (recs, bodies) = (&mut self.recs, &self.bodies);
        let mut removed = 0u32;
        let mut checks = 0u64;
        list.retain(|&c| {
            let rec = &mut recs[c as usize];
            if !rec.live {
                return false;
            }
            if sig & !rec.sig != 0 {
                return true;
            }
            checks += 1;
            if is_subset(body, bodies.get(rec.body)) {
                rec.live = false;
                removed += 1;
                return false;
            }
            true
        });
        self.counters.subset_checks += checks;
        if removed > 0 {
            let dead = self.dead.entry(head).or_default();
            *dead += removed;
            let len = self.heads.get(&head).map_or(0, Vec::len) as u32;
            if *dead * 2 > len {
                *dead = 0;
                if let Some(list) = self.heads.get_mut(&head) {
                    let recs = &self.recs;
                    list.retain(|&c| recs[c as usize].live);
                }
            }
        }
        removed
    }

    fn index_role(&mut self, head: Atom) {
        let (r, t) = (head.pred(), head.term());
        match head.kind() {
            Kind::Concept => {
                if t == CTerm::X {
                    self.concepts.push(r);
                }
            }
            Kind::Out => {
                let terms = self.out_terms.entry(r).or_default();
                if !terms.contains(&t) {
                    terms.push(t);
                }
                if t == CTerm::X {
                    let terms = self.in_terms.entry(r).or_default();
                    if !terms.contains(&t) {
                        terms.push(t);
                    }
                }
            }
            Kind::In => {
                let terms = self.in_terms.entry(r).or_default();
                if !terms.contains(&t) {
                    terms.push(t);
                }
            }
        }
    }

    /// The next clause to process.
    pub fn next_given(&mut self) -> Option<ClauseId> {
        while let Some(c) = self.agenda.pop().or_else(|| self.agenda_conditional.pop()) {
            if self.recs[c as usize].live {
                return Some(c);
            }
        }
        None
    }

    /// The concepts `B` with `⊤ → B(x)` here: the subsumers of the core.
    pub fn subsumers(&self) -> impl Iterator<Item = u32> + '_ {
        self.heads.iter().filter_map(|(&atom, list)| {
            let unconditional = list
                .iter()
                .any(|&c| self.recs[c as usize].live && self.recs[c as usize].body == 0);
            (unconditional
                && !atom.is_bottom()
                && atom.kind() == Kind::Concept
                && atom.term() == CTerm::X)
                .then(|| atom.pred())
        })
    }

    /// The roles `S` with `⊤ → S(x, x)` here.
    pub fn self_loops(&self) -> impl Iterator<Item = RoleId> + '_ {
        self.heads.iter().filter_map(|(&atom, list)| {
            let unconditional = list
                .iter()
                .any(|&c| self.recs[c as usize].live && self.recs[c as usize].body == 0);
            (unconditional && atom == Atom::out(atom.pred(), CTerm::X)).then(|| atom.pred())
        })
    }

    /// The live unconditional clause with head `head`, if any.
    pub fn unconditional(&self, head: Atom) -> Option<ClauseId> {
        self.heads.get(&head)?.iter().copied().find(|&c| {
            let rec = &self.recs[c as usize];
            rec.live && rec.body == 0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derive(c: &mut Clauses, body: &[Atom], head: Atom) -> Option<ClauseId> {
        c.derive(body, head, (Rule::Hyper, 0, &[]), false)
    }

    #[test]
    fn incremental_capacity_matches_a_full_index_walk() {
        let task = super::super::memory::Task::new(usize::MAX);
        let mut c = Clauses {
            memory: super::super::memory::Charge::new(Some(&task)),
            ..Clauses::default()
        };
        for i in 0..500 {
            let body = [
                Atom::concept(i % 31, CTerm::X),
                Atom::into(100 + i % 17, CTerm::Y),
            ];
            let head = if i % 3 == 0 {
                Atom::out(i % 23, CTerm::func(i % 7))
            } else {
                Atom::concept(100 + i % 47, CTerm::X)
            };
            c.derive(
                &body,
                head,
                (
                    Rule::Hyper,
                    i,
                    &[ClauseRef {
                        context: 0,
                        clause: 0,
                    }],
                ),
                true,
            );
            assert_eq!(task.used(), c.flat_bytes() + c.index_bytes());
            if i % 7 == 0 {
                derive(&mut c, &[], head);
                assert_eq!(task.used(), c.flat_bytes() + c.index_bytes());
            }
        }
        derive(&mut c, &[], Atom::BOTTOM);
        assert_eq!(task.used(), c.flat_bytes() + c.index_bytes());
        drop(c);
        assert_eq!(task.used(), 0);
    }

    #[test]
    fn redundancy_forward_and_backward() {
        let mut c = Clauses::default();
        let (a, b, s) = (
            Atom::concept(1, CTerm::X),
            Atom::concept(2, CTerm::X),
            Atom::into(3, CTerm::Y),
        );
        let weak = derive(&mut c, &[a, s], b).expect("new");
        assert_eq!(derive(&mut c, &[a, s], b), None, "the same clause again");
        let strong = derive(&mut c, &[s], b).expect("stronger");
        assert!(!c.recs[weak as usize].live, "the weaker one goes");
        assert_eq!(derive(&mut c, &[a, s], b), None);
        derive(&mut c, &[s], Atom::BOTTOM).expect("⊥ under s");
        assert_eq!(
            derive(&mut c, &[s], a),
            None,
            "⊥ under a subset of the body"
        );
        assert!(c.recs[strong as usize].live);
        derive(&mut c, &[], Atom::BOTTOM).expect("⊤ → ⊥");
        assert!(c.unsat && !c.recs[strong as usize].live);
        assert_eq!(derive(&mut c, &[], a), None);
        assert_eq!(c.next_given(), Some(c.recs.len() as ClauseId - 1));
    }

    #[test]
    fn subsumers_are_the_unconditional_concepts_at_x() {
        let mut c = Clauses::default();
        derive(&mut c, &[], Atom::concept(1, CTerm::X));
        derive(
            &mut c,
            &[Atom::into(3, CTerm::Y)],
            Atom::concept(2, CTerm::X),
        );
        derive(&mut c, &[], Atom::concept(4, CTerm::Y));
        derive(&mut c, &[], Atom::out(3, CTerm::func(0)));
        let mut s: Vec<u32> = c.subsumers().collect();
        s.sort_unstable();
        assert_eq!(s, vec![1]);
        assert_eq!(c.out_terms[&3], vec![CTerm::func(0)]);
    }
}
