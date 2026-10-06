//! The NI rule (JAIR 2009, §3.2.4 and Table 5): an equality `s ≈ t @u ≤ n R.B` raised by
//! an at-most restriction at a root `u`, between blockable nodes of which `s` isn't a
//! successor of `u`, merges `s` into one of `n` roots reserved for `u`'s `R.B`-neighbours,
//! `⟨u.⟨R, B, i⟩⟩` for `1 ≤ i ≤ n`, a choice per `i`. The reserved roots are what keeps
//! the calculus terminating where nominals, inverses and at-most restrictions meet: a
//! blockable node that would otherwise turn into a fresh root again and again reuses one
//! of them.
//!
//! - **Precedence:** an equality the rule governs is deferred here instead of merged (the
//!   ≈-rule may not take it), and it is applied after saturation, before the ≥-rule. It
//!   applies even where `s` and `t` are one node.
//! - **Identity:** the reserved roots are keyed by `u`'s representative, the restriction
//!   and `i`; each is made when first needed (and again after a backtrack removed it),
//!   saturated, then chosen among.
//! - **Symmetry:** reserved roots nothing was merged into are interchangeable, so the
//!   choice is the first of them, then those in use: `n` alternatives become at most one
//!   more than the roots in use (DL-906 reserves 600 for one restriction).
//! - **Dependencies:** the choice depends on the equality's dependencies and on the merges
//!   that made `s`, `t` and `u` what they are. The choice is not a disjunction that holds
//!   (it picks among fresh individuals), so semantic branching doesn't negate its failed
//!   alternatives; backjumping applies as everywhere.
//! - Once `s` is merged into a reserved root, the equality is no longer the rule's (that
//!   root isn't blockable): it is then an ordinary equality and merges `t` as well.

use hashbrown::HashMap;

use super::depset::DepSetId;
use super::engine::{Engine, Lit, Step, ni_stop, proof};
use super::graph::{Annot, NONE, flag};

/// An equality waiting for the NI rule, as raised.
#[derive(Debug, Clone, Copy)]
pub struct NiPending {
    a: u32,
    b: u32,
    dep: DepSetId,
    annot: Annot,
}

/// The NI rule's state.
#[derive(Debug, Clone, Default)]
pub struct Ni {
    pub pending: Vec<NiPending>,
    /// The pending equalities before this one are settled.
    pub open: u32,
    /// The reserved roots by `(root, restriction, i)`, and each reserved root's key (a
    /// backtrack can remove one and its node index be reused: both must agree).
    roots: HashMap<(u32, u32, u32), u32>,
    keys: HashMap<u32, (u32, u32, u32)>,
}

impl Ni {
    pub fn bytes(&self) -> usize {
        self.pending.capacity() * std::mem::size_of::<NiPending>()
            + (self.roots.capacity() + self.keys.capacity()) * 20
    }
}

impl Engine<'_> {
    /// Whether `a ≈ b @annot` waits for the NI rule (it is in the ABox, then: a clause
    /// instance with it in its head is satisfied).
    pub fn ni_pending(&self, a: u32, b: u32, annot: Annot) -> bool {
        if annot.root == NONE {
            return false;
        }
        let g = &self.g;
        let key = |a: u32, b: u32, annot: Annot| {
            let (a, b) = (g.find(a), g.find(b));
            (a.min(b), a.max(b), g.find(annot.root), annot.number)
        };
        let new = key(a, b, annot);
        self.ni.pending[self.ni.open as usize..]
            .iter()
            .any(|p| key(p.a, p.b, p.annot) == new)
    }

    /// Defers `a ≈ b @annot` to the NI rule; `false` if it is pending already.
    pub fn ni_defer(&mut self, a: u32, b: u32, dep: DepSetId, annot: Annot) -> bool {
        if self.ni_pending(a, b, annot) {
            return false;
        }
        self.ni.pending.push(NiPending { a, b, dep, annot });
        true
    }

    /// The reserved root `⟨u.⟨restriction, i⟩⟩`, if it exists now.
    fn reserved(&self, key: (u32, u32, u32)) -> Option<u32> {
        let &n = self.ni.roots.get(&key)?;
        let node = self.g.nodes.get(n as usize)?;
        let fits = node.flags & flag::ROOT != 0
            && node.parent == NONE
            && node.named == NONE
            && self.ni.keys.get(&n) == Some(&key);
        fits.then_some(n)
    }

    /// Applies the NI rule to the first pending equality it governs; whether anything
    /// changed. Equalities it no longer governs are asserted as ordinary ones.
    pub fn apply_ni(&mut self) -> Step<bool> {
        if self.ni.open as usize == self.ni.pending.len() {
            return Ok(false);
        }
        // Indirectly blocked nodes are left alone: blocking must be current.
        self.update_blocking();
        let mut changed = false;
        let mut settled = true;
        let mut at = self.ni.open as usize;
        while at < self.ni.pending.len() {
            let p = self.ni.pending[at];
            at += 1;
            let (s, t, dep) = self.canonical_pair(p.a, p.b, p.dep);
            let (u, du) = self.canonical(p.annot.root);
            let dep = self.deps.union(dep, du);
            let annot = Annot {
                root: u,
                number: p.annot.number,
            };
            // Pruned with a merge's descendants: the equality went with it (pruning
            // removes every assertion about them).
            let gone = !self.g.live(s) || !self.g.live(t) || !self.g.live(u);
            if gone {
                if settled {
                    self.ni.open = at as u32;
                }
                continue;
            }
            if !self.needs_ni(Lit::Equal(s, t, annot)) {
                if settled {
                    self.ni.open = at as u32;
                }
                if s != t {
                    self.assert(Lit::Equal(s, t, Annot::NONE), dep, proof::MERGE)?;
                    changed = true;
                }
                continue;
            }
            if self.indirectly_blocked(s) || self.indirectly_blocked(t) {
                settled = false;
                continue;
            }
            if annot.number == NONE {
                return Err(ni_stop());
            }
            // `s`: the side that isn't a successor of `u`.
            let s = if self.g.nodes[s as usize].parent != u {
                s
            } else {
                t
            };
            let n = self.p.annotations[annot.number as usize].n;
            // The reserved roots nothing was merged into yet are interchangeable (each has
            // what every node has, no edge, no inequality): one of them stands for all,
            // and is tried first. Then the ones in use.
            let mut unused = None;
            let mut used = Vec::new();
            for i in 0..n {
                let key = (u, annot.number, i);
                let root = match self.reserved(key) {
                    Some(root) => self.g.find(root),
                    None if unused.is_none() => {
                        let root = self.new_node(NONE, NONE)?;
                        self.ni.roots.insert(key, root);
                        self.ni.keys.insert(root, key);
                        // Saturated before the choice: a branch point is opened with every
                        // queue empty.
                        return Ok(true);
                    }
                    None => continue,
                };
                let node = &self.g.nodes[root as usize];
                let fresh = node.first_out == NONE
                    && node.first_in == NONE
                    && node.inequalities == NONE
                    && node.flags & flag::ROOT != 0;
                if fresh {
                    unused.get_or_insert(root);
                } else if !used.contains(&root) {
                    used.push(root);
                }
            }
            let alternatives: Vec<Lit> = unused
                .into_iter()
                .chain(used)
                .map(|root| Lit::Equal(s, root, Annot::NONE))
                .collect();
            self.stats.ni_applications += 1;
            if let [only] = alternatives[..] {
                self.assert(only, dep, proof::CHOICE)?;
            } else {
                self.branch_with(alternatives, dep, false)?;
            }
            return Ok(true);
        }
        Ok(changed)
    }
}
