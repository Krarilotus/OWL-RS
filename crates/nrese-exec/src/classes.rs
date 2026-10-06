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

/// Classes of two or more ids; an id in none is its own class.
#[derive(Debug, Clone, Default)]
pub struct Classes {
    /// Every id in a class of two or more, with its parent (a root is its own parent).
    parent: HashMap<u64, u64>,
    /// Per root: the class's smallest id and its members, in no particular order.
    roots: HashMap<u64, (u64, Vec<u64>)>,
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
    /// order).
    pub fn classes(&self) -> impl Iterator<Item = (u64, &[u64])> {
        self.roots
            .values()
            .map(|(representative, members)| (*representative, members.as_slice()))
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
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
        let (ra, rb) = (self.root_or_new(a), self.root_or_new(b));
        if ra == rb {
            self.drop_if_single(ra);
            return None;
        }
        let (keep, gone) = match self.roots[&ra].1.len() >= self.roots[&rb].1.len() {
            true => (ra, rb),
            false => (rb, ra),
        };
        let (gone_min, moved) = self.roots.remove(&gone).expect("a root");
        self.parent.insert(gone, keep);
        let (keep_min, members) = self.roots.get_mut(&keep).expect("a root");
        members.extend(moved);
        let lost = gone_min.max(*keep_min);
        *keep_min = gone_min.min(*keep_min);
        Some(lost)
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
                self.roots.insert(id, (id, vec![id]));
                id
            }
        }
    }

    /// Forgets `root` if its class is one id (made for a union of an id with itself).
    fn drop_if_single(&mut self, root: u64) {
        if self
            .roots
            .get(&root)
            .is_some_and(|(_, members)| members.len() == 1)
        {
            self.roots.remove(&root);
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
