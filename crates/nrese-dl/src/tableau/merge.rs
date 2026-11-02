//! The ≈-rule (JAIR 2009, Table 5): merging one node into another. The direction is the
//! calculus's: into a named individual, into a root from a node that isn't named, into an
//! ancestor; otherwise the other way round. Of two named individuals, which the calculus
//! leaves open, the one holding fewer facts goes into the other, as far as a bounded read of
//! both tells; past it, the smaller class into the larger. The merged node's descendants
//! are pruned, its facts are copied to the target with the equality's dependencies added, and it
//! points at the target from then on. Everything changed in place goes on the trail, so
//! a backtrack undoes the merge (no path compression).
//!
//! A merge the NI rule governs (two blockable neighbours of a root, one of them not its
//! successor, raised by an at-most restriction at the root) is left to it (`ni.rs`).

use super::depset::DepSetId;
use super::engine::{Engine, Lit, Step, Stop, proof};
use super::graph::{Annot, NONE, flag};
use crate::limits::Limit;

/// The entries of each side's fact lists, retracted ones included, that the choice between two
/// named individuals reads at most (`lighter`); a side with more is weighed by its class size.
/// It bounds the choice's work per merge at about what copying a small side costs anyway; on
/// I35's key witness the choice reads 16 entries a merge, at 1,000 members as at 8,000.
const WALK: u32 = 1024;

/// What reading a side's next weighed fact found.
enum Read {
    Fact,
    End,
    Spent,
}

/// Reads `entries` to its next weighed fact, counting each entry read in `walked`, at most
/// `WALK`.
fn next_fact(entries: &mut impl Iterator<Item = bool>, walked: &mut u32) -> Read {
    loop {
        if *walked == WALK {
            return Read::Spent;
        }
        *walked += 1;
        match entries.next() {
            None => return Read::End,
            Some(true) => return Read::Fact,
            Some(false) => {}
        }
    }
}

