use crate::reject_attribution::{RejectAttribution, RejectAttributionCandidate};
use nrese_reasoner::RejectExplanation;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RejectExplanationView {
    pub summary: String,
    pub violated_constraint: String,
    pub focus_resource: String,
    pub primary_conflicting_term: String,
    pub secondary_conflicting_term: String,
    pub likely_commit_trigger: Option<(String, String, String)>,
    pub commit_attribution: Option<RejectAttributionView>,
    pub heuristic: &'static str,
    pub principal_contributor: String,
    pub principal_origin: String,
    pub contextual_contributor: String,
    pub contextual_origin: String,
    pub evidence: Vec<RejectEvidenceView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectEvidenceView {
    pub role: &'static str,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectAttributionView {
    pub heuristic: &'static str,
    pub candidates: Vec<RejectAttributionCandidateView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectAttributionCandidateView {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub score: u16,
    pub matched_evidence_roles: Vec<&'static str>,
    pub match_reasons: Vec<&'static str>,
}

pub fn reject_view(
    reject: &RejectExplanation,
    attribution: Option<&RejectAttribution>,
) -> RejectExplanationView {
    RejectExplanationView {
        summary: reject.summary.clone(),
        violated_constraint: reject.violated_constraint.clone(),
        focus_resource: reject.focus_resource.clone(),
        primary_conflicting_term: reject.primary_conflicting_term.clone(),
        secondary_conflicting_term: reject.secondary_conflicting_term.clone(),
        likely_commit_trigger: attribution.and_then(RejectAttribution::likely_commit_trigger),
        commit_attribution: attribution.map(attribution_view),
        heuristic: reject.blame.heuristic,
        principal_contributor: reject.blame.principal_contributor.clone(),
        principal_origin: reject.blame.principal_origin.clone(),
        contextual_contributor: reject.blame.contextual_contributor.clone(),
        contextual_origin: reject.blame.contextual_origin.clone(),
        evidence: reject
            .evidence
            .iter()
            .map(|evidence| RejectEvidenceView {
                role: evidence.role,
                subject: evidence.subject.clone(),
                predicate: evidence.predicate.clone(),
                object: evidence.object.clone(),
                origin: evidence.origin.clone(),
            })
            .collect(),
    }
}

fn attribution_view(attribution: &RejectAttribution) -> RejectAttributionView {
    RejectAttributionView {
        heuristic: attribution.heuristic,
        candidates: attribution
            .candidates
            .iter()
            .map(attribution_candidate_view)
            .collect(),
    }
}

fn attribution_candidate_view(
    candidate: &RejectAttributionCandidate,
) -> RejectAttributionCandidateView {
    RejectAttributionCandidateView {
        subject: candidate.subject.clone(),
        predicate: candidate.predicate.clone(),
        object: candidate.object.clone(),
        score: candidate.score,
        matched_evidence_roles: candidate.matched_evidence_roles.clone(),
        match_reasons: candidate.match_reasons.clone(),
    }
}
