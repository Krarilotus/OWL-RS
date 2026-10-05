//! The completion graph as append-only fact tables (docs/design/owl2-dl.md §6, "data
//! layout"): the semantic truth lives in the tables, nodes hold the heads of their
//! per-node lists through the tables, and undo is a cut of every table back to a saved
//! length plus a trail of the few fields changed in place.
//!
//! - **Facts** carry their dependency set and, apart from it, their proof (the HT-clause
//!   or rule that made them).
//! - **Lists without a trail:** a fact is linked into its node's list as the new head; a
//!   cut removes facts newest first, so restoring each head from the removed fact's
//!   `next` gives back the old lists exactly.
//! - **Trail:** only node flags and representatives change in place (pruning, merging).
//! - **Merges have dependencies:** a merged node keeps what its merge depended on, and
//!   following a node to its representative collects them ([`Graph::find_with`]): a fact
//!   stated about a node that was merged since holds of the representative only by those
//!   merges (HermiT's canonical-node dependency set).
//! - **Membership:** a hash index per table; the hyperresolution joins and the clash
//!   checks ask it.

use hashbrown::HashMap;

use super::depset::DepSetId;
use super::program::{ConceptId, RoleId};

pub const NONE: u32 = u32::MAX;

/// Node flags.
pub mod flag {
    /// A root (an individual of the ABox, or the test's individual): never blocked.
    pub const ROOT: u32 = 1;
    /// Removed by a merge's pruning.
    pub const PRUNED: u32 = 2;
    /// Merged into its representative.
    pub const MERGED: u32 = 4;
    /// Blocking status, recomputed before each expansion.
    pub const DIRECTLY_BLOCKED: u32 = 8;
    pub const INDIRECTLY_BLOCKED: u32 = 16;
    /// A data value (concrete node): a leaf, never blocked nor a blocker, no centre of a
    /// clause, outside the NI rule (`data.rs`).
    pub const CONCRETE: u32 = 32;
    pub const DEAD: u32 = PRUNED | MERGED;
    pub const BLOCKED: u32 = DIRECTLY_BLOCKED | INDIRECTLY_BLOCKED;
}

/// The expansion and blocking data of a node: one cache line.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
pub struct HotNode {
    pub parent: u32,
    pub representative: u32,
    /// Heads of the node's lists in the tables.
    pub label: u32,
    pub first_out: u32,
    pub first_in: u32,
    pub numbers: u32,
    pub negatives: u32,
    pub inequalities: u32,
    pub blocker: u32,
    pub depth: u32,
    /// The individual's index for a named root, else [`NONE`].
    pub named: u32,
    pub flags: u32,
    /// The hash of the node's blocking signature, from the last blocking pass.
    pub blocking_hash: u128,
}

const _: () = assert!(std::mem::size_of::<HotNode>() == 64);

impl HotNode {
    pub fn live(&self) -> bool {
        self.flags & flag::DEAD == 0
    }
}

/// `concept(node)` or, in the negative table, `¬concept(node)`.
#[derive(Debug, Clone, Copy)]
pub struct Unary {
    pub concept: ConceptId,
    pub node: u32,
    pub dep: DepSetId,
    #[expect(
        dead_code,
        reason = "recorded for the proof IR (package 3.8), not read yet"
    )]
    pub proof: u32,
    pub next: u32,
}

/// `role(from, to)`.
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub role: RoleId,
    pub from: u32,
    pub to: u32,
    pub dep: DepSetId,
    #[expect(
        dead_code,
        reason = "recorded for the proof IR (package 3.8), not read yet"
    )]
    pub proof: u32,
    pub next_out: u32,
    pub next_in: u32,
}

/// `≥ n R.F(node)` (`at_most` false) or `≤ n R.F(node)`, by the number's index.
#[derive(Debug, Clone, Copy)]
pub struct NumberFact {
    pub at_most: bool,
    pub number: u32,
    pub node: u32,
    pub dep: DepSetId,
    #[expect(
        dead_code,
        reason = "recorded for the proof IR (package 3.8), not read yet"
    )]
    pub proof: u32,
    pub next: u32,
}

