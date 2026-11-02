//! The ≈-rule (JAIR 2009, Table 5): merging one node into another. The direction is the
//! calculus's: into a named individual, into a root from a node that isn't named, into an
//! ancestor; otherwise the other way round. Of two named individuals, which the calculus
//! leaves open, the other way round where that copies fewer facts and changes nothing else
//! (`swaps`). The merged node's descendants are pruned,
//! its facts are copied to the target with the equality's dependencies added, and it
//! points at the target from then on. Everything changed in place goes on the trail, so
//! a backtrack undoes the merge (no path compression).
//!
//! A merge the NI rule governs (two blockable neighbours of a root, one of them not its
//! successor, raised by an at-most restriction at the root) is left to it (`ni.rs`).

use super::depset::DepSetId;
use super::engine::{Engine, Lit, Step, Stop, proof};
use super::graph::{Annot, Entries, Entry, NONE, flag};
use crate::limits::Limit;

/// The entries of each side's lists that choosing to swap a merge reads at most (`swaps`). It
/// bounds the choice's work per merge; on I35's key witness the choice reads 22 entries a
/// merge (15 in step, 7 in `alike`), at 1,000 members as at 8,000.
const WALK: u32 = 1024;

/// What reading a side's next entry found.
enum Read {
    Entry(Entry),
    End,
    Spent,
}

/// One side's lists, read against `WALK`.
struct Walk<'g> {
    entries: Entries<'g>,
    read: u32,
}

