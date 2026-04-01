mod builder;
mod expansion;

use std::collections::BTreeSet;

use crate::dataset_index::IndexedDataset;
use crate::identity::EqualityIndex;
use crate::property_chain::PropertyChainPlan;
use crate::property_taxonomy::PropertyTaxonomyIndex;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PropertyClosure {
    assertions: BTreeSet<(u32, u32, u32)>,
    derived_assertions: BTreeSet<(u32, u32, u32)>,
}

impl PropertyClosure {
    pub fn build(
        index: &IndexedDataset,
        property_taxonomy: &PropertyTaxonomyIndex,
        equality: &EqualityIndex,
        property_chain_plan: &PropertyChainPlan,
        policy: &RulesMvpFeaturePolicy,
    ) -> Self {
        builder::PropertyClosureBuilder::build(
            index,
            property_taxonomy,
            equality,
            property_chain_plan,
            policy,
        )
    }

    pub fn all_assertions(&self) -> &BTreeSet<(u32, u32, u32)> {
        &self.assertions
    }

    pub fn derived_assertions(&self) -> &BTreeSet<(u32, u32, u32)> {
        &self.derived_assertions
    }
}

pub(crate) fn insert_assertion(
    closure: &mut PropertyClosure,
    triple: (u32, u32, u32),
    derived: bool,
) -> bool {
    if !closure.assertions.insert(triple) {
        return false;
    }

    if derived {
        closure.derived_assertions.insert(triple);
    }

    true
}

#[cfg(test)]
mod tests;
