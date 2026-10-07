//! Dependency sets (docs/design/owl2-dl.md §6): the branch points a fact depends on, as
//! interned persistent chains, hash-consed by `(point, rest)` as HermiT's are.
//!
//! A set is a chain of branch levels in decreasing order, so its newest point (where a
//! clash jumps back to) is the head, and equal sets are one id. Sets are values: a fact
//! keeps its id when the search backtracks past other facts, and an id never changes its
//! meaning, so unions can be memoised for the whole run.
//!
//! Dependency sets answer "where to jump back to" only; which axioms explain a fact is
//! its proof id, kept apart (D, E).

use hashbrown::HashMap;

/// An interned dependency set; [`DepSetId::EMPTY`] depends on no choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DepSetId(pub(crate) u32);

impl DepSetId {
    pub const EMPTY: Self = Self(0);

    pub fn is_empty(self) -> bool {
        self == Self::EMPTY
    }
}

/// Above this many memoised unions the memo is cleared: it is a cache, not state.
const MEMO_LIMIT: usize = 1 << 20;

/// The arena of dependency sets.
#[derive(Debug, Clone)]
pub struct DepSets {
    /// `(point, rest)` per set; index 0 is the empty set.
    cells: Vec<(u32, DepSetId)>,
    index: HashMap<(u32, DepSetId), DepSetId>,
    unions: HashMap<(DepSetId, DepSetId), DepSetId>,
    /// `{1, …, k}` by `k`, for chronological backtracking.
    prefixes: Vec<DepSetId>,
}

impl Default for DepSets {
    fn default() -> Self {
        Self {
            cells: vec![(0, DepSetId::EMPTY)],
            index: HashMap::new(),
            unions: HashMap::new(),
            prefixes: vec![DepSetId::EMPTY],
        }
    }
}

impl DepSets {
    /// The set `{point} ∪ rest`, where `point` is above every point of `rest`.
    fn cons(&mut self, point: u32, rest: DepSetId) -> DepSetId {
        debug_assert!(self.max(rest).is_none_or(|m| m < point));
        if let Some(&id) = self.index.get(&(point, rest)) {
            return id;
        }
        let id = DepSetId(self.cells.len() as u32);
        self.cells.push((point, rest));
        self.index.insert((point, rest), id);
        id
    }

    /// The newest branch point of `set`.
    pub fn max(&self, set: DepSetId) -> Option<u32> {
        (!set.is_empty()).then(|| self.cells[set.0 as usize].0)
    }

    fn rest(&self, set: DepSetId) -> DepSetId {
        self.cells[set.0 as usize].1
    }

    /// The set of one branch point.
    pub fn single(&mut self, point: u32) -> DepSetId {
        self.cons(point, DepSetId::EMPTY)
    }

    /// Whether `point` is in `set`.
    pub fn contains(&self, mut set: DepSetId, point: u32) -> bool {
        while !set.is_empty() {
            let (p, rest) = self.cells[set.0 as usize];
            if p <= point {
                return p == point;
            }
            set = rest;
        }
        false
    }

    /// The points of `set`, newest first.
    pub fn points(&self, mut set: DepSetId) -> Vec<u32> {
        let mut out = Vec::new();
        while !set.is_empty() {
            out.push(self.cells[set.0 as usize].0);
            set = self.rest(set);
        }
        out
    }

    /// `a ∪ b`.
    pub fn union(&mut self, a: DepSetId, b: DepSetId) -> DepSetId {
        if a == b || b.is_empty() {
            return a;
        }
        if a.is_empty() {
            return b;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(&id) = self.unions.get(&key) {
            return id;
        }
        // Merge the two descending chains, then rebuild from the smallest point up; the
        // longest common tail is shared as it is.
        let (mut x, mut y) = (a, b);
        let mut points = Vec::new();
        let tail = loop {
            if x == y {
                break x;
            }
            match (self.max(x), self.max(y)) {
                (None, _) => break y,
                (_, None) => break x,
                (Some(p), Some(q)) if p > q => {
                    points.push(p);
                    x = self.rest(x);
                }
                (Some(p), Some(q)) if q > p => {
                    points.push(q);
                    y = self.rest(y);
                }
                (Some(p), Some(_)) => {
                    points.push(p);
                    x = self.rest(x);
                    y = self.rest(y);
                }
            }
        };
        let mut out = tail;
        for &p in points.iter().rev() {
            out = self.cons(p, out);
        }
        if self.unions.len() > MEMO_LIMIT {
            self.unions.clear();
        }
        self.unions.insert(key, out);
        out
    }

    /// `set` without `point`.
    pub fn without(&mut self, set: DepSetId, point: u32) -> DepSetId {
        let mut above = Vec::new();
        let mut at = set;
        while let Some(p) = self.max(at) {
            if p < point {
                return set;
            }
            if p == point {
                let mut out = self.rest(at);
                for &q in above.iter().rev() {
                    out = self.cons(q, out);
                }
                return out;
            }
            above.push(p);
            at = self.rest(at);
        }
        set
    }

    /// `{1, …, k}`: every branch level up to `k` (chronological backtracking).
    pub fn prefix(&mut self, k: u32) -> DepSetId {
        while self.prefixes.len() <= k as usize {
            let next = self.prefixes.len() as u32;
            let below = self.prefixes[next as usize - 1];
            let set = self.cons(next, below);
            self.prefixes.push(set);
        }
        self.prefixes[k as usize]
    }

    /// Interned sets, for the memory measure.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether only the empty set is interned.
    pub fn is_empty(&self) -> bool {
        self.cells.len() == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(d: &mut DepSets, points: &[u32]) -> DepSetId {
        points.iter().fold(DepSetId::EMPTY, |acc, &p| {
            let s = d.single(p);
            d.union(acc, s)
        })
    }

    #[test]
    fn unions_are_canonical_and_sorted() {
        let mut d = DepSets::default();
        let a = set(&mut d, &[3, 1, 7]);
        let b = set(&mut d, &[7, 3, 1]);
        assert_eq!(a, b);
        assert_eq!(d.points(a), vec![7, 3, 1]);
        let c = set(&mut d, &[2, 7]);
        let u = d.union(a, c);
        assert_eq!(d.points(u), vec![7, 3, 2, 1]);
        assert_eq!(d.max(u), Some(7));
        let w = d.without(u, 3);
        assert_eq!(d.points(w), vec![7, 2, 1]);
        assert_eq!(d.without(u, 5), u);
        let three = d.prefix(3);
        assert_eq!(d.points(three), vec![3, 2, 1]);
        assert_eq!(d.union(DepSetId::EMPTY, a), a);
    }
}