impl Walk<'_> {
    /// The next entry; the end of the lists costs no read.
    fn step(&mut self) -> Read {
        if self.entries.done() {
            return Read::End;
        }
        if self.read == WALK {
            return Read::Spent;
        }
        self.read += 1;
        self.entries.next().map_or(Read::End, Read::Entry)
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
        let (mut from, mut into) = self.direction(a, b);
        if self.swaps(from, into, dep) {
            (from, into) = (into, from);
        }
        self.stats.merges += 1;
        self.prune(from);
        self.copy_facts(from, into, dep)?;
        let flags = self.g.nodes[from as usize].flags | flag::MERGED;
        self.g.set_flags(from, flags);
        self.g.set_representative(from, into, dep);
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

    /// Whether `t` goes into `s` instead, where `direction` put `s` into `t`: only between two
    /// named individuals (`direction` leaves their order to the equality), only for a merge
    /// that depends on nothing (so every fact keeps the dependencies it would have the other
    /// way), and only where neither has successors and `t` copies fewer facts (`lighter`), every
    /// fact both hold with the same dependencies (`alike`).
    /// Then the graph after the merge is the other way's, its class standing under `s`, but
    /// with fewer copies (I35: a key's members, each merged into the last, copied the whole
    /// class at every merge, memory quadratic in them). Design and proof:
    /// Harness notes/2026-11-02-i35-merge-direction-design.md.
    fn swaps(&mut self, s: u32, t: u32, dep: DepSetId) -> bool {
        let named = |n: u32| self.g.nodes[n as usize].named != NONE;
        if dep != DepSetId::EMPTY || !named(s) || !named(t) {
            return false;
        }
        let (lighter, read) = self.lighter(s, t);
        let (swap, more) = if lighter {
            self.alike(s, t)
        } else {
            (false, 0)
        };
        self.stats.merge_walk += u64::from(read + more);
        swap
    }

    /// Whether moving `t` into `s` leaves every fact the dependencies moving `s` into `t` gives
    /// it, and the facts read to tell. A fact both hold keeps `t`'s dependencies that way and
    /// `s`'s this way, so they must be equal (PR #72 r2 sol-loop S1: a disjunct's `C(s)` beside
    /// an asserted `C(t)` turned a clash without dependencies into one that backtracks). No fact
    /// may meet its complement (the copy would clash at whichever pair it reaches first), and
    /// no edge may join the two (its copies coalesce with the self-loops in copying order).
    /// Reads `t`'s live facts once, which `lighter` read to their end within `WALK`.
    fn alike(&self, s: u32, t: u32) -> (bool, u32) {
        let g = &self.g;
        let mut read = 0u32;
        for f in g.labels(t) {
            read += 1;
            let held = g.concept(s, f.concept);
            if g.negative(s, f.concept).is_some()
                || held.is_some_and(|i| g.unary[i as usize].dep != f.dep)
            {
                return (false, read);
            }
        }
        for f in g.negative_facts(t) {
            read += 1;
            let held = g.negative(s, f.concept);
            if g.concept(s, f.concept).is_some()
                || held.is_some_and(|i| g.negatives[i as usize].dep != f.dep)
            {
                return (false, read);
            }
        }
        for f in g.number_facts(t) {
            read += 1;
            let held = g.number(s, f.at_most, f.number);
            if held.is_some_and(|i| g.numbers[i as usize].dep != f.dep) {
                return (false, read);
            }
        }
        let moved = |n: u32| if n == t { s } else { n };
        for (_, e) in g.out_edges(t).chain(g.in_edges(t)) {
            read += 1;
            if e.from == s || e.to == s {
                return (false, read);
            }
            if !(g.live(e.from) && g.live(e.to)) {
                continue;
            }
            let held = g.edge(e.role, moved(e.from), moved(e.to));
            if held.is_some_and(|i| g.edges[i as usize].dep != e.dep) {
                return (false, read);
            }
        }
        for f in g.inequality_facts(t) {
            read += 1;
            let other = if f.a == t { f.b } else { f.a };
            let held = g.unequal(s, other);
            if g.live(other) && held.is_some_and(|i| g.inequalities[i as usize].dep != f.dep) {
                return (false, read);
            }
        }
        (true, read)
    }

    /// Whether moving `t` copies fewer facts than moving `s` and neither prunes nor keeps
    /// successors; and the entries read to tell.
    ///
    /// A side holding a live at-least number may have successors (the ≥-rule makes them from
    /// one, and retraction kills both by the same dependencies): no swap. Otherwise `prune`
    /// marks nothing either way. `t`'s count is every live entry (a self-loop twice), at least
    /// what moving it asserts; `s`'s is each live fact once, at most what moving it asserts
    /// plus its duplicates, which `t`'s entries match one for one. So `t` showing fewer than
    /// `s` means fewer new copies. The lists are read in step, `t` never ahead of `s` by more
    /// than one entry, `t` to its end within `WALK`; `s` until it shows more, its numbers read
    /// to their end. So the choice reads at most twice what copying `s` walks, plus one.
    fn lighter(&self, s: u32, t: u32) -> (bool, u32) {
        let g = &self.g;
        let live = |e: &Entry| !e.dead && (e.other == NONE || g.live(e.other));
        let mut ws = Walk {
            entries: g.entries(s),
            read: 0,
        };
        let mut wt = Walk {
            entries: g.entries(t),
            read: 0,
        };
        let (mut held_s, mut held_t) = (0u32, 0u32);
        let swap = 'choice: {
            // `t` to its end, never more than one entry ahead of `s`.
            while !wt.entries.done() {
                let (walk, held, once) = if wt.read > ws.read {
                    (&mut ws, &mut held_s, true)
                } else {
                    (&mut wt, &mut held_t, false)
                };
                match walk.step() {
                    Read::Entry(e) if e.at_least && !e.dead => break 'choice false,
                    Read::Entry(e) => {
                        if live(&e) && !(once && e.repeat) {
                            *held += 1;
                        }
                    }
                    Read::End | Read::Spent => break 'choice false,
                }
                if ws.entries.done() && held_t >= held_s {
                    break 'choice false;
                }
            }
            // `s` until it shows more than `t`, its numbers to their end.
            while held_s <= held_t || ws.entries.numbers_left() {
                match ws.step() {
                    Read::Entry(e) if e.at_least && !e.dead => break 'choice false,
                    Read::Entry(e) => {
                        if live(&e) && !e.repeat {
                            held_s += 1;
                        }
                    }
                    Read::End => break 'choice held_s > held_t,
                    Read::Spent => break 'choice false,
                }
            }
            true
        };
        (swap, ws.read + wt.read)
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
