use crate::closure_index::ClosureIndex;

pub type PropertyTaxonomyIndex = ClosureIndex;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::PropertyTaxonomyIndex;

    #[test]
    fn property_taxonomy_index_tracks_transitive_super_properties() {
        let mut edges = HashMap::new();
        edges.insert(10, BTreeSet::from([20]));
        edges.insert(20, BTreeSet::from([30]));

        let taxonomy = PropertyTaxonomyIndex::from_edges(&edges);
        let ancestors = taxonomy.ancestors_of(10).expect("ancestors");

        assert_eq!(taxonomy.node_count(), 3);
        assert!(ancestors.contains(&20));
        assert!(ancestors.contains(&30));
    }
}
