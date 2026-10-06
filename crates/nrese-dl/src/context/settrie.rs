//! A set-trie for subset queries (Savnik, *Index data structure for fast subset and
//! superset queries*, CD-ARES 2013): the bodies of a context's clauses with one head, so
//! that "is some stored body a subset of this one" (forward redundancy, and the Pred
//! join's pruning) walks only the trie paths whose atoms the query has, instead of every
//! clause with that head.
//!
//! Bodies are sorted (the core keeps them so); a path from the root spells a body, and
//! its end node holds the clauses with that body. Removed clauses stay and are skipped
//! by the caller's liveness test.

use super::atoms::Atom;
use super::state::ClauseId;

#[derive(Debug, Default)]
struct Node {
    /// Children by atom, sorted.
    children: Vec<(Atom, u32)>,
    clauses: Vec<ClauseId>,
}

/// The bodies of the clauses with one head.
#[derive(Debug)]
pub struct SetTrie {
    nodes: Vec<Node>,
}

impl Default for SetTrie {
    fn default() -> Self {
        Self {
            nodes: vec![Node::default()],
        }
    }
}

impl SetTrie {
    /// Adds clause `c` with `body` (sorted).
    pub fn insert(&mut self, body: &[Atom], c: ClauseId) {
        let mut at = 0u32;
        for &a in body {
            let children = &self.nodes[at as usize].children;
            at = match children.binary_search_by_key(&a, |&(b, _)| b) {
                Ok(i) => children[i].1,
                Err(i) => {
                    let id = self.nodes.len() as u32;
                    self.nodes.push(Node::default());
                    self.nodes[at as usize].children.insert(i, (a, id));
                    id
                }
            };
        }
        self.nodes[at as usize].clauses.push(c);
    }

    /// Whether a stored body is a subset of `query` (sorted) with a clause `live` says
    /// is there.
    pub fn has_subset(&self, query: &[Atom], live: &dyn Fn(ClauseId) -> bool) -> bool {
        self.walk(0, query, live)
    }

    fn walk(&self, at: u32, query: &[Atom], live: &dyn Fn(ClauseId) -> bool) -> bool {
        let node = &self.nodes[at as usize];
        if node.clauses.iter().any(|&c| live(c)) {
            return true;
        }
        if node.children.is_empty() {
            return false;
        }
        let children = &node.children;
        if children.len() > 4 * query.len() {
            // Many children: look each query atom up.
            for (i, &q) in query.iter().enumerate() {
                if let Ok(k) = children.binary_search_by_key(&q, |&(b, _)| b)
                    && self.walk(children[k].1, &query[i + 1..], live)
                {
                    return true;
                }
            }
            return false;
        }
        // Few: walk both sorted lists together.
        let (mut i, mut k) = (0, 0);
        while i < query.len() && k < children.len() {
            let (b, child) = children[k];
            match query[i].cmp(&b) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => k += 1,
                std::cmp::Ordering::Equal => {
                    if self.walk(child, &query[i + 1..], live) {
                        return true;
                    }
                    i += 1;
                    k += 1;
                }
            }
        }
        false
    }

    /// Memory in bytes, roughly.
    pub fn bytes(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| 48 + n.children.capacity() * 8 + n.clauses.capacity() * 4)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::atoms::CTerm;

    fn atoms(ids: &[u32]) -> Vec<Atom> {
        let mut v: Vec<Atom> = ids.iter().map(|&i| Atom::concept(i, CTerm::X)).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn subsets_are_found_and_only_they() {
        let mut t = SetTrie::default();
        t.insert(&atoms(&[1, 3]), 0);
        t.insert(&atoms(&[2]), 1);
        t.insert(&atoms(&[1, 4, 5]), 2);
        let live = |_: ClauseId| true;
        assert!(t.has_subset(&atoms(&[1, 2, 3]), &live));
        assert!(t.has_subset(&atoms(&[2]), &live));
        assert!(t.has_subset(&atoms(&[1, 3, 9]), &live));
        assert!(!t.has_subset(&atoms(&[1, 4]), &live));
        assert!(t.has_subset(&atoms(&[1, 4, 5, 6]), &live));
        assert!(!t.has_subset(&atoms(&[3, 4, 5]), &live));
        assert!(!t.has_subset(&atoms(&[]), &live));
        // A removed clause doesn't count.
        let only_two = |c: ClauseId| c == 2;
        assert!(!t.has_subset(&atoms(&[1, 3]), &only_two));
        t.insert(&[], 3);
        assert!(t.has_subset(&atoms(&[]), &live));
    }
}
