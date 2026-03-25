use std::collections::{BTreeSet, HashMap};

use crate::dataset_index::IndexedDataset;
use crate::taxonomy::TaxonomyIndex;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PreparedClassConsistency {
    effective_disjoint_targets_by_class: HashMap<u32, BTreeSet<u32>>,
    unsatisfiable_classes: BTreeSet<u32>,
    owl_nothing_id: u32,
}

impl PreparedClassConsistency {
    pub(crate) fn build(index: &IndexedDataset, taxonomy: &TaxonomyIndex) -> Self {
        let owl_nothing_id = index.owl_nothing_id();
        let mut class_ids = taxonomy.nodes().clone();
        class_ids.insert(owl_nothing_id);
        class_ids.extend(index.disjoint_class_pairs().keys().copied());
        for targets in index.disjoint_class_pairs().values() {
            class_ids.extend(targets.iter().copied());
        }
        for class_ids_for_instance in index.type_assertions().values() {
            class_ids.extend(class_ids_for_instance.iter().copied());
        }

        let mut effective_disjoint_targets_by_class = HashMap::new();
        let mut unsatisfiable_classes = BTreeSet::new();

        for &class_id in &class_ids {
            let disjoint_targets = index
                .disjoint_class_pairs()
                .get(&class_id)
                .cloned()
                .unwrap_or_default();
            if let Some(ancestors) = taxonomy.ancestors_of(class_id)
                && ancestors.contains(&owl_nothing_id)
            {
                unsatisfiable_classes.insert(class_id);
            }

            if class_id == owl_nothing_id {
                unsatisfiable_classes.insert(class_id);
            }
            if !disjoint_targets.is_empty() {
                effective_disjoint_targets_by_class.insert(class_id, disjoint_targets);
            }
        }

        Self {
            effective_disjoint_targets_by_class,
            unsatisfiable_classes,
            owl_nothing_id,
        }
    }

    pub(crate) fn effective_disjoint_targets(&self, class_id: u32) -> Option<&BTreeSet<u32>> {
        self.effective_disjoint_targets_by_class.get(&class_id)
    }

    pub(crate) fn unsatisfiable_classes(&self) -> &BTreeSet<u32> {
        &self.unsatisfiable_classes
    }

    pub(crate) fn owl_nothing_id(&self) -> u32 {
        self.owl_nothing_id
    }
}
