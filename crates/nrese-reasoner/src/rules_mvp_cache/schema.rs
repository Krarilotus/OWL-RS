use std::collections::BTreeSet;

use crate::class_consistency::PreparedClassConsistency;
use crate::dataset_index::IndexedDataset;
use crate::materialization::{derive_subclass_axiom_closure, derive_subproperty_axiom_closure};
use crate::property_chain::PropertyChainPlan;
use crate::property_consistency::PropertyCharacteristicPlan;
use crate::property_taxonomy::PropertyTaxonomyIndex;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;
use crate::taxonomy::TaxonomyIndex;

#[derive(Debug, Clone)]
pub(super) struct CachedPreparedSchema {
    pub(super) class_taxonomy: TaxonomyIndex,
    pub(super) class_consistency: PreparedClassConsistency,
    pub(super) property_taxonomy: PropertyTaxonomyIndex,
    pub(super) property_chain_plan: PropertyChainPlan,
    pub(super) property_characteristic_plan: PropertyCharacteristicPlan,
    pub(super) subclass_axiom_closure: BTreeSet<(u32, u32, u32)>,
    pub(super) subproperty_axiom_closure: BTreeSet<(u32, u32, u32)>,
}

impl CachedPreparedSchema {
    pub(super) fn build(index: &IndexedDataset, policy: &RulesMvpFeaturePolicy) -> Self {
        let class_taxonomy = if policy.needs_class_taxonomy() {
            TaxonomyIndex::from_edges(index.subclass_edges())
        } else {
            TaxonomyIndex::default()
        };
        let class_consistency = if policy.owl_consistency_check_enabled() {
            PreparedClassConsistency::build(index, &class_taxonomy)
        } else {
            PreparedClassConsistency::default()
        };
        let property_taxonomy = if policy.needs_property_taxonomy() {
            PropertyTaxonomyIndex::from_edges(index.subproperty_edges())
        } else {
            PropertyTaxonomyIndex::default()
        };
        let property_chain_plan = if policy.needs_property_chain_plan() {
            PropertyChainPlan::build(index)
        } else {
            PropertyChainPlan::default()
        };
        let property_characteristic_plan =
            if policy.owl_consistency_check_enabled() || policy.owl_equality_reasoning_enabled() {
                PropertyCharacteristicPlan::build(index)
            } else {
                PropertyCharacteristicPlan::default()
            };
        let subclass_axiom_closure = if policy.rdfs_subclass_closure_enabled() {
            derive_subclass_axiom_closure(index, &class_taxonomy)
        } else {
            BTreeSet::new()
        };
        let subproperty_axiom_closure = if policy.rdfs_subproperty_closure_enabled() {
            derive_subproperty_axiom_closure(index, &property_taxonomy)
        } else {
            BTreeSet::new()
        };

        Self {
            class_taxonomy,
            class_consistency,
            property_taxonomy,
            property_chain_plan,
            property_characteristic_plan,
            subclass_axiom_closure,
            subproperty_axiom_closure,
        }
    }
}
