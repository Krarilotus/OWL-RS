mod builder;
mod origins;

pub use origins::EffectiveTypeOrigin;

use std::collections::{BTreeMap, HashMap};

use crate::dataset_index::IndexedDataset;
use crate::identity::EqualityIndex;
use crate::property_closure::PropertyClosure;
use crate::taxonomy::TaxonomyIndex;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectiveTypeSet {
    types_by_instance: HashMap<u32, BTreeMap<u32, EffectiveTypeOrigin>>,
}

impl EffectiveTypeSet {
    pub fn build(
        index: &IndexedDataset,
        class_taxonomy: &TaxonomyIndex,
        property_closure: &PropertyClosure,
        equality: &EqualityIndex,
        include_domain_range_typing: bool,
    ) -> Self {
        builder::build_effective_type_set(
            index,
            class_taxonomy,
            property_closure,
            equality,
            include_domain_range_typing,
        )
    }

    pub fn instances(&self) -> impl Iterator<Item = (&u32, &BTreeMap<u32, EffectiveTypeOrigin>)> {
        self.types_by_instance.iter()
    }

    pub fn classes_for(&self, instance_id: u32) -> Option<&BTreeMap<u32, EffectiveTypeOrigin>> {
        self.types_by_instance.get(&instance_id)
    }

    pub fn describe_origin(index: &IndexedDataset, origin: &EffectiveTypeOrigin) -> Option<String> {
        origins::describe_origin(index, origin)
    }
}

pub(crate) fn effective_origin_rank(origin: &EffectiveTypeOrigin) -> u8 {
    origins::effective_origin_rank(origin)
}

pub(crate) fn from_types_by_instance(
    types_by_instance: HashMap<u32, BTreeMap<u32, EffectiveTypeOrigin>>,
) -> EffectiveTypeSet {
    EffectiveTypeSet { types_by_instance }
}

#[cfg(test)]
mod tests;
