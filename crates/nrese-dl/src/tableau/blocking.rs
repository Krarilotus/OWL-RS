//! Blocking (JAIR 2009, Definition 7; HermiT's implementation, §7 there): the status of
//! every node in creation order, which is a strict order containing the ancestor
//! relation. A blockable node is
//! - **indirectly blocked** if its parent is blocked;
//! - **directly blocked** by an earlier node that isn't blocked and has the same
//!   signature: its atomic concepts, and under pairwise blocking also its parent's and
//!   the roles of the edges between the two, both ways.
//!
//! Candidate blockers are found through a 128-bit hash of the signature, and the exact
//! signature is compared after a hash hit, so the whole pass takes a linear number of
//! lookups. Signatures are cached per node and reused while the lists they were read
//! from are unchanged (same heads, same cut generation). Single blocking is used only
//! where it is complete: the clauses are simple (Definition 10, no inverses). Anywhere
//! blocking can be switched to ancestor blocking.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use hashbrown::HashMap;

use super::engine::Engine;
use super::graph::{NONE, flag};

/// A node's blocking signature, flat: its sorted concepts, then (pairwise) the
/// parent's, the roles towards the parent and those from it, each part preceded by its
/// length.
type Signature = Vec<u32>;

/// A cached signature with what it was read from.
#[derive(Debug, Clone, Default)]
struct Cached {
    generation: u32,
    heads: [u32; 4],
    hash: u128,
    sig: Signature,
    valid: bool,
    /// Listed in the table of unblocked nodes under `hash`.
    in_table: bool,
}

/// The blocking pass's reusable state (cold data beside the hot nodes).
#[derive(Debug, Default)]
pub struct Blocking {
    cache: Vec<Cached>,
    table: HashMap<u128, Vec<u32>>,
    /// How many nodes the last pass saw.
    extent: u32,
    /// The cached signatures' bytes (for the memory budget).
    pub bytes: usize,
}

impl Engine<'_> {
    fn pairwise(&self) -> bool {
        !(self.config.single_blocking && self.p.simple)
    }

    fn heads(&self, n: u32, pairwise: bool) -> [u32; 4] {
        let node = &self.g.nodes[n as usize];
        if pairwise {
            let parent = &self.g.nodes[node.parent as usize];
            [node.label, parent.label, node.first_out, node.first_in]
        } else {
            [node.label, NONE, NONE, NONE]
        }
    }

    fn signature(&self, n: u32, pairwise: bool) -> Signature {
        fn part(sig: &mut Signature, items: impl Iterator<Item = u32>) {
            let at = sig.len();
            sig.push(0);
            sig.extend(items);
            sig[at + 1..].sort_unstable();
            sig[at] = (sig.len() - at - 1) as u32;
        }
        let mut sig = Signature::new();
        part(&mut sig, self.g.labels(n).map(|f| f.concept));
        if pairwise {
            let p = self.g.nodes[n as usize].parent;
            part(&mut sig, self.g.labels(p).map(|f| f.concept));
            let up = self
                .g
                .out_edges(n)
                .filter(|(_, e)| e.to == p)
                .map(|(_, e)| e.role);
            part(&mut sig, up);
            let down = self
                .g
                .in_edges(n)
                .filter(|(_, e)| e.from == p)
                .map(|(_, e)| e.role);
            part(&mut sig, down);
        }
        sig.shrink_to_fit();
        sig
    }

    /// Brings the cached signature of `n` up to date; its hash.
    fn refresh(&mut self, n: u32, pairwise: bool) -> u128 {
        let heads = self.heads(n, pairwise);
        let generation = self.g.generation;
        let index = n as usize;
        if self.blocking.cache.len() <= index {
            self.blocking.cache.resize(index + 1, Cached::default());
        }
        let c = &self.blocking.cache[index];
        if c.valid && c.generation == generation && c.heads == heads {
            return c.hash;
        }
        let sig = self.signature(n, pairwise);
        // A fixed-key hasher: cached hashes are compared across passes.
        let mut h = std::hash::DefaultHasher::new();
        sig.hash(&mut h);
        let low = h.finish();
        sig.len().hash(&mut h);
        let hash = (u128::from(h.finish()) << 64) | u128::from(low);
        let old = self.blocking.cache[index].sig.capacity();
        self.blocking.bytes = (self.blocking.bytes + sig.capacity() * 4).saturating_sub(old * 4);
        self.blocking.cache[index] = Cached {
            generation,
            heads,
            hash,
            sig,
            valid: true,
            in_table: false,
        };
        hash
    }

    /// Recomputes the blocking status of every node from `floor` on; those before it
    /// keep theirs (nothing they depend on changed).
    pub fn compute_blocking(&mut self, floor: u32) {
        let started = Instant::now();
        let pairwise = self.pairwise();
        let len = self.g.nodes.len() as u32;
        let floor = floor.min(self.blocking.extent).min(len);
        let mut table = std::mem::take(&mut self.blocking.table);
        // Unlist the nodes the pass redoes (including any cut away since).
        let upto = (self.blocking.extent as usize).min(self.blocking.cache.len());
        for n in floor as usize..upto {
            let c = &mut self.blocking.cache[n];
            if c.in_table {
                c.in_table = false;
                if let Some(bucket) = table.get_mut(&c.hash) {
                    bucket.retain(|&t| t as usize != n);
                }
            }
        }
        for n in floor..len {
            let node = self.g.nodes[n as usize];
            if !node.live() {
                continue;
            }
            let mut flags = node.flags & !flag::BLOCKED;
            let mut blocker = NONE;
            if flags & flag::ROOT == 0 {
                let parent = self.g.nodes[node.parent as usize];
                if parent.flags & flag::BLOCKED != 0 {
                    flags |= flag::INDIRECTLY_BLOCKED;
                } else {
                    self.stats.blocking_tests += 1;
                    let hash = self.refresh(n, pairwise);
                    self.g.nodes[n as usize].blocking_hash = hash;
                    let cache = &self.blocking.cache;
                    let sig = &cache[n as usize].sig;
                    let found = table.get(&hash).and_then(|cands| {
                        cands.iter().copied().find(|&t| {
                            &cache[t as usize].sig == sig
                                && (self.config.anywhere_blocking || self.g.descends(n, t))
                        })
                    });
                    match found {
                        Some(t) => {
                            flags |= flag::DIRECTLY_BLOCKED;
                            blocker = t;
                            self.stats.blocking_hits += 1;
                        }
                        None => {
                            table.entry(hash).or_default().push(n);
                            self.blocking.cache[n as usize].in_table = true;
                        }
                    }
                }
            }
            // Blocking status is recomputed before every use: no trail.
            self.g.nodes[n as usize].flags = flags;
            self.g.nodes[n as usize].blocker = blocker;
        }
        self.blocking.extent = len;
        self.blocking.table = table;
        self.stats.blocking += started.elapsed();
    }

    pub fn blocked(&self, n: u32) -> bool {
        self.g.nodes[n as usize].flags & flag::BLOCKED != 0
    }

    pub fn indirectly_blocked(&self, n: u32) -> bool {
        self.g.nodes[n as usize].flags & flag::INDIRECTLY_BLOCKED != 0
    }
}
