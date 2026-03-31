use crate::reject_attribution::RejectAttribution;
use nrese_core::{ReasonerRunReport, ReasonerRunStatus};
use nrese_reasoner::{InferenceDelta, ReasoningCacheStats, ReasoningStats, RejectExplanation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastReasoningRun {
    pub revision: u64,
    pub status: ReasonerRunStatus,
    pub inferred_triples: u64,
    pub consistency_violations: u64,
    pub stats: ReasoningStats,
    pub cache: ReasoningCacheStats,
    pub notes: Vec<String>,
    pub diagnostics: Vec<String>,
    pub primary_reject: Option<RejectExplanation>,
    pub commit_attribution: Option<RejectAttribution>,
    pub derived_triples_sample: Vec<(String, String, String)>,
}

impl LastReasoningRun {
    pub fn from_report(
        report: &ReasonerRunReport,
        inferred: &InferenceDelta,
        commit_attribution: Option<RejectAttribution>,
    ) -> Self {
        Self {
            revision: report.revision,
            status: report.status,
            inferred_triples: inferred.inferred_triples,
            consistency_violations: inferred.consistency_violations,
            stats: inferred.stats.clone(),
            cache: inferred.cache.clone(),
            notes: report.notes.iter().map(|note| (*note).to_owned()).collect(),
            diagnostics: inferred.diagnostics.clone(),
            primary_reject: inferred.primary_reject.clone(),
            commit_attribution,
            derived_triples_sample: inferred.derived_triples.iter().take(5).cloned().collect(),
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
            .or_else(|| self.diagnostics.first().cloned())
            .or_else(|| self.notes.first().cloned())
    }
}
