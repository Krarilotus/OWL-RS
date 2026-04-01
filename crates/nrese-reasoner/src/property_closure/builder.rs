use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::dataset_index::IndexedDataset;
use crate::identity::EqualityIndex;
use crate::property_chain::PropertyChainPlan;
use crate::property_taxonomy::PropertyTaxonomyIndex;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;

use super::expansion::for_each_equality_expansion;
use super::{PropertyClosure, insert_assertion};

type AdjacencyKey = (u32, u32);
type AdjacencyMap = HashMap<AdjacencyKey, BTreeSet<u32>>;
type PropertyTriple = (u32, u32, u32);

#[derive(Debug, Default)]
pub(super) struct PropertyClosureBuilder {
    closure: PropertyClosure,
    queue: VecDeque<PropertyTriple>,
    outgoing: AdjacencyMap,
    incoming: AdjacencyMap,
}

impl PropertyClosureBuilder {
    pub(super) fn build(
        index: &IndexedDataset,
        property_taxonomy: &PropertyTaxonomyIndex,
        equality: &EqualityIndex,
        property_chain_plan: &PropertyChainPlan,
        policy: &RulesMvpFeaturePolicy,
    ) -> PropertyClosure {
        let mut builder = Self::default();

        for &(subject_id, predicate_id, object_id) in index.property_assertions() {
            builder.enqueue_assertion(
                (subject_id, predicate_id, object_id),
                false,
                equality,
                policy,
            );
        }
        builder.seed_reflexive_assertions(index, equality, policy);

        while let Some((subject_id, predicate_id, object_id)) = builder.queue.pop_front() {
            builder.expand_subproperties(
                subject_id,
                predicate_id,
                object_id,
                property_taxonomy,
                equality,
                policy,
            );
            builder.expand_inverses(subject_id, predicate_id, object_id, index, equality, policy);
            builder.expand_symmetric(subject_id, predicate_id, object_id, index, equality, policy);
            builder.expand_transitive(subject_id, predicate_id, object_id, index, equality, policy);
            builder.expand_property_chains(
                subject_id,
                predicate_id,
                object_id,
                property_chain_plan,
                equality,
                policy,
            );
        }

        builder.closure
    }

    fn seed_reflexive_assertions(
        &mut self,
        index: &IndexedDataset,
        equality: &EqualityIndex,
        policy: &RulesMvpFeaturePolicy,
    ) {
        if !policy.owl_property_chain_axioms_enabled() {
            return;
        }

        for &predicate_id in index.reflexive_properties() {
            for &resource_id in index.observed_named_resources() {
                self.enqueue_assertion(
                    (resource_id, predicate_id, resource_id),
                    true,
                    equality,
                    policy,
                );
            }
        }
    }

    fn expand_subproperties(
        &mut self,
        subject_id: u32,
        predicate_id: u32,
        object_id: u32,
        property_taxonomy: &PropertyTaxonomyIndex,
        equality: &EqualityIndex,
        policy: &RulesMvpFeaturePolicy,
    ) {
        if !policy.rdfs_subproperty_closure_enabled() {
            return;
        }

        if let Some(ancestors) = property_taxonomy.ancestors_of(predicate_id) {
            for &ancestor_id in ancestors {
                self.enqueue_assertion(
                    (subject_id, ancestor_id, object_id),
                    true,
                    equality,
                    policy,
                );
            }
        }
    }

    fn expand_inverses(
        &mut self,
        subject_id: u32,
        predicate_id: u32,
        object_id: u32,
        index: &IndexedDataset,
        equality: &EqualityIndex,
        policy: &RulesMvpFeaturePolicy,
    ) {
        if !policy.owl_property_assertion_closure_enabled() {
            return;
        }

        if let Some(inverses) = index.inverse_of_pairs().get(&predicate_id) {
            for &inverse_id in inverses {
                self.enqueue_assertion((object_id, inverse_id, subject_id), true, equality, policy);
            }
        }
    }