/// `a ≉ b`.
#[derive(Debug, Clone, Copy)]
pub struct Inequality {
    pub a: u32,
    pub b: u32,
    pub dep: DepSetId,
    #[expect(
        dead_code,
        reason = "recorded for the proof IR (package 3.8), not read yet"
    )]
    pub proof: u32,
    pub next_a: u32,
    pub next_b: u32,
}

/// `a ≈ b`, waiting to be merged.
#[derive(Debug, Clone, Copy)]
pub struct Equality {
    pub a: u32,
    pub b: u32,
    pub dep: DepSetId,
    /// Raised by an at-most restriction at a root: the merge may need the NI rule.
    pub annot: Annot,
}

/// The annotation `@u ≤ n R.B` of an equality (JAIR 2009, Definition 5): the root `u` an
/// at-most restriction raised it at, and the restriction (an index into
/// `Program::annotations`); `NONE` parts where there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Annot {
    pub root: u32,
    pub number: u32,
}

impl Annot {
    pub const NONE: Self = Self {
        root: NONE,
        number: NONE,
    };
}

/// What changed in place, to undo.
#[derive(Debug, Clone, Copy)]
pub enum Undo {
    Flags {
        node: u32,
        old: u32,
    },
    Representative {
        node: u32,
        old: u32,
        old_dep: DepSetId,
    },
}

/// The lengths of every table: a point to cut back to.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mark {
    pub nodes: u32,
    pub unary: u32,
    pub negatives: u32,
    pub edges: u32,
    pub numbers: u32,
    pub inequalities: u32,
    pub equalities: u32,
    pub trail: u32,
}

fn key(a: u32, b: u32) -> u64 {
    (u64::from(a) << 32) | u64::from(b)
}

/// The completion graph.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: Vec<HotNode>,
    pub unary: Vec<Unary>,
    pub negatives: Vec<Unary>,
    pub edges: Vec<Edge>,
    pub numbers: Vec<NumberFact>,
    pub inequalities: Vec<Inequality>,
    pub equalities: Vec<Equality>,
    pub trail: Vec<Undo>,
    /// By node: what its merge into its representative depended on (read only while the
    /// node is merged).
    pub merge_deps: Vec<DepSetId>,
    /// Counts cuts: a fact index can be reused after one, so caches keyed by list heads
    /// are valid within one generation only.
    pub generation: u32,
    /// The nodes whose facts, edges or flags changed since the last blocking pass.
    pub touched: Vec<u32>,
    /// The lowest node a cut changed since the last blocking pass (`NONE`: none): the
    /// pass redoes every node from it.
    pub full_from: u32,
    unary_ix: HashMap<u64, u32>,
    negative_ix: HashMap<u64, u32>,
    edge_ix: HashMap<(u32, u32, u32), u32>,
    number_ix: HashMap<u64, u32>,
    inequality_ix: HashMap<u64, u32>,
}

impl Graph {
    pub fn mark(&self) -> Mark {
        Mark {
            nodes: self.nodes.len() as u32,
            unary: self.unary.len() as u32,
            negatives: self.negatives.len() as u32,
            edges: self.edges.len() as u32,
            numbers: self.numbers.len() as u32,
            inequalities: self.inequalities.len() as u32,
            equalities: self.equalities.len() as u32,
            trail: self.trail.len() as u32,
        }
    }

    fn touch(&mut self, node: u32) {
        self.touched.push(node);
    }

    pub fn new_node(&mut self, parent: u32, named: u32) -> u32 {
        let id = self.nodes.len() as u32;
        self.touch(id);
        let (depth, flags) = if parent == NONE {
            (0, flag::ROOT)
        } else {
            (self.nodes[parent as usize].depth + 1, 0)
        };
        self.nodes.push(HotNode {
            parent,
            representative: id,
            label: NONE,
            first_out: NONE,
            first_in: NONE,
            numbers: NONE,
            negatives: NONE,
            inequalities: NONE,
            blocker: NONE,
            depth,
            named,
            flags,
            blocking_hash: 0,
        });
        self.merge_deps.push(DepSetId::EMPTY);
        id
    }

    pub fn live(&self, node: u32) -> bool {
        self.nodes[node as usize].live()
    }

    /// The node `node` was merged into, followed to a node that wasn't merged.
    pub fn find(&self, mut node: u32) -> u32 {
        while self.nodes[node as usize].flags & flag::MERGED != 0 {
            node = self.nodes[node as usize].representative;
        }
        node
    }

