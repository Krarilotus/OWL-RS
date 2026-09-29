use serde::Serialize;

use crate::reject_view::RejectExplanationView;

#[derive(Debug, Serialize)]
pub struct ReasoningDiagnosticsResponse {
    pub revision: u64,
    pub mode: &'static str,
    pub profile: &'static str,
    pub read_model: &'static str,
    pub capabilities: Vec<ReasoningCapabilityView>,
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
pub struct RejectDiagnosticsBaseline {
    pub available: bool,
    pub strategy: &'static str,
    pub last_reject_reason: Option<String>,
    pub last_reject: Option<RejectExplanationView>,
    pub hint: &'static str,
}

/// The latest commit-path reasoning run (reasoner v2's delta executor).
#[derive(Debug, Serialize)]
pub struct LastReasoningRunView {
    pub revision: u64,
    pub status: &'static str,
    pub ruleset: &'static str,
    pub inferred_triples: u64,
    pub inferred_inserted: u64,
    pub inferred_deleted: u64,
    pub consistency_violations: u64,
    pub rounds: usize,
    pub elapsed_micros: u64,
    pub primary_reject: Option<RejectExplanationView>,
    pub likely_commit_trigger: Option<(String, String, String)>,
}
