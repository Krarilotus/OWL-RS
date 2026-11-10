//! Equivalence classes of ids by union (the `owl:sameAs` classes of equality by
//! representatives): each class represented by its smallest id.
//!
//! The one union kernel of the reasoner (its equality by representatives, in the batch
//! executor's rounds and in compact-mode commits) and the engine (the classes its reads
//! expand), G7 of the investigation of 6 October 2026. A union-find by size: a class's
//! members hang from a root (the root of the larger side wins a union), the root knows
//! the class's smallest id and its members, and a union moves the smaller side's members
//! to the larger: O(n log n) for any order of unions. Lookups follow parents without
//! compressing them (they take `&self`); union by size keeps the paths at O(log n), and
//! most ids are in no class (one hash miss). Splitting a class isn't supported (a reason
//! forest would be its next step, B3).

use hashbrown::HashMap;

/// Test-only: the members unions moved on this thread, counted where they are moved, so a
/// counted caller's tests see the work itself and not what it was told (REAS-3a fix check r1
/// F3). Compiled for this crate's tests, or with the `witness` feature in a debug build (other
/// crates' tests, as a dev-dependency).
#[cfg(any(test, all(feature = "witness", debug_assertions)))]
pub mod witness {
    use std::cell::Cell;

    thread_local! {
        static MOVED: Cell<u64> = const { Cell::new(0) };
        static EXAMINED: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn add(n: u64) {
        MOVED.with(|moved| moved.set(moved.get() + n));
    }

    pub(crate) fn examine(n: u64) {
        EXAMINED.with(|examined| examined.set(examined.get() + n));
    }

    /// The members unions have moved on this thread so far.
    pub fn moved() -> u64 {
        MOVED.with(Cell::get)
    }

    /// The steps listing the classes has taken on this thread so far (fix check r2 G3).
    pub fn examined() -> u64 {
        EXAMINED.with(Cell::get)
    }
}

/// Classes of two or more ids; an id in none is its own class.
#[derive(Debug, Clone, Default)]
pub struct Classes {
    /// Every id in a class of two or more, with its parent (a root is its own parent).
    parent: HashMap<u64, u64>,
    /// Per root: the class's smallest id, its members (in no particular order) and its root's
    /// place in `listed`.
    roots: HashMap<u64, (u64, Vec<u64>, usize)>,
    /// The roots, densely. Listing the classes walks only these, never a table emptied by
    /// unions (fix check r2 G3); a lookup reads `roots` alone, as before G3 (`p2-reas3-abl3`:
    /// a lookup through the list cost reasoning and equality queries up to 0.4 % of their
    /// instructions).
    listed: Vec<u64>,
}

impl PartialEq for Classes {
    /// The same classes, whatever their roots and member order.
    fn eq(&self, other: &Self) -> bool {
        let canonical = |classes: &Self| {
            let mut all: Vec<(u64, Vec<u64>)> = classes
                .classes()
                .map(|(r, members)| {
                    let mut members = members.to_vec();
                    members.sort_unstable();
                    (r, members)
                })
                .collect();
            all.sort_unstable();
            all
        };
        canonical(self) == canonical(other)
    }
}

impl Eq for Classes {}

impl Classes {
    fn root(&self, id: u64) -> Option<u64> {
        let mut at = *self.parent.get(&id)?;
        while let Some(&up) = self.parent.get(&at)
            && up != at
        {
            at = up;
        }
        Some(at)
    }

    /// The representative of `id` (the id itself outside every class).
    pub fn representative(&self, id: u64) -> u64 {
        self.root(id).map_or(id, |root| self.roots[&root].0)
    }

    /// The members of `id`'s class, in no particular order, if it has two or more.
    pub fn class_of(&self, id: u64) -> Option<&[u64]> {
        self.root(id).map(|root| self.roots[&root].1.as_slice())
    }

    /// The members of `id`'s class (`[id]` outside every class), sorted.
    pub fn members(&self, id: u64) -> Vec<u64> {
        let mut members = self.class_of(id).map_or_else(|| vec![id], <[u64]>::to_vec);
        members.sort_unstable();
        members
    }

    /// Whether `id` represents its class (or is in none).
    pub fn is_representative(&self, id: u64) -> bool {
        self.representative(id) == id
    }

    /// The classes of two or more ids: representative and members (in no particular
    /// order), each class one step of a dense list.
    pub fn classes(&self) -> impl ExactSizeIterator<Item = (u64, &[u64])> {
        self.listed.iter().map(|root| {
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            witness::examine(1);
            let (representative, members, _) = &self.roots[root];
            (*representative, members.as_slice())
        })
    }

    pub fn is_empty(&self) -> bool {
        self.listed.is_empty()
    }

    /// `fact` with every id replaced by its representative.
    pub fn rewrite(&self, [s, p, o]: [u64; 3]) -> [u64; 3] {
        [
            self.representative(s),
            self.representative(p),
            self.representative(o),
        ]
    }

    /// Every fact a fact over representatives stands for: each combination of its ids'
    /// members.
    pub fn expand(&self, [s, p, o]: [u64; 3]) -> Vec<[u64; 3]> {
        let (ms, mp, mo) = (
            self.class_of(s).unwrap_or(std::slice::from_ref(&s)),
            self.class_of(p).unwrap_or(std::slice::from_ref(&p)),
            self.class_of(o).unwrap_or(std::slice::from_ref(&o)),
        );
        let mut out = Vec::with_capacity(ms.len() * mp.len() * mo.len());
        for &s in ms {
            for &p in mp {
                for &o in mo {
                    out.push([s, p, o]);
                }
            }
        }
        out
    }