    pub fn set_flags(&mut self, node: u32, flags: u32) {
        let old = self.nodes[node as usize].flags;
        if old != flags {
            self.touch(node);
            let parent = self.nodes[node as usize].parent;
            if parent != NONE {
                self.touch(parent);
            }
            self.trail.push(Undo::Flags { node, old });
            self.nodes[node as usize].flags = flags;
        }
    }

    /// The node `node` was merged into, followed to a node that wasn't merged; `each` is
    /// given what every merge on the way depended on.
    pub fn find_with(&self, mut node: u32, mut each: impl FnMut(DepSetId)) -> u32 {
        while self.nodes[node as usize].flags & flag::MERGED != 0 {
            each(self.merge_deps[node as usize]);
            node = self.nodes[node as usize].representative;
        }
        node
    }

    /// Records that `node` is merged into `to` by `dep`.
    pub fn set_representative(&mut self, node: u32, to: u32, dep: DepSetId) {
        let old = self.nodes[node as usize].representative;
        let old_dep = self.merge_deps[node as usize];
        self.trail.push(Undo::Representative { node, old, old_dep });
        self.nodes[node as usize].representative = to;
        self.merge_deps[node as usize] = dep;
    }

    // Membership -------------------------------------------------------------------------

    pub fn concept(&self, node: u32, concept: ConceptId) -> Option<u32> {
        self.unary_ix.get(&key(node, concept)).copied()
    }

    pub fn negative(&self, node: u32, concept: ConceptId) -> Option<u32> {
        self.negative_ix.get(&key(node, concept)).copied()
    }

    pub fn edge(&self, role: RoleId, from: u32, to: u32) -> Option<u32> {
        self.edge_ix.get(&(role, from, to)).copied()
    }

    pub fn number(&self, node: u32, at_most: bool, number: u32) -> Option<u32> {
        self.number_ix
            .get(&key(node, number | (u32::from(at_most) << 31)))
            .copied()
    }

    pub fn unequal(&self, a: u32, b: u32) -> Option<u32> {
        self.inequality_ix.get(&key(a.min(b), a.max(b))).copied()
    }

    // Adding (no clash checks here: the engine does them) ---------------------------------

    /// Adds `concept(node)`; `false` if it was there.
    pub fn add_concept(
        &mut self,
        node: u32,
        concept: ConceptId,
        dep: DepSetId,
        proof: u32,
    ) -> bool {
        let id = self.unary.len() as u32;
        if self.unary_ix.try_insert(key(node, concept), id).is_err() {
            return false;
        }
        let n = &mut self.nodes[node as usize];
        self.unary.push(Unary {
            concept,
            node,
            dep,
            proof,
            next: n.label,
        });
        n.label = id;
        self.touch(node);
        true
    }

    pub fn add_negative(
        &mut self,
        node: u32,
        concept: ConceptId,
        dep: DepSetId,
        proof: u32,
    ) -> bool {
        let id = self.negatives.len() as u32;
        if self.negative_ix.try_insert(key(node, concept), id).is_err() {
            return false;
        }
        let n = &mut self.nodes[node as usize];
        self.negatives.push(Unary {
            concept,
            node,
            dep,
            proof,
            next: n.negatives,
        });
        n.negatives = id;
        self.touch(node);
        true
    }

    pub fn add_edge(
        &mut self,
        role: RoleId,
        from: u32,
        to: u32,
        dep: DepSetId,
        proof: u32,
    ) -> bool {
        let id = self.edges.len() as u32;
        if self.edge_ix.try_insert((role, from, to), id).is_err() {
            return false;
        }
        let next_out = self.nodes[from as usize].first_out;
        let next_in = self.nodes[to as usize].first_in;
        self.edges.push(Edge {
            role,
            from,
            to,
            dep,
            proof,
            next_out,
            next_in,
        });
        self.nodes[from as usize].first_out = id;
        self.nodes[to as usize].first_in = id;
        // Both ends: a pairwise signature holds the edges between a node and its parent.
        self.touch(from);
        self.touch(to);
        true
    }

