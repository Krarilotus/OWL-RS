use serde::Serialize;

use crate::reject_view::RejectExplanationView;

#[derive(Debug, Serialize)]
pub struct ReasoningDiagnosticsResponse {
    pub revision: u64,
    pub mode: &'static str,
    pub profile: &'static str,
    pub read_model: &'static str,
    pub capabilities: Vec<ReasoningCapabilityView>,
    pub configured_policy: Option<ConfiguredReasoningPolicyView>,
    pub last_run: Option<LastReasoningRunView>,
    pub reject_diagnostics: RejectDiagnosticsBaseline,
}

#[derive(Debug, Serialize)]
pub struct ReasoningCapabilityView {
    pub feature: &'static str,
    pub maturity: &'static str,
    pub enabled_by_default: bool,
}

#[derive(Debug, Serialize)]
pub struct ConfiguredReasoningPolicyView {
    pub preset: &'static str,
    pub semantic_tier: &'static str,
    pub available_presets: &'static [&'static str],
    pub feature_modes: Vec<ConfiguredFeatureView>,
    pub unsupported_constructs: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ConfiguredFeatureView {
    pub feature: &'static str,
    pub mode: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RejectDiagnosticsBaseline {
    pub available: bool,
    pub strategy: &'static str,
    pub last_reject_reason: Option<String>,
    pub last_reject: Option<RejectExplanationView>,
    pub hint: &'static str,
}

#[derive(Debug, Serialize)]
pub struct LastReasoningRunView {
    pub revision: u64,
    pub status: &'static str,
    pub inferred_triples: u64,
    pub consistency_violations: u64,
    pub stats: ReasoningStatsView,
    pub cache: ReasoningCacheView,
    pub notes: Vec<String>,
    pub diagnostics: Vec<String>,
    pub primary_reject: Option<RejectExplanationView>,
    pub likely_commit_trigger: Option<(String, String, String)>,
    pub derived_triples_sample: Vec<(String, String, String)>,
}

#[derive(Debug, Serialize)]
pub struct ReasoningStatsView {
    pub supported_asserted_triples: u64,
    pub unsupported_asserted_triples: u64,
    pub interned_terms: usize,
    pub subclass_edge_count: usize,
    pub subproperty_edge_count: usize,
    pub type_assertion_count: usize,
    pub property_assertion_count: usize,
    pub equality_assertion_count: usize,
    pub equality_cluster_count: usize,
    pub inferred_equality_link_count: usize,
    pub domain_assertion_count: usize,
    pub range_assertion_count: usize,
    pub taxonomy_node_count: usize,
    pub property_taxonomy_node_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ReasoningCacheView {
    pub execution_cache_hit: bool,
    pub schema_cache_hit: bool,
    pub execution_cache_entries: usize,
    pub schema_cache_entries: usize,
    pub execution_cache_capacity: usize,
    pub schema_cache_capacity: usize,
    pub execution_cache_hits_total: u64,
    pub execution_cache_misses_total: u64,
    pub schema_cache_hits_total: u64,
    pub schema_cache_misses_total: u64,
}
