use std::collections::BTreeSet;

use nrese_core::DatasetSnapshot;

use crate::class_consistency::{PreparedClassConsistency, detect_class_consistency_conflicts};
use crate::dataset_index::IndexedDataset;
use crate::effective_types::EffectiveTypeSet;
use crate::identity::{EqualityIndex, detect_different_from_conflicts, prepare_identity};
use crate::materialization::derive_rule_closure;
use crate::output::{InferenceDelta, ReasoningStats};
use crate::property_closure::PropertyClosure;
use crate::property_consistency::{
    PreparedPropertyAssertions, PropertyCharacteristicPlan,
    detect_property_characteristic_conflicts_prepared,
};
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;
use crate::support::collect_unsupported_construct_diagnostics;
use crate::taxonomy::TaxonomyIndex;

use super::schema::CachedPreparedSchema;

#[derive(Debug, Clone)]
pub(super) struct PreparedRulesMvp {
    index: IndexedDataset,
    class_taxonomy: TaxonomyIndex,
    class_consistency: PreparedClassConsistency,
    property_characteristic_plan: PropertyCharacteristicPlan,
    subclass_axiom_closure: BTreeSet<(u32, u32, u32)>,
    subproperty_axiom_closure: BTreeSet<(u32, u32, u32)>,
    property_closure: PropertyClosure,
    property_assertions: PreparedPropertyAssertions,
    effective_types: EffectiveTypeSet,
    equality: EqualityIndex,
    inferred_same_as_pairs: BTreeSet<(u32, u32)>,
    policy: RulesMvpFeaturePolicy,
    unsupported_diagnostics: Vec<String>,
    stats: ReasoningStats,
}

impl PreparedRulesMvp {
    pub(super) fn execute(&self) -> InferenceDelta {
        let mut derived = self.subclass_axiom_closure.clone();
        derived.extend(self.subproperty_axiom_closure.iter().copied());
        derived.extend(derive_rule_closure(
            &self.index,
            &self.class_taxonomy,
            &self.property_closure,
            &self.effective_types,
            &self.inferred_same_as_pairs,
            &self.policy,
        ));

        let mut consistency_violations = Vec::new();
        if self.policy.owl_consistency_check_enabled() {
            consistency_violations.extend(detect_class_consistency_conflicts(
                &self.index,
                &self.effective_types,
                &self.class_consistency,
            ));
            consistency_violations
                .extend(detect_different_from_conflicts(&self.index, &self.equality));
            consistency_violations.extend(detect_property_characteristic_conflicts_prepared(
                &self.index,
                &self.property_characteristic_plan,
                &self.property_assertions,
            ));
        }

        let primary_reject = consistency_violations
            .first()
            .map(|violation| violation.reject_explanation());
        let mut diagnostics = consistency_violations
            .iter()
            .map(|violation| violation.message.clone())
            .collect::<Vec<_>>();
        diagnostics.extend(self.unsupported_diagnostics.clone());
        if self.index.unsupported_asserted_triples() > 0 {
            diagnostics.push(format!(
                "{} asserted triples were skipped because rules-mvp currently supports only named-node subject/object triples",
                self.index.unsupported_asserted_triples()
            ));
        }

        let derived_triples = derived
            .into_iter()
            .filter_map(|(subject_id, predicate_id, object_id)| {
                Some((
                    self.index.symbols().resolve(subject_id)?.to_owned(),
                    self.index.symbols().resolve(predicate_id)?.to_owned(),
                    self.index.symbols().resolve(object_id)?.to_owned(),
                ))
            })
            .collect::<Vec<_>>();

        InferenceDelta {
            inferred_triples: derived_triples.len() as u64,
            consistency_violations: consistency_violations.len() as u64,
            derived_triples,
            diagnostics,
            primary_reject,
            stats: self.stats.clone(),
            cache: Default::default(),
        }
    }