    pub fn add_number(
        &mut self,
        node: u32,
        at_most: bool,
        number: u32,
        dep: DepSetId,
        proof: u32,
    ) -> bool {
        let id = self.numbers.len() as u32;
        let k = key(node, number | (u32::from(at_most) << 31));
        if self.number_ix.try_insert(k, id).is_err() {
            return false;
        }
        let n = &mut self.nodes[node as usize];
        self.numbers.push(NumberFact {
            at_most,
            number,
            node,
            dep,
            proof,
            next: n.numbers,
        });
        n.numbers = id;
        self.touch(node);
        true
    }

    /// Adds `a ≉ b` (`a ≠ b`; the engine checks that).
    pub fn add_inequality(&mut self, a: u32, b: u32, dep: DepSetId, proof: u32) -> bool {
        let id = self.inequalities.len() as u32;
        if self
            .inequality_ix
            .try_insert(key(a.min(b), a.max(b)), id)
            .is_err()
        {
            return false;
        }
        let next_a = self.nodes[a as usize].inequalities;
        let next_b = self.nodes[b as usize].inequalities;
        self.inequalities.push(Inequality {
            a,
            b,
            dep,
            proof,
            next_a,
            next_b,
        });
        self.nodes[a as usize].inequalities = id;
        self.nodes[b as usize].inequalities = id;
        self.touch(a.min(b));
        true
    }

    // Cutting back ---------------------------------------------------------------------

    /// Restores the graph to `mark`.
    pub fn cut(&mut self, mark: &Mark) {
        self.generation = self.generation.wrapping_add(1);
        let mut low = mark.nodes;
        while self.trail.len() > mark.trail as usize {
            match self.trail.pop() {
                Some(Undo::Flags { node, old }) => {
                    low = low.min(node);
                    let parent = self.nodes[node as usize].parent;
                    if parent != NONE {
                        low = low.min(parent);
                    }
                    self.nodes[node as usize].flags = old
                }
                Some(Undo::Representative { node, old, old_dep }) => {
                    self.nodes[node as usize].representative = old;
                    self.merge_deps[node as usize] = old_dep;
                }
                None => break,
            }
        }
        let alive = mark.nodes as usize;
        let tail = |v: usize, at: u32| v > at as usize;
        if tail(self.unary.len(), mark.unary) {
            low = low.min(
                self.unary[mark.unary as usize..]
                    .iter()
                    .map(|f| f.node)
                    .min()
                    .unwrap_or(NONE),
            );
        }
        if tail(self.negatives.len(), mark.negatives) {
            low = low.min(
                self.negatives[mark.negatives as usize..]
                    .iter()
                    .map(|f| f.node)
                    .min()
                    .unwrap_or(NONE),
            );
        }
        if tail(self.edges.len(), mark.edges) {
            low = low.min(
                self.edges[mark.edges as usize..]
                    .iter()
                    .map(|e| e.from.min(e.to))
                    .min()
                    .unwrap_or(NONE),
            );
        }
        if tail(self.numbers.len(), mark.numbers) {
            low = low.min(
                self.numbers[mark.numbers as usize..]
                    .iter()
                    .map(|f| f.node)
                    .min()
                    .unwrap_or(NONE),
            );
        }
        if tail(self.inequalities.len(), mark.inequalities) {
            low = low.min(
                self.inequalities[mark.inequalities as usize..]
                    .iter()
                    .map(|i| i.a.min(i.b))
                    .min()
                    .unwrap_or(NONE),
            );
        }
        self.touch(low);
        self.full_from = self.full_from.min(low);
        for f in self.unary.drain(mark.unary as usize..).rev() {
            self.unary_ix.remove(&key(f.node, f.concept));
            if (f.node as usize) < alive {
                self.nodes[f.node as usize].label = f.next;
            }
        }
        for f in self.negatives.drain(mark.negatives as usize..).rev() {
            self.negative_ix.remove(&key(f.node, f.concept));
            if (f.node as usize) < alive {
                self.nodes[f.node as usize].negatives = f.next;
            }
        }
        for e in self.edges.drain(mark.edges as usize..).rev() {
            self.edge_ix.remove(&(e.role, e.from, e.to));
            if (e.from as usize) < alive {
                self.nodes[e.from as usize].first_out = e.next_out;
            }
            if (e.to as usize) < alive {
                self.nodes[e.to as usize].first_in = e.next_in;
            }
        }
        for f in self.numbers.drain(mark.numbers as usize..).rev() {
            self.number_ix
                .remove(&key(f.node, f.number | (u32::from(f.at_most) << 31)));
            if (f.node as usize) < alive {
                self.nodes[f.node as usize].numbers = f.next;
            }
        }
        for i in self.inequalities.drain(mark.inequalities as usize..).rev() {
            self.inequality_ix.remove(&key(i.a.min(i.b), i.a.max(i.b)));
            if (i.a as usize) < alive {
                self.nodes[i.a as usize].inequalities = i.next_a;
            }
            if (i.b as usize) < alive {
                self.nodes[i.b as usize].inequalities = i.next_b;
            }
        }
        self.equalities.truncate(mark.equalities as usize);
        self.nodes.truncate(alive);
        self.merge_deps.truncate(alive);
    }

