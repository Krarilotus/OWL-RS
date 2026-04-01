use std::collections::BTreeSet;

use crate::dataset_index::IndexedDataset;
use crate::property_chain::PropertyChainPlan;
use crate::property_closure::PropertyClosure;
use crate::property_consistency::{PreparedPropertyAssertions, PropertyCharacteristicPlan};
use crate::property_taxonomy::PropertyTaxonomyIndex;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;

use super::EqualityIndex;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct IdentityPreparation {
    equality: EqualityIndex,
    inferred_same_as_pairs: BTreeSet<(u32, u32)>,
    iterations: usize,
}

impl IdentityPreparation {
    pub(crate) fn equality(&self) -> &EqualityIndex {
        &self.equality
    }

    pub(crate) fn inferred_same_as_pairs(&self) -> &BTreeSet<(u32, u32)> {
        &self.inferred_same_as_pairs
    }
}

pub(crate) fn prepare_identity(
    index: &IndexedDataset,
    property_taxonomy: &PropertyTaxonomyIndex,
    property_chain_plan: &PropertyChainPlan,
    plan: &PropertyCharacteristicPlan,
    policy: &RulesMvpFeaturePolicy,
) -> IdentityPreparation {
    if !policy.owl_equality_reasoning_enabled() {
        return IdentityPreparation::default();
    }

    let mut equality = EqualityIndex::build(index);
    let mut inferred_same_as_pairs = BTreeSet::new();
    let mut iterations = 0;

    loop {
        let property_closure = PropertyClosure::build(
            index,
            property_taxonomy,
            &equality,
            property_chain_plan,
            policy,
        );
        let prepared = PreparedPropertyAssertions::build(plan, &property_closure);
        let next_pairs = derive_property_implied_same_as_pairs(plan, &prepared, &equality);
        if next_pairs.is_empty() {
            return IdentityPreparation {
                equality,
                inferred_same_as_pairs,
                iterations,
            };
        }

        inferred_same_as_pairs.extend(next_pairs);
        let next_equality = EqualityIndex::build_with_inferred(index, &inferred_same_as_pairs);
        if next_equality == equality {
            return IdentityPreparation {
                equality: next_equality,
                inferred_same_as_pairs,
                iterations,
            };
        }

        equality = next_equality;
        iterations += 1;
    }
}

fn derive_property_implied_same_as_pairs(
    plan: &PropertyCharacteristicPlan,
    prepared: &PreparedPropertyAssertions,
    equality: &EqualityIndex,
) -> BTreeSet<(u32, u32)> {
    let mut inferred_pairs = BTreeSet::new();

    for (&(predicate_id, _subject_id), object_ids) in
        prepared.outgoing_objects_by_property_subject()
    {
        if !plan.functional_properties().contains(&predicate_id) || object_ids.len() < 2 {
            continue;
        }
        collect_implied_pairs(object_ids, equality, &mut inferred_pairs);
    }

    for (&(predicate_id, _object_id), subject_ids) in
        prepared.incoming_subjects_by_property_object()
    {
        if !plan.inverse_functional_properties().contains(&predicate_id) || subject_ids.len() < 2 {
            continue;
        }
        collect_implied_pairs(subject_ids, equality, &mut inferred_pairs);
    }

    inferred_pairs
}

fn collect_implied_pairs(
    members: &BTreeSet<u32>,
    equality: &EqualityIndex,
    inferred_pairs: &mut BTreeSet<(u32, u32)>,
) {
    let mut member_ids = members.iter().copied();
    let Some(anchor_id) = member_ids.next() else {
        return;
    };

    for member_id in member_ids {
        if equality.are_equivalent(anchor_id, member_id) {
            continue;
        }
        inferred_pairs.insert(ordered_pair(anchor_id, member_id));
    }
}

fn ordered_pair(left: u32, right: u32) -> (u32, u32) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

#[cfg(test)]
#[path = "../tests/identity_inference_tests.rs"]
mod tests;