    /// Merges the classes of `a` and `b`; returns the representative that lost its place
    /// (the larger of the two smallest ids), or `None` if they were one class.
    pub fn union(&mut self, a: u64, b: u64) -> Option<u64> {
        self.union_asking(a, b, |_| true)
            .expect("a union never told to stop finishes")
    }

    /// [`Self::union`], `ask(n)` called before each `n` members it moves, `n` at most
    /// [`crate::POLL_UNITS`], so a caller that counts its work for a stop sees it within a run
    /// of members however large the classes are (the engine's counted reads,
    /// `nrese_engine::visit`; REAS-3a fix check r1 R1). `None` once `ask` answers false: the
    /// union is then half made and these classes are no use, so a caller that stops drops
    /// them. (Growing the larger side's list and the maps, allocator work, is held for the
    /// owner with the engine's map initialisation: fix check r1 F2.)
    pub fn union_asking(
        &mut self,
        a: u64,
        b: u64,
        mut ask: impl FnMut(u64) -> bool,
    ) -> Option<Option<u64>> {
        let (ra, rb) = (self.root_or_new(a), self.root_or_new(b));
        if ra == rb {
            self.drop_if_single(ra);
            return Some(None);
        }
        let size = |root: u64| self.roots[&root].1.len();
        let (keep, gone) = match size(ra) >= size(rb) {
            true => (ra, rb),
            false => (rb, ra),
        };
        let (gone_min, moved) = self.remove_root(gone);
        self.parent.insert(gone, keep);
        let (keep_min, members, _) = self.roots.get_mut(&keep).expect("a root");
        members.reserve(moved.len());
        for part in moved.chunks(crate::POLL_UNITS as usize) {
            if !ask(part.len() as u64) {
                return None;
            }
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            witness::add(part.len() as u64);
            members.extend_from_slice(part);
        }
        let lost = gone_min.max(*keep_min);
        *keep_min = gone_min.min(*keep_min);
        Some(Some(lost))
    }

    /// Merges the classes of each pair; returns the representatives that lost their place.
    pub fn union_all(&mut self, pairs: &[(u64, u64)]) -> Vec<u64> {
        pairs
            .iter()
            .filter_map(|&(a, b)| self.union(a, b))
            .collect()
    }

    /// The root of `id`'s class, a class of its own made for it if it has none.
    fn root_or_new(&mut self, id: u64) -> u64 {
        match self.root(id) {
            Some(root) => root,
            None => {
                self.parent.insert(id, id);
                self.roots.insert(id, (id, vec![id], self.listed.len()));
                self.listed.push(id);
                id
            }
        }
    }

    /// Takes `root`'s class out (the last listed root moved into its place): its smallest id
    /// and members.
    fn remove_root(&mut self, root: u64) -> (u64, Vec<u64>) {
        let (min, members, at) = self.roots.remove(&root).expect("a root");
        self.listed.swap_remove(at);
        if let Some(moved) = self.listed.get(at) {
            self.roots.get_mut(moved).expect("a root").2 = at;
        }
        (min, members)
    }

    /// Forgets `root` if its class is one id (made for a union of an id with itself).
    fn drop_if_single(&mut self, root: u64) {
        if self
            .roots
            .get(&root)
            .is_some_and(|(_, members, _)| members.len() == 1)
        {
            self.remove_root(root);
            self.parent.remove(&root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_merge_expand_and_rewrite() {
        let mut classes = Classes::default();
        assert_eq!(classes.union_all(&[(5, 3), (7, 9)]), vec![5, 9]);
        assert!(classes.union_all(&[(3, 5), (8, 8)]).is_empty());
        assert_eq!(classes.classes().count(), 2, "no class of one");
        assert_eq!(classes.union_all(&[(9, 5)]), vec![7]);
        assert_eq!(classes.members(7), &[3, 5, 7, 9]);
        assert_eq!(classes.representative(9), 3);
        assert_eq!(classes.members(42), vec![42]);
        assert!(classes.is_representative(3) && !classes.is_representative(5));
        assert!(classes.is_representative(42));
        assert_eq!(classes.rewrite([9, 1, 42]), [3, 1, 42]);
        assert_eq!(classes.expand([3, 1, 42]).len(), 4);
        // The representative that lost its place is reported, whichever side is larger.
        assert_eq!(classes.union(2, 7), Some(3));
        assert_eq!(classes.members(9), &[2, 3, 5, 7, 9]);
        assert_eq!(classes.union(9, 2), None);
        assert_eq!(classes.union(11, 10), Some(11));
        // The smaller class moves; the larger representative loses its place.
        assert_eq!(classes.union(10, 3), Some(10));
        assert_eq!(classes.representative(11), 2);
    }

    /// Unions in any order cost O(n log n): a class of 200,000 built one id at a time,
    /// each new id smaller than the class's representative (the worst order for moving
    /// the class under the smaller id).
    #[test]
    fn a_large_class_in_any_order_is_cheap() {
        let mut classes = Classes::default();
        let n = 200_000u64;
        for i in (0..n).rev() {
            classes.union(i, n);
        }
        assert_eq!(classes.representative(n), 0);
        assert_eq!(classes.class_of(n).map(<[u64]>::len), Some(n as usize + 1));
    }
}