    pub(super) fn build_from_index(
        index: IndexedDataset,
        cached_schema: Option<&CachedPreparedSchema>,
        policy: &RulesMvpFeaturePolicy,
    ) -> Self {
        let (
            class_taxonomy,
            class_consistency,
            property_taxonomy,
            property_chain_plan,
            property_characteristic_plan,
            subclass_axiom_closure,
            subproperty_axiom_closure,
        ) = match cached_schema {
            Some(schema) => (
                schema.class_taxonomy.clone(),
                schema.class_consistency.clone(),
                schema.property_taxonomy.clone(),
                schema.property_chain_plan.clone(),
                schema.property_characteristic_plan.clone(),
                schema.subclass_axiom_closure.clone(),
                schema.subproperty_axiom_closure.clone(),
            ),
            None => {
                let schema = CachedPreparedSchema::build(&index, policy);
                (
                    schema.class_taxonomy,
                    schema.class_consistency,
                    schema.property_taxonomy,
                    schema.property_chain_plan,
                    schema.property_characteristic_plan,
                    schema.subclass_axiom_closure,
                    schema.subproperty_axiom_closure,
                )
            }
        };

        let identity = prepare_identity(
            &index,
            &property_taxonomy,
            &property_chain_plan,
            &property_characteristic_plan,
            policy,
        );
        let equality = identity.equality().clone();
        let property_closure = PropertyClosure::build(
            &index,
            &property_taxonomy,
            &equality,
            &property_chain_plan,
            policy,
        );
        let property_assertions =
            PreparedPropertyAssertions::build(&property_characteristic_plan, &property_closure);
        let effective_types = EffectiveTypeSet::build(
            &index,
            &class_taxonomy,
            &property_closure,
            &equality,
            policy.rdfs_domain_range_typing_enabled(),
        );
        let unsupported_diagnostics = collect_unsupported_construct_diagnostics(&index, policy);
        let mut unsupported_diagnostics = unsupported_diagnostics;
        if policy.unsupported_construct_diagnostics_enabled() {
            unsupported_diagnostics.extend(property_chain_plan.diagnostics().iter().cloned());
        }
        let stats = ReasoningStats {
            supported_asserted_triples: index.supported_asserted_triples(),
            unsupported_asserted_triples: index.unsupported_asserted_triples(),
            interned_terms: index.symbols().len(),
            subclass_edge_count: index.subclass_edge_count(),
            subproperty_edge_count: index.subproperty_edge_count(),
            type_assertion_count: index.type_assertion_count(),
            property_assertion_count: index.property_assertion_count(),
            equality_assertion_count: if policy.owl_equality_reasoning_enabled() {
                equality.assertion_count()
            } else {
                index.same_as_assertion_count()
            },
            equality_cluster_count: equality.cluster_count(),
            inferred_equality_link_count: identity.inferred_same_as_pairs().len(),
            domain_assertion_count: index.domain_assertion_count(),
            range_assertion_count: index.range_assertion_count(),
            taxonomy_node_count: class_taxonomy.node_count(),
            property_taxonomy_node_count: property_taxonomy.node_count(),
        };

        Self {
            index,
            class_taxonomy,
            class_consistency,
            property_characteristic_plan,
            subclass_axiom_closure,
            subproperty_axiom_closure,
            property_closure,
            property_assertions,
            effective_types,
            equality,
            inferred_same_as_pairs: identity.inferred_same_as_pairs().clone(),
            unsupported_diagnostics,
            stats,
            policy: *policy,
        }
    }
}

pub(super) fn build_prepared<'a, S>(
    snapshot: &'a S,
    cached_schema: Option<&CachedPreparedSchema>,
    policy: &RulesMvpFeaturePolicy,
) -> PreparedRulesMvp
where
    S: DatasetSnapshot<'a>,
{
    PreparedRulesMvp::build_from_index(
        IndexedDataset::from_snapshot(snapshot),
        cached_schema,
        policy,
    )
}