impl Engine<'_> {
    /// Merges what `a` and `b` stand for now, by the equality's `dep` and the merges that
    /// made them stand for it (an equality waits in the queue while other merges happen).
    pub fn merge(&mut self, a: u32, b: u32, dep: DepSetId, annot: Annot) -> Step<()> {
        let (a, b, dep) = self.canonical_pair(a, b, dep);
        if a == b || !self.g.live(a) || !self.g.live(b) {
            return Ok(());
        }
        if let Some(i) = self.g.unequal(a, b) {
            let d = self.g.inequalities[i as usize].dep;
            return Err(self.clash(dep, d));
        }
        if self.needs_ni(Lit::Equal(a, b, annot)) {
            self.ni_defer(a, b, dep, annot);
            return Ok(());
        }
        if self
            .config
            .max_merges
            .is_some_and(|m| self.stats.merges >= m)
        {
            return Err(Stop::GaveUp(Limit::Merges.reached(self.stats.merges)));
        }
        let (from, into) = self.direction(a, b);
        self.stats.merges += 1;
        self.prune(from);
        self.copy_facts(from, into, dep)?;
        let flags = self.g.nodes[from as usize].flags | flag::MERGED;
        self.g.set_flags(from, flags);
        self.g.set_representative(from, into, dep);
        Ok(())
    }

    /// `(from, into)` by the calculus's order. It leaves two named individuals either way
    /// (`lighter`).
    fn direction(&mut self, s: u32, t: u32) -> (u32, u32) {
        let named = |n: u32| self.g.nodes[n as usize].named != NONE;
        if named(s) && named(t) {
            return self.lighter(s, t);
        }
        let root = |n: u32| self.g.nodes[n as usize].flags & flag::ROOT != 0;
        if named(t) || (root(t) && !named(s)) || self.g.descends(s, t) {
            (s, t)
        } else {
            (t, s)
        }
    }

    /// Of two named individuals, `(from, into)` with `from` the one holding fewer facts, by
    /// `weighed` (I35: a key's members, each merged into the last, copied the whole class at
    /// every merge, memory quadratic in them; PR #72 r1 F1: the smaller class isn't the side
    /// with fewer facts). Both sides' fact lists are read in step, one weighed fact of each in
    /// turn, the source's first: the side that runs out first moves (on a tie the source, as the
    /// calculus orders a pair), and neither is read past the other's count. So a merge copies
    /// no more than the other way round would, and a fact that is new where it goes joins at
    /// least as many as it left (a copy already there adds nothing).
    ///
    /// Each side is read for at most `WALK` entries, retracted ones included, so the choice
    /// costs O(`WALK`) however long the lists. When a side hasn't run out by then and the other
    /// doesn't run out holding no more, the smaller class goes into the larger (union by size):
    /// classes partition the nodes, so that at least doubles the class of every fact it moves,
    /// a fact moves that way at most log₂ n times for a class of n members, and such moves copy
    /// O(F log n) facts in all for F facts.
    fn lighter(&mut self, s: u32, t: u32) -> (u32, u32) {
        let (decided, walked) = {
            let mut sides = [self.weighed(s), self.weighed(t)];
            let (mut count, mut walked) = ([0u32; 2], [0u32; 2]);
            let decided = 'read: loop {
                for side in 0..2 {
                    match next_fact(&mut sides[side], &mut walked[side]) {
                        Read::Fact => count[side] += 1,
                        Read::End => break 'read Some(side),
                        Read::Spent => {
                            // The other side moves only if it runs out holding no more.
                            let other = 1 - side;
                            let moves = loop {
                                if count[other] > count[side] {
                                    break None;
                                }
                                match next_fact(&mut sides[other], &mut walked[other]) {
                                    Read::Fact => count[other] += 1,
                                    Read::End => break Some(other),
                                    Read::Spent => break None,
                                }
                            };
                            break 'read moves;
                        }
                    }
                }
            };
            (decided, walked[0] + walked[1])
        };
        self.stats.merge_walk += u64::from(walked);
        let pair = [s, t];
        match decided {
            Some(side) => (pair[side], pair[1 - side]),
            None if self.g.class_sizes[s as usize] <= self.g.class_sizes[t as usize] => (s, t),
            None => (t, s),
        }
    }

    /// Each entry of `node`'s fact lists, in `copy_facts`'s order: whether it weighs. A
    /// retracted entry doesn't, nor an edge or inequality whose other end is dead. An edge to
    /// `node`'s own successor does, though `prune(node)` removes it before the copy: it stands
    /// for the subtree the target builds again from the labels it is copied. Left out, S2's
    /// witness (`a : ≥1024 R.C`, `∃R.C ⊑ {b}`, 64 labels on b) moved a, and b built a's 1,024
    /// successors and their inequalities again: 1,051,717 facts, where moving b held 525,956
    /// (PR #72's fix, Sol r2).
    fn weighed(&self, node: u32) -> impl Iterator<Item = bool> + '_ {
        let g = &self.g;
        g.fact_entries(node).map(move |entry| match entry {
            None => false,
            Some(NONE) => true,
            Some(other) => g.live(other),
        })
    }

    /// Marks every descendant of `node` pruned.
    fn prune(&mut self, node: u32) {
        let len = self.g.nodes.len() as u32;
        for n in node + 1..len {
            let parent = self.g.nodes[n as usize].parent;
            if parent == NONE {
                continue;
            }
            let gone = parent == node || self.g.nodes[parent as usize].flags & flag::PRUNED != 0;
            if gone && self.g.live(n) {
                let flags = self.g.nodes[n as usize].flags | flag::PRUNED;
                self.g.set_flags(n, flags);
            }
        }
    }

    /// Copies the facts of `from` onto `into`, each with `dep` added.
    fn copy_facts(&mut self, from: u32, into: u32, dep: DepSetId) -> Step<()> {
        let moved = |n: u32| if n == from { into } else { n };
        let labels: Vec<(u32, DepSetId)> =
            self.g.labels(from).map(|f| (f.concept, f.dep)).collect();
        for (c, d) in labels {
            let d = self.deps.union(d, dep);
            self.assert(Lit::Concept(c, into), d, proof::MERGE)?;
        }
        let negatives: Vec<(u32, DepSetId)> = self
            .g
            .negative_facts(from)
            .map(|f| (f.concept, f.dep))
            .collect();
        for (c, d) in negatives {
            let d = self.deps.union(d, dep);
            self.add_negative(into, c, d, proof::MERGE)?;
        }
        let numbers: Vec<(bool, u32, DepSetId)> = self
            .g
            .number_facts(from)
            .map(|f| (f.at_most, f.number, f.dep))
            .collect();
        for (at_most, number, d) in numbers {
            let d = self.deps.union(d, dep);
            let lit = Lit::Number {
                at_most,
                number,
                node: into,
            };
            self.assert(lit, d, proof::MERGE)?;
        }
        let mut edges: Vec<(u32, u32, u32, DepSetId)> = self
            .g
            .out_edges(from)
            .map(|(_, e)| (e.role, e.from, e.to, e.dep))
            .collect();
        edges.extend(
            self.g
                .in_edges(from)
                .map(|(_, e)| (e.role, e.from, e.to, e.dep)),
        );
        for (r, a, b, d) in edges {
            if !(self.g.live(a) && self.g.live(b)) {
                continue;
            }
            let d = self.deps.union(d, dep);
            self.assert(Lit::Role(r, moved(a), moved(b)), d, proof::MERGE)?;
        }
        let unequal: Vec<(u32, u32, DepSetId)> = self
            .g
            .inequality_facts(from)
            .map(|i| (i.a, i.b, i.dep))
            .collect();
        for (a, b, d) in unequal {
            let other = if a == from { b } else { a };
            if !self.g.live(other) {
                continue;
            }
            let d = self.deps.union(d, dep);
            self.add_inequality(into, other, d, proof::MERGE)?;
        }
        Ok(())
    }
}
