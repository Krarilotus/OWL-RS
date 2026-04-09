use crate::reasoning_runtime::LastReasoningRun;
use crate::reject_view::reject_view;

use super::types::{
    ConfiguredFeatureView, ConfiguredReasoningPolicyView, LastReasoningRunView, ReasoningCacheView,
    ReasoningCapabilityView, ReasoningStatsView, RejectDiagnosticsBaseline,
};

pub fn capability_view(capability: &nrese_core::ReasonerCapability) -> ReasoningCapabilityView {
    ReasoningCapabilityView {
        feature: feature_name(capability.feature),
        maturity: maturity_name(capability.maturity),
        enabled_by_default: capability.enabled_by_default,
    }
}

pub fn configured_reasoning_policy(
    service: &nrese_reasoner::ReasonerService,
) -> Option<ConfiguredReasoningPolicyView> {
    if service.config().mode() != nrese_reasoner::ReasoningMode::RulesMvp {
        return None;
    }

    let policy = service.rules_mvp_feature_policy()?;
    Some(ConfiguredReasoningPolicyView {
        preset: service.rules_mvp_preset().as_str(),
        semantic_tier: service.semantic_tier(),
        available_presets: nrese_reasoner::RulesMvpPreset::available(),
        feature_modes: vec![
            configured_feature("rdfs-subclass-closure", policy.rdfs_subclass_closure),
            configured_feature("rdfs-subproperty-closure", policy.rdfs_subproperty_closure),
            configured_feature("rdfs-type-propagation", policy.rdfs_type_propagation),
            configured_feature("rdfs-domain-range-typing", policy.rdfs_domain_range_typing),
            configured_feature(
                "owl-property-assertion-closure",
                policy.owl_property_assertion_closure,
            ),
            configured_feature("owl-equality-reasoning", policy.owl_equality_reasoning),
            configured_feature(
                "owl-property-chain-axioms",
                policy.owl_property_chain_axioms,
            ),
            configured_feature("owl-consistency-check", policy.owl_consistency_check),
        ],
        unsupported_constructs: unsupported_construct_behavior_name(policy.unsupported_constructs),
    })
}

pub fn last_run_view(run: &LastReasoningRun) -> LastReasoningRunView {
    LastReasoningRunView {
        revision: run.revision,
        status: run_status_name(run.status),
        inferred_triples: run.inferred_triples,
        consistency_violations: run.consistency_violations,
        stats: ReasoningStatsView {
            supported_asserted_triples: run.stats.supported_asserted_triples,
            unsupported_asserted_triples: run.stats.unsupported_asserted_triples,
            unsupported_blank_node_subjects: run.stats.unsupported_blank_node_subjects,
            unsupported_blank_node_objects: run.stats.unsupported_blank_node_objects,
            unsupported_literal_objects: run.stats.unsupported_literal_objects,
            flattened_named_graph_quads: run.stats.flattened_named_graph_quads,
            interned_terms: run.stats.interned_terms,
            subclass_edge_count: run.stats.subclass_edge_count,
            subproperty_edge_count: run.stats.subproperty_edge_count,
            type_assertion_count: run.stats.type_assertion_count,
            property_assertion_count: run.stats.property_assertion_count,
            equality_assertion_count: run.stats.equality_assertion_count,
            equality_cluster_count: run.stats.equality_cluster_count,
            inferred_equality_link_count: run.stats.inferred_equality_link_count,
            domain_assertion_count: run.stats.domain_assertion_count,
            range_assertion_count: run.stats.range_assertion_count,
            taxonomy_node_count: run.stats.taxonomy_node_count,
            property_taxonomy_node_count: run.stats.property_taxonomy_node_count,
        },
        cache: ReasoningCacheView {
            execution_cache_hit: run.cache.execution_cache_hit,
            schema_cache_hit: run.cache.schema_cache_hit,
            execution_cache_entries: run.cache.execution_cache_entries,
            schema_cache_entries: run.cache.schema_cache_entries,
            execution_cache_capacity: run.cache.execution_cache_capacity,
            schema_cache_capacity: run.cache.schema_cache_capacity,
            execution_cache_hits_total: run.cache.execution_cache_hits_total,
            execution_cache_misses_total: run.cache.execution_cache_misses_total,
            schema_cache_hits_total: run.cache.schema_cache_hits_total,
            schema_cache_misses_total: run.cache.schema_cache_misses_total,
        },
        notes: run.notes.clone(),
        diagnostics: run.diagnostics.clone(),
        primary_reject: run
            .primary_reject
            .as_ref()
            .map(|reject| reject_view(reject, run.commit_attribution.as_ref())),
        likely_commit_trigger: run.likely_commit_trigger(),
        derived_triples_sample: run.derived_triples_sample.clone(),
    }
}