    // Iteration ------------------------------------------------------------------------

    /// The concepts of `node` (unordered).
    pub fn labels(&self, node: u32) -> impl Iterator<Item = &Unary> {
        let mut at = self.nodes[node as usize].label;
        std::iter::from_fn(move || {
            (at != NONE).then(|| {
                let f = &self.unary[at as usize];
                at = f.next;
                f
            })
        })
    }

    pub fn out_edges(&self, node: u32) -> impl Iterator<Item = (u32, &Edge)> {
        let mut at = self.nodes[node as usize].first_out;
        std::iter::from_fn(move || {
            (at != NONE).then(|| {
                let id = at;
                let e = &self.edges[at as usize];
                at = e.next_out;
                (id, e)
            })
        })
    }

    pub fn in_edges(&self, node: u32) -> impl Iterator<Item = (u32, &Edge)> {
        let mut at = self.nodes[node as usize].first_in;
        std::iter::from_fn(move || {
            (at != NONE).then(|| {
                let id = at;
                let e = &self.edges[at as usize];
                at = e.next_in;
                (id, e)
            })
        })
    }

    pub fn number_facts(&self, node: u32) -> impl Iterator<Item = &NumberFact> {
        let mut at = self.nodes[node as usize].numbers;
        std::iter::from_fn(move || {
            (at != NONE).then(|| {
                let f = &self.numbers[at as usize];
                at = f.next;
                f
            })
        })
    }

    pub fn negative_facts(&self, node: u32) -> impl Iterator<Item = &Unary> {
        let mut at = self.nodes[node as usize].negatives;
        std::iter::from_fn(move || {
            (at != NONE).then(|| {
                let f = &self.negatives[at as usize];
                at = f.next;
                f
            })
        })
    }

    pub fn inequality_facts(&self, node: u32) -> impl Iterator<Item = &Inequality> {
        let mut at = self.nodes[node as usize].inequalities;
        std::iter::from_fn(move || {
            (at != NONE).then(|| {
                let f = &self.inequalities[at as usize];
                at = if f.a == node { f.next_a } else { f.next_b };
                f
            })
        })
    }

    /// Whether `node` descends from `ancestor` (by parents).
    pub fn descends(&self, mut node: u32, ancestor: u32) -> bool {
        while node != NONE {
            node = self.nodes[node as usize].parent;
            if node == ancestor {
                return true;
            }
        }
        false
    }

    /// The amortised bytes of the tables (capacity), for the memory measure.
    pub fn bytes(&self) -> usize {
        use std::mem::size_of;
        self.nodes.capacity() * size_of::<HotNode>()
            + self.unary.capacity() * size_of::<Unary>()
            + self.negatives.capacity() * size_of::<Unary>()
            + self.edges.capacity() * size_of::<Edge>()
            + self.numbers.capacity() * size_of::<NumberFact>()
            + self.inequalities.capacity() * size_of::<Inequality>()
            + self.equalities.capacity() * size_of::<Equality>()
            + self.trail.capacity() * size_of::<Undo>()
            + self.merge_deps.capacity() * size_of::<DepSetId>()
            + (self.unary_ix.capacity() + self.negative_ix.capacity()) * 12
            + self.edge_ix.capacity() * 16
            + (self.number_ix.capacity() + self.inequality_ix.capacity()) * 12
    }
}
