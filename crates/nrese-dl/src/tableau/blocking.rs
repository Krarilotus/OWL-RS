//! Blocking (JAIR 2009, Definition 7; HermiT's implementation, §7 there): the status of
//! every node in creation order, which is a strict order containing the ancestor
//! relation. A blockable node is
//! - **indirectly blocked** if its parent is blocked;
//! - **directly blocked** by an earlier node that isn't blocked and has the same
//!   signature: its atomic concepts, and under pairwise blocking also its parent's and
//!   the roles of the edges between the two, both ways.
//!
//! Candidate blockers are found through a 128-bit hash of the signature, and the exact
//! signature is compared after a hash hit. Signatures are cached per node and reused while
//! the lists they were read from are unchanged (same heads, same cut generation). Single
//! blocking is used only where it is complete: the clauses are simple (Definition 10, no
//! inverses). Anywhere blocking can be switched to ancestor blocking.
//!
//! **Incremental** (`Config::incremental_blocking`): a pass rechecks only what a change
//! can affect, in creation order, since a node's status depends on earlier nodes only:
//! the nodes whose facts or edges changed (the graph's touched list), under pairwise
//! blocking their children (whose signatures hold the parent's label), the children of a
//! node whose status changed (indirect blocking), the nodes a node blocked when it
//! changes, and the later nodes listed under a node's new signature (it may block them
//! now). A backtrack's cut redoes every node from the lowest it changed. Recomputing every
//! node from the lowest changed one after each choice made the passes quadratic in the
//! graph (DL-209's entailment case: 19 of 25 s). With `Config::check_blocking` each pass
//! is compared with a recomputation from scratch (the oracle for the debug and fuzz runs:
//! a wrongly blocked node would be unsound).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
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
    /// The hash it is listed under in the table of unblocked nodes, if it is.
    listed: Option<u128>,
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
    /// By blocker: the nodes it blocked when they were checked (stale entries are
    /// rechecked harmlessly).
    blocked_by: HashMap<u32, Vec<u32>>,
    /// The pass a node was last queued in.
    queued: Vec<u32>,
    pass: u32,
    /// The nodes the ≥-rule must look at again (checked or changed since its last pass):
    /// these, and every node from `expand_from` on.
    pub expand: Vec<u32>,
    pub expand_from: u32,
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
        let listed = self.blocking.cache[index].listed;
        self.blocking.cache[index] = Cached {
            generation,
            heads,
            hash,
            sig,
            valid: true,
            listed,
        };
        hash
    }

    /// Removes `n` from the table of unblocked nodes; the hash it was listed under.
    #[inline]
    fn unlist(&mut self, n: u32) -> Option<u128> {
        let c = self.blocking.cache.get_mut(n as usize)?;
        c.listed?;
        let hash = c.listed.take()?;
        if let Some(bucket) = self.blocking.table.get_mut(&hash) {
            bucket.retain(|&t| t != n);
        }
        Some(hash)
    }

    fn queue(&mut self, heap: &mut BinaryHeap<Reverse<u32>>, n: u32) {
        let i = n as usize;
        if i >= self.g.nodes.len() {
            return;
        }
        if self.blocking.queued.len() <= i {
            self.blocking.queued.resize(i + 1, 0);
        }
        if self.blocking.queued[i] != self.blocking.pass {
            self.blocking.queued[i] = self.blocking.pass;
            heap.push(Reverse(n));
        }
    }

    /// The live children of `n` (its successors in the tree).
    fn children(&self, n: u32) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .g
            .out_edges(n)
            .map(|(_, e)| e.to)
            .chain(self.g.in_edges(n).map(|(_, e)| e.from))
            .filter(|&c| c != n && self.g.nodes[c as usize].parent == n && self.g.live(c))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The nodes linked to `n` by an edge, either way.
    fn neighbours_of(&self, n: u32) -> Vec<u32> {
        self.g
            .out_edges(n)
            .map(|(_, e)| e.to)
            .chain(self.g.in_edges(n).map(|(_, e)| e.from))
            .collect()
    }

    /// Brings every node's blocking status up to date (see the module's description).
    pub fn update_blocking(&mut self) {
        let started = Instant::now();
        let pairwise = self.pairwise();
        let len = self.g.nodes.len() as u32;
        let touched = std::mem::take(&mut self.g.touched);
        let mut floor = std::mem::replace(&mut self.g.full_from, NONE);
        if !self.config.incremental_blocking {
            floor = touched.iter().copied().fold(floor, u32::min);
        }
        self.blocking.pass = self.blocking.pass.wrapping_add(1);
        if self.blocking.cache.len() < len as usize {
            self.blocking.cache.resize(len as usize, Cached::default());
        }
        let mut heap = BinaryHeap::new();
        let floor = floor.min(len);
        if floor < len || floor < self.blocking.extent {
            // Everything from `floor` on, including what a cut removed since.
            let upto = (self.blocking.extent as usize).min(self.blocking.cache.len());
            let (cache, table) = (&mut self.blocking.cache, &mut self.blocking.table);
            for (n, c) in cache[floor as usize..upto].iter_mut().enumerate() {
                if let Some(hash) = c.listed.take()
                    && let Some(bucket) = table.get_mut(&hash)
                {
                    let n = (n + floor as usize) as u32;
                    bucket.retain(|&t| t != n);
                }
            }
        }
        let mut touched = touched;
        touched.retain(|&n| n < floor.min(len));
        touched.sort_unstable();
        touched.dedup();
        for &n in &touched {
            self.queue(&mut heap, n);
            if pairwise {
                for c in self.children(n) {
                    self.queue(&mut heap, c);
                }
            }
        }
        // Before `floor`, what changed and what that affects; from it on every node, in
        // order (what a change there affects is redone anyway, so nothing is passed on).
        while let Some(&Reverse(n)) = heap.peek() {
            if n >= floor {
                break;
            }
            heap.pop();
            self.recheck(n, pairwise, true, &mut heap);
        }
        for n in floor..len {
            self.recheck_unlisted(n, pairwise);
        }
        self.blocking.expand_from = self.blocking.expand_from.min(floor);
        self.blocking.extent = len;
        self.stats.blocking += started.elapsed();
        if self.config.check_blocking {
            self.check_blocking(pairwise);
        }
    }

    /// Recomputes `n`'s status in a pass that redoes every node from some floor up to and
    /// beyond `n`, in order: `n` is unlisted already, and what its change affects is
    /// redone anyway.
    fn recheck_unlisted(&mut self, n: u32, pairwise: bool) {
        let node = self.g.nodes[n as usize];
        if !node.live() {
            return;
        }
        let old = node.flags & flag::BLOCKED;
        let mut flags = node.flags & !flag::BLOCKED;
        let mut blocker = NONE;
        if flags & (flag::ROOT | flag::CONCRETE) == 0 {
            if self.g.nodes[node.parent as usize].flags & flag::BLOCKED != 0 {
                flags |= flag::INDIRECTLY_BLOCKED;
            } else {
                self.stats.blocking_tests += 1;
                let hash = self.refresh(n, pairwise);
                self.g.nodes[n as usize].blocking_hash = hash;
                let cache = &self.blocking.cache;
                let sig = &cache[n as usize].sig;
                let found = self.blocking.table.get(&hash).and_then(|cands| {
                    cands.iter().copied().find(|&t| {
                        t < n
                            && &cache[t as usize].sig == sig
                            && (self.config.anywhere_blocking || self.g.descends(n, t))
                    })
                });
                match found {
                    Some(t) => {
                        flags |= flag::DIRECTLY_BLOCKED;
                        blocker = t;
                        self.stats.blocking_hits += 1;
                        if !(old == flag::DIRECTLY_BLOCKED && node.blocker == t) {
                            self.blocking.blocked_by.entry(t).or_default().push(n);
                        }
                    }
                    None => {
                        let bucket = self.blocking.table.entry(hash).or_default();
                        let at = bucket.partition_point(|&t| t < n);
                        bucket.insert(at, n);
                        self.blocking.cache[n as usize].listed = Some(hash);
                    }
                }
            }
        }
        self.g.nodes[n as usize].flags = flags;
        self.g.nodes[n as usize].blocker = blocker;
        if flags & flag::BLOCKED != old {
            let neighbours = self.neighbours_of(n);
            self.blocking.expand.extend(neighbours);
        }
    }

    /// Recomputes `n`'s status and, if `propagate`, queues what its change affects.
    fn recheck(
        &mut self,
        n: u32,
        pairwise: bool,
        propagate: bool,
        heap: &mut BinaryHeap<Reverse<u32>>,
    ) {
        let node = self.g.nodes[n as usize];
        let old = (
            node.flags & flag::BLOCKED,
            self.blocking.cache[n as usize].listed,
        );
        let mut flags = node.flags & !flag::BLOCKED;
        let mut blocker = NONE;
        let mut listed = None;
        if !node.live() || flags & (flag::ROOT | flag::CONCRETE) != 0 {
            self.unlist(n);
        } else if self.g.nodes[node.parent as usize].flags & flag::BLOCKED != 0 {
            flags |= flag::INDIRECTLY_BLOCKED;
            self.unlist(n);
        } else {
            self.stats.blocking_tests += 1;
            let hash = self.refresh(n, pairwise);
            self.g.nodes[n as usize].blocking_hash = hash;
            let cache = &self.blocking.cache;
            let sig = &cache[n as usize].sig;
            let found = self.blocking.table.get(&hash).and_then(|cands| {
                cands.iter().copied().find(|&t| {
                    t < n
                        && &cache[t as usize].sig == sig
                        && (self.config.anywhere_blocking || self.g.descends(n, t))
                })
            });
            match found {
                Some(t) => {
                    flags |= flag::DIRECTLY_BLOCKED;
                    blocker = t;
                    self.stats.blocking_hits += 1;
                    self.unlist(n);
                    // Listed once per blocker: a node blocked by `t` is in `t`'s list (a
                    // list taken resets its nodes' blockers, so they are listed again).
                    if !(old.0 == flag::DIRECTLY_BLOCKED && node.blocker == t) {
                        self.blocking.blocked_by.entry(t).or_default().push(n);
                    }
                }
                None => {
                    if old.1 != Some(hash) {
                        self.unlist(n);
                        let bucket = self.blocking.table.entry(hash).or_default();
                        let at = bucket.partition_point(|&t| t < n);
                        bucket.insert(at, n);
                        self.blocking.cache[n as usize].listed = Some(hash);
                    }
                    listed = Some(hash);
                }
            }
        }
        // Blocking status is recomputed before every use: no trail.
        self.g.nodes[n as usize].flags = flags;
        self.g.nodes[n as usize].blocker = blocker;
        if propagate {
            self.blocking.expand.push(n);
        }
        if flags & flag::BLOCKED != old.0 {
            // Their ≥-restrictions count non-successor neighbours that aren't blocked.
            let neighbours = self.neighbours_of(n);
            self.blocking.expand.extend(neighbours);
        }
        if !propagate {
            return;
        }
        if flags & flag::BLOCKED != old.0 {
            for c in self.children(n) {
                self.queue(heap, c);
            }
        }
        if old.1.is_some() && old.1 != listed {
            // What it blocked must find another blocker, or none.
            if let Some(blocked) = self.blocking.blocked_by.remove(&n) {
                for b in blocked {
                    if (b as usize) < self.g.nodes.len() && self.g.nodes[b as usize].blocker == n {
                        self.g.nodes[b as usize].blocker = NONE;
                    }
                    self.queue(heap, b);
                }
            }
        }
        if let Some(hash) = listed
            && old.1 != listed
        {
            // It may block later nodes with its signature.
            let later: Vec<u32> = self
                .blocking
                .table
                .get(&hash)
                .map(|b| b.iter().copied().filter(|&t| t > n).collect())
                .unwrap_or_default();
            for t in later {
                self.queue(heap, t);
            }
        }
    }

    /// Recomputes every node's blocking status from scratch.
    pub fn recompute_blocking(&mut self) {
        self.g.full_from = 0;
        self.update_blocking();
    }

    /// The oracle: every node's status as a recomputation from scratch has it (panics on
    /// a difference).
    fn check_blocking(&self, pairwise: bool) {
        let len = self.g.nodes.len();
        let mut status = vec![0u32; len];
        let mut seen: std::collections::HashMap<Signature, Vec<u32>> =
            std::collections::HashMap::new();
        for n in 0..len as u32 {
            let node = &self.g.nodes[n as usize];
            if !node.live() || node.flags & (flag::ROOT | flag::CONCRETE) != 0 {
                continue;
            }
            if status[node.parent as usize] != 0 {
                status[n as usize] = flag::INDIRECTLY_BLOCKED;
                continue;
            }
            let sig = self.signature(n, pairwise);
            let blocked = seen.get(&sig).is_some_and(|ts| {
                ts.iter()
                    .any(|&t| self.config.anywhere_blocking || self.g.descends(n, t))
            });
            if blocked {
                status[n as usize] = flag::DIRECTLY_BLOCKED;
            } else {
                seen.entry(sig).or_default().push(n);
            }
        }
        for (n, (node, &expected)) in self.g.nodes.iter().zip(&status).enumerate() {
            if node.live() {
                assert_eq!(
                    node.flags & flag::BLOCKED,
                    expected,
                    "node {n}: blocking status differs from a recomputation"
                );
            }
        }
    }

    pub fn blocked(&self, n: u32) -> bool {
        self.g.nodes[n as usize].flags & flag::BLOCKED != 0
    }

    pub fn indirectly_blocked(&self, n: u32) -> bool {
        self.g.nodes[n as usize].flags & flag::INDIRECTLY_BLOCKED != 0
    }
}
