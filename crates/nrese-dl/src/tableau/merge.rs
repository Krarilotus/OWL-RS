//! The ≈-rule (JAIR 2009, Table 5): merging one node into another. The direction is the
//! calculus's: into a named individual, into a root from a node that isn't named, into an
//! ancestor; otherwise the other way round. The merged node's descendants are pruned,
//! its facts are copied to the target with the equality's dependencies added, and it
//! points at the target from then on. Everything changed in place goes on the trail, so
//! a backtrack undoes the merge (no path compression).
//!
//! The NI-rule is not implemented: a merge it would govern (two blockable neighbours of
//! a root, one of them not its successor, raised by an at-most restriction at the root)
//! stops the run as `gave-up`, never with an answer.

use super::depset::DepSetId;
use super::engine::{Engine, Lit, Step, ni_stop, proof};
use super::graph::{NONE, flag};

impl Engine<'_> {
    /// Merges `a` and `b` (as they are now) by the equality's `dep`.
    pub fn merge(&mut self, a: u32, b: u32, dep: DepSetId, at_root: u32) -> Step<()> {
        let (a, b) = (self.g.find(a), self.g.find(b));
        if a == b || !self.g.live(a) || !self.g.live(b) {
            return Ok(());
        }
        if let Some(i) = self.g.unequal(a, b) {
            let d = self.g.inequalities[i as usize].dep;
            return Err(self.clash(dep, d));
        }
        if self.needs_ni(Lit::Equal(a, b, at_root)) {
            return Err(ni_stop());
        }
        let (from, into) = self.direction(a, b);
        self.stats.merges += 1;
        self.prune(from);
        self.copy_facts(from, into, dep)?;
        let flags = self.g.nodes[from as usize].flags | flag::MERGED;
        self.g.set_flags(from, flags);
        self.g.set_representative(from, into);
        Ok(())
    }

    /// `(from, into)` by the calculus's order.
    fn direction(&self, s: u32, t: u32) -> (u32, u32) {
        let node = |n: u32| &self.g.nodes[n as usize];
        let named = |n: u32| node(n).named != NONE;
        let root = |n: u32| node(n).flags & flag::ROOT != 0;
        if named(t) || (root(t) && !named(s)) || self.g.descends(s, t) {
            (s, t)
        } else {
            (t, s)
        }
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
