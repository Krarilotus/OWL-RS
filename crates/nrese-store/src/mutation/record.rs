use nrese_core::ReasonerRunStatus;
use nrese_reasoner::RejectExplanation;

use super::attribution::RejectAttribution;
use crate::reasoning::MaterialisationReport;

/// One commit-path reasoning run (reasoner v2), kept for operator diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningRunRecord {
    pub revision: u64,
    pub status: ReasonerRunStatus,
    pub ruleset: &'static str,
    /// Inferred statements after the run, and those it added and removed.
    pub inferred_triples: u64,
    pub inferred_inserted: u64,
    pub inferred_deleted: u64,
    pub consistency_violations: u64,
    pub rounds: usize,
    pub elapsed_micros: u64,
    pub primary_reject: Option<RejectExplanation>,
    pub commit_attribution: Option<RejectAttribution>,
}

impl ReasoningRunRecord {
    pub fn from_report(
        report: &MaterialisationReport,
        primary_reject: Option<RejectExplanation>,
        commit_attribution: Option<RejectAttribution>,
    ) -> Self {
        Self {
            revision: report.revision,
            status: if report.violations > 0 {
                ReasonerRunStatus::Rejected
            } else {
                ReasonerRunStatus::Completed
            },
            ruleset: report.ruleset,
            inferred_triples: report.inferred,
            inferred_inserted: report.inferred_inserted,
            inferred_deleted: report.inferred_deleted,
            consistency_violations: report.violations as u64,
            rounds: report.rounds,
            elapsed_micros: u64::try_from(report.elapsed.as_micros()).unwrap_or(u64::MAX),
            primary_reject,
            commit_attribution,
        }
    }

    pub fn likely_commit_trigger(&self) -> Option<(String, String, String)> {
        self.commit_attribution
            .as_ref()
            .and_then(RejectAttribution::likely_commit_trigger)
    }

    pub fn primary_reject_reason(&self) -> Option<String> {
        self.primary_reject
            .as_ref()
            .map(|reject| reject.summary.clone())
    }
}
