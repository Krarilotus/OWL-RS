use std::collections::{BTreeSet, HashMap};

use crate::dataset_index::IndexedDataset;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PropertyCharacteristicPlan {
    constrained_predicates: BTreeSet<u32>,
    irreflexive_properties: BTreeSet<u32>,
    asymmetric_properties: BTreeSet<u32>,
    functional_properties: BTreeSet<u32>,
    inverse_functional_properties: BTreeSet<u32>,
    property_disjoint_pairs: HashMap<u32, BTreeSet<u32>>,
}

impl PropertyCharacteristicPlan {
    pub(crate) fn build(index: &IndexedDataset) -> Self {
        let mut constrained_predicates = BTreeSet::new();
        constrained_predicates.extend(index.irreflexive_properties().iter().copied());
        constrained_predicates.extend(index.asymmetric_properties().iter().copied());
        constrained_predicates.extend(index.functional_properties().iter().copied());
        constrained_predicates.extend(index.inverse_functional_properties().iter().copied());
        constrained_predicates.extend(index.property_disjoint_pairs().keys().copied());

        Self {
            constrained_predicates,
            irreflexive_properties: index.irreflexive_properties().clone(),
            asymmetric_properties: index.asymmetric_properties().clone(),
            functional_properties: index.functional_properties().clone(),
            inverse_functional_properties: index.inverse_functional_properties().clone(),
            property_disjoint_pairs: index.property_disjoint_pairs().clone(),
        }
    }

    pub(crate) fn is_constrained_predicate(&self, predicate_id: u32) -> bool {
        self.constrained_predicates.contains(&predicate_id)
    }

    pub(crate) fn irreflexive_properties(&self) -> &BTreeSet<u32> {
        &self.irreflexive_properties
    }

    pub(crate) fn asymmetric_properties(&self) -> &BTreeSet<u32> {
        &self.asymmetric_properties
    }

    pub(crate) fn functional_properties(&self) -> &BTreeSet<u32> {
        &self.functional_properties
    }

    pub(crate) fn inverse_functional_properties(&self) -> &BTreeSet<u32> {
        &self.inverse_functional_properties
    }

    pub(crate) fn property_disjoint_pairs(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.property_disjoint_pairs
    }
}
