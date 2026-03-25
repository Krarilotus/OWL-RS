use std::collections::BTreeSet;

use crate::dataset_index::IndexedDataset;
use crate::effective_types::EffectiveTypeSet;
use crate::property_closure::PropertyClosure;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;
use crate::taxonomy::TaxonomyIndex;

pub fn derive_rule_closure(
    index: &IndexedDataset,
    class_taxonomy: &TaxonomyIndex,
    property_closure: &PropertyClosure,
    effective_types: &EffectiveTypeSet,
    inferred_same_as_pairs: &BTreeSet<(u32, u32)>,
    policy: &RulesMvpFeaturePolicy,
) -> BTreeSet<(u32, u32, u32)> {
    let mut derived = BTreeSet::new();

    if policy.rdfs_subclass_closure_enabled() {
        derived.extend(derive_subclass_axiom_closure(index, class_taxonomy));
    }
    derive_property_closure(property_closure, &mut derived);
    if policy.rdfs_type_propagation_enabled() {
        derive_effective_types(index, effective_types, &mut derived);
    }
    if policy.owl_equality_reasoning_enabled() {
        derive_inferred_same_as(index, inferred_same_as_pairs, &mut derived);
    }

    derived
}

pub fn derive_subclass_axiom_closure(
    index: &IndexedDataset,
    class_taxonomy: &TaxonomyIndex,
) -> BTreeSet<(u32, u32, u32)> {
    let mut derived = BTreeSet::new();

    for &class_id in index.subclass_edges().keys() {
        if let Some(ancestors) = class_taxonomy.ancestors_of(class_id) {
            for &ancestor_id in ancestors {
                let candidate = (class_id, index.rdfs_subclass_of_id(), ancestor_id);
                if !index.asserted_triples().contains(&candidate) {
                    derived.insert(candidate);
                }
            }
        }
    }

    derived
}

fn derive_property_closure(
    property_closure: &PropertyClosure,
    derived: &mut BTreeSet<(u32, u32, u32)>,
) {
    derived.extend(property_closure.derived_assertions().iter().copied());
}

pub fn derive_subproperty_axiom_closure(
    index: &IndexedDataset,
    property_taxonomy: &crate::property_taxonomy::PropertyTaxonomyIndex,
) -> BTreeSet<(u32, u32, u32)> {
    let mut derived = BTreeSet::new();

    for &property_id in index.subproperty_edges().keys() {
        if let Some(ancestors) = property_taxonomy.ancestors_of(property_id) {
            for &ancestor_id in ancestors {
                let candidate = (property_id, index.rdfs_subproperty_of_id(), ancestor_id);
                if !index.asserted_triples().contains(&candidate) {
                    derived.insert(candidate);
                }
            }
        }
    }

    derived
}

fn derive_effective_types(
    index: &IndexedDataset,
    effective_types: &EffectiveTypeSet,
    derived: &mut BTreeSet<(u32, u32, u32)>,
) {
    for (&instance_id, class_map) in effective_types.instances() {
        for &class_id in class_map.keys() {
            let candidate = (instance_id, index.rdf_type_id(), class_id);
            if !index.asserted_triples().contains(&candidate) {
                derived.insert(candidate);
            }
        }
    }
}

fn derive_inferred_same_as(
    index: &IndexedDataset,
    inferred_same_as_pairs: &BTreeSet<(u32, u32)>,
    derived: &mut BTreeSet<(u32, u32, u32)>,
) {
    for &(left_id, right_id) in inferred_same_as_pairs {
        let forward = (left_id, index.owl_same_as_id(), right_id);
        let reverse = (right_id, index.owl_same_as_id(), left_id);
        if index.asserted_triples().contains(&forward)
            || index.asserted_triples().contains(&reverse)
        {
            continue;
        }
        derived.insert(forward);
    }
}
