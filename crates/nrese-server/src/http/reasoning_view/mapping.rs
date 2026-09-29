use crate::reject_view::reject_view;
use nrese_store::ReasoningRunRecord;

use super::types::{LastReasoningRunView, ReasoningCapabilityView, RejectDiagnosticsBaseline};

pub fn capability_view(capability: &nrese_core::ReasonerCapability) -> ReasoningCapabilityView {
    ReasoningCapabilityView {
        feature: feature_name(capability.feature),
        maturity: maturity_name(capability.maturity),
        enabled_by_default: capability.enabled_by_default,
    }
}

pub fn last_run_view(run: &ReasoningRunRecord) -> LastReasoningRunView {
    LastReasoningRunView {
        revision: run.revision,
        status: run_status_name(run.status),
        ruleset: run.ruleset,
        inferred_triples: run.inferred_triples,
        inferred_inserted: run.inferred_inserted,
        inferred_deleted: run.inferred_deleted,
        consistency_violations: run.consistency_violations,
        rounds: run.rounds,
        elapsed_micros: run.elapsed_micros,
        primary_reject: run
            .primary_reject
            .as_ref()
            .map(|reject| reject_view(reject, run.commit_attribution.as_ref())),
        likely_commit_trigger: run.likely_commit_trigger(),
    }
}

pub fn reject_diagnostics_baseline(
    last_run: Option<&ReasoningRunRecord>,
) -> RejectDiagnosticsBaseline {
    RejectDiagnosticsBaseline {
        available: last_run.is_some(),
        strategy: "rule-premises-plus-commit-delta-attribution",
        last_reject_reason: last_run.and_then(ReasoningRunRecord::primary_reject_reason),
        last_reject: last_run.and_then(|run| {
            run.primary_reject
                .as_ref()
                .map(|reject| reject_view(reject, run.commit_attribution.as_ref()))
        }),
        hint: "Reject diagnostics show the violated OWL 2 RL rule with the facts it matched (asserted or inferred) and the commit's likely trigger.",
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
