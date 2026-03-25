use crate::closure_index::ClosureIndex;

pub type TaxonomyIndex = ClosureIndex;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::TaxonomyIndex;

    #[test]
    fn taxonomy_index_memoizes_transitive_ancestors() {
        let mut edges = HashMap::new();
        edges.insert(1, BTreeSet::from([2]));
        edges.insert(2, BTreeSet::from([3]));

        let taxonomy = TaxonomyIndex::from_edges(&edges);
        let ancestors = taxonomy.ancestors_of(1).expect("ancestors");

        assert_eq!(taxonomy.node_count(), 3);
        assert!(ancestors.contains(&2));
        assert!(ancestors.contains(&3));
    }

    #[test]
    fn taxonomy_index_handles_cycles_without_recursing_forever() {
        let mut edges = HashMap::new();
        edges.insert(1, BTreeSet::from([2]));
        edges.insert(2, BTreeSet::from([1]));

        let taxonomy = TaxonomyIndex::from_edges(&edges);

        let ancestors_one = taxonomy.ancestors_of(1).expect("ancestors for 1");
        let ancestors_two = taxonomy.ancestors_of(2).expect("ancestors for 2");

        assert!(ancestors_one.contains(&2));
        assert!(ancestors_two.contains(&1));
    }
}
