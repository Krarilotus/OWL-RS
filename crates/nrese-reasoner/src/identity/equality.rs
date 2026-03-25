use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::dataset_index::IndexedDataset;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EqualityIndex {
    members_by_canonical: BTreeMap<u32, BTreeSet<u32>>,
    canonical_by_member: HashMap<u32, u32>,
    assertion_count: usize,
}

impl EqualityIndex {
    pub fn build(index: &IndexedDataset) -> Self {
        Self::build_from_pairs(
            index.same_as_pairs().iter().copied(),
            index.same_as_assertion_count(),
        )
    }

    pub fn build_with_inferred(
        index: &IndexedDataset,
        inferred_pairs: &BTreeSet<(u32, u32)>,
    ) -> Self {
        Self::build_from_pairs(
            index
                .same_as_pairs()
                .iter()
                .copied()
                .chain(inferred_pairs.iter().copied()),
            index.same_as_assertion_count(),
        )
    }

    pub fn assertion_count(&self) -> usize {
        self.assertion_count
    }

    pub fn cluster_count(&self) -> usize {
        self.members_by_canonical.len()
    }

    pub fn equivalents_of(&self, member_id: u32) -> Option<&BTreeSet<u32>> {
        let canonical_id = self.canonical_by_member.get(&member_id)?;
        self.members_by_canonical.get(canonical_id)
    }

    pub fn canonical_of(&self, member_id: u32) -> u32 {
        self.canonical_by_member
            .get(&member_id)
            .copied()
            .unwrap_or(member_id)
    }

    pub fn are_equivalent(&self, left_id: u32, right_id: u32) -> bool {
        self.canonical_of(left_id) == self.canonical_of(right_id)
    }

    fn build_from_pairs<I>(pairs: I, assertion_count: usize) -> Self
    where
        I: IntoIterator<Item = (u32, u32)>,
    {
        let mut union_find = UnionFind::default();
        for (left_id, right_id) in pairs {
            union_find.ensure(left_id);
            union_find.ensure(right_id);
            union_find.union(left_id, right_id);
        }

        if union_find.parent_by_member.is_empty() {
            return Self {
                members_by_canonical: BTreeMap::new(),
                canonical_by_member: HashMap::new(),
                assertion_count,
            };
        }

        let member_ids = union_find.members().collect::<Vec<_>>();
        let mut raw_clusters = BTreeMap::<u32, BTreeSet<u32>>::new();
        for member_id in member_ids {
            let root_id = union_find.find(member_id);
            raw_clusters.entry(root_id).or_default().insert(member_id);
        }

        let mut members_by_canonical = BTreeMap::new();
        let mut canonical_by_member = HashMap::new();
        for members in raw_clusters.into_values() {
            let Some(&canonical_id) = members.iter().next() else {
                continue;
            };

            for &member_id in &members {
                canonical_by_member.insert(member_id, canonical_id);
            }
            members_by_canonical.insert(canonical_id, members);
        }

        Self {
            members_by_canonical,
            canonical_by_member,
            assertion_count,
        }
    }
}

#[derive(Debug, Default)]
struct UnionFind {
    parent_by_member: HashMap<u32, u32>,
}

impl UnionFind {
    fn ensure(&mut self, member_id: u32) {
        self.parent_by_member.entry(member_id).or_insert(member_id);
    }

    fn find(&mut self, member_id: u32) -> u32 {
        let parent_id = *self
            .parent_by_member
            .get(&member_id)
            .expect("union-find member must exist");
        if parent_id == member_id {
            return member_id;
        }

        let root_id = self.find(parent_id);
        self.parent_by_member.insert(member_id, root_id);
        root_id
    }

    fn union(&mut self, left_id: u32, right_id: u32) {
        let left_root = self.find(left_id);
        let right_root = self.find(right_id);
        if left_root == right_root {
            return;
        }

        let (canonical_root, non_canonical_root) = if left_root <= right_root {
            (left_root, right_root)
        } else {
            (right_root, left_root)
        };
        self.parent_by_member
            .insert(non_canonical_root, canonical_root);
    }

    fn members(&self) -> impl Iterator<Item = u32> + '_ {
        self.parent_by_member.keys().copied()
    }
}

#[cfg(test)]
#[path = "../tests/equality_tests.rs"]
mod tests;