    fn expand_symmetric(
        &mut self,
        subject_id: u32,
        predicate_id: u32,
        object_id: u32,
        index: &IndexedDataset,
        equality: &EqualityIndex,
        policy: &RulesMvpFeaturePolicy,
    ) {
        if !policy.owl_property_assertion_closure_enabled() {
            return;
        }

        if index.symmetric_properties().contains(&predicate_id) {
            self.enqueue_assertion(
                (object_id, predicate_id, subject_id),
                true,
                equality,
                policy,
            );
        }
    }

    fn expand_transitive(
        &mut self,
        subject_id: u32,
        predicate_id: u32,
        object_id: u32,
        index: &IndexedDataset,
        equality: &EqualityIndex,
        policy: &RulesMvpFeaturePolicy,
    ) {
        if !policy.owl_property_assertion_closure_enabled() {
            return;
        }

        if !index.transitive_properties().contains(&predicate_id) {
            return;
        }

        if let Some(next_objects) = self.outgoing.get(&(predicate_id, object_id)).cloned() {
            for next_object_id in next_objects {
                self.enqueue_assertion(
                    (subject_id, predicate_id, next_object_id),
                    true,
                    equality,
                    policy,
                );
            }
        }

        if let Some(previous_subjects) = self.incoming.get(&(predicate_id, subject_id)).cloned() {
            for previous_subject_id in previous_subjects {
                self.enqueue_assertion(
                    (previous_subject_id, predicate_id, object_id),
                    true,
                    equality,
                    policy,
                );
            }
        }
    }

    fn expand_property_chains(
        &mut self,
        subject_id: u32,
        predicate_id: u32,
        object_id: u32,
        property_chain_plan: &PropertyChainPlan,
        equality: &EqualityIndex,
        policy: &RulesMvpFeaturePolicy,
    ) {
        if !policy.owl_property_assertion_closure_enabled() {
            return;
        }

        if let Some(chains) = property_chain_plan.chains_starting_with(predicate_id) {
            for chain in chains {
                if let Some(next_objects) = self
                    .outgoing
                    .get(&(chain.second_property_id, object_id))
                    .cloned()
                {
                    for next_object_id in next_objects {
                        self.enqueue_assertion(
                            (subject_id, chain.super_property_id, next_object_id),
                            true,
                            equality,
                            policy,
                        );
                    }
                }
            }
        }

        if let Some(chains) = property_chain_plan.chains_ending_with(predicate_id) {
            for chain in chains {
                if let Some(previous_subjects) = self
                    .incoming
                    .get(&(chain.first_property_id, subject_id))
                    .cloned()
                {
                    for previous_subject_id in previous_subjects {
                        self.enqueue_assertion(
                            (previous_subject_id, chain.super_property_id, object_id),
                            true,
                            equality,
                            policy,
                        );
                    }
                }
            }
        }
    }

    fn enqueue_assertion(
        &mut self,
        triple: PropertyTriple,
        derived: bool,
        equality: &EqualityIndex,
        policy: &RulesMvpFeaturePolicy,
    ) {
        let (subject_id, predicate_id, object_id) = triple;
        for_each_equality_expansion(
            equality,
            subject_id,
            object_id,
            policy.owl_equality_reasoning_enabled(),
            |expanded_subject, expanded_object| {
                let expanded_triple = (expanded_subject, predicate_id, expanded_object);
                let expanded_is_derived =
                    derived || expanded_subject != subject_id || expanded_object != object_id;

                if !insert_assertion(&mut self.closure, expanded_triple, expanded_is_derived) {
                    return;
                }

                self.outgoing
                    .entry((predicate_id, expanded_subject))
                    .or_default()
                    .insert(expanded_object);
                self.incoming
                    .entry((predicate_id, expanded_object))
                    .or_default()
                    .insert(expanded_subject);
                self.queue.push_back(expanded_triple);
            },
        );
    }
}