pub fn reject_diagnostics_baseline(
    last_run: Option<&LastReasoningRun>,
) -> RejectDiagnosticsBaseline {
    RejectDiagnosticsBaseline {
        available: last_run.is_some(),
        strategy: "hybrid-heuristic-plus-deep-justification",
        last_reject_reason: last_run.and_then(LastReasoningRun::primary_reject_reason),
        last_reject: last_run.and_then(|run| {
            run.primary_reject
                .as_ref()
                .map(|reject| reject_view(reject, run.commit_attribution.as_ref()))
        }),
        hint: "Reject diagnostics expose the latest commit-path reasoning result; deeper justifications remain a later milestone.",
    }
}

fn configured_feature(
    feature: &'static str,
    mode: nrese_reasoner::FeatureMode,
) -> ConfiguredFeatureView {
    ConfiguredFeatureView {
        feature,
        mode: feature_mode_name(mode),
    }
}

fn run_status_name(status: nrese_core::ReasonerRunStatus) -> &'static str {
    match status {
        nrese_core::ReasonerRunStatus::Completed => "completed",
        nrese_core::ReasonerRunStatus::Skipped => "skipped",
        nrese_core::ReasonerRunStatus::Rejected => "rejected",
    }
}

fn feature_name(feature: nrese_core::ReasonerFeature) -> &'static str {
    match feature {
        nrese_core::ReasonerFeature::RdfsSubclassClosure => "rdfs-subclass-closure",
        nrese_core::ReasonerFeature::RdfsSubpropertyClosure => "rdfs-subproperty-closure",
        nrese_core::ReasonerFeature::RdfsTypePropagation => "rdfs-type-propagation",
        nrese_core::ReasonerFeature::RdfsDomainRangeTyping => "rdfs-domain-range-typing",
        nrese_core::ReasonerFeature::OwlEqualityReasoning => "owl-equality-reasoning",
        nrese_core::ReasonerFeature::OwlPropertyChainAxioms => "owl-property-chain-axioms",
        nrese_core::ReasonerFeature::OwlClassSatisfiability => "owl-class-satisfiability",
        nrese_core::ReasonerFeature::OwlConsistencyCheck => "owl-consistency-check",
        nrese_core::ReasonerFeature::IncrementalRefresh => "incremental-refresh",
        nrese_core::ReasonerFeature::ExplanationTrace => "explanation-trace",
    }
}

fn maturity_name(maturity: nrese_core::CapabilityMaturity) -> &'static str {
    match maturity {
        nrese_core::CapabilityMaturity::Experimental => "experimental",
        nrese_core::CapabilityMaturity::Mvp => "mvp",
        nrese_core::CapabilityMaturity::Target => "target",
    }
}

fn feature_mode_name(mode: nrese_reasoner::FeatureMode) -> &'static str {
    match mode {
        nrese_reasoner::FeatureMode::Disabled => "disabled",
        nrese_reasoner::FeatureMode::Enabled => "enabled",
    }
}

fn unsupported_construct_behavior_name(
    behavior: nrese_reasoner::UnsupportedConstructBehavior,
) -> &'static str {
    match behavior {
        nrese_reasoner::UnsupportedConstructBehavior::Ignore => "ignore",
        nrese_reasoner::UnsupportedConstructBehavior::Diagnose => "diagnose",
    }
}
