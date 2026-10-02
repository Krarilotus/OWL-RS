//! Heuristic attribution of a gate rejection to the triples a mutation inserted.

use crate::delta::MutationDeltaPreview;
use nrese_reasoner::{RejectEvidence, RejectExplanation};

const MAX_ATTRIBUTION_CANDIDATES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectAttribution {
    pub heuristic: &'static str,
    pub candidates: Vec<RejectAttributionCandidate>,
}

impl RejectAttribution {
    pub fn likely_commit_trigger(&self) -> Option<(String, String, String)> {
        self.candidates
            .first()
            .map(RejectAttributionCandidate::triple)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectAttributionCandidate {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub score: u16,
    pub matched_evidence_roles: Vec<&'static str>,
    pub match_reasons: Vec<&'static str>,
}

impl RejectAttributionCandidate {
    pub fn triple(&self) -> (String, String, String) {
        (
            self.subject.clone(),
            self.predicate.clone(),
            self.object.clone(),
        )
    }
}

pub fn attribute_reject_delta(
    reject: &RejectExplanation,
    delta: &MutationDeltaPreview,
) -> Option<RejectAttribution> {
    let mut candidates = delta
        .inserted_triples
        .iter()
        .filter_map(|triple| score_triple(reject, triple))
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.subject.cmp(&right.subject))
            .then_with(|| left.predicate.cmp(&right.predicate))
            .then_with(|| left.object.cmp(&right.object))
    });
    candidates.truncate(MAX_ATTRIBUTION_CANDIDATES);

    if candidates.is_empty() {
        None
    } else {
        Some(RejectAttribution {
            heuristic: "reasoner-evidence-plus-commit-delta-scoring",
            candidates,
        })
    }
}

fn score_triple(
    reject: &RejectExplanation,
    triple: &(String, String, String),
) -> Option<RejectAttributionCandidate> {
    let (subject, predicate, object) = triple;
    let mut score = 0u16;
    let mut matched_evidence_roles = Vec::new();
    let mut match_reasons = Vec::new();

    for evidence in &reject.evidence {
        if triple_matches_evidence(triple, evidence) {
            score += score_for_evidence_role(evidence.role);
            matched_evidence_roles.push(evidence.role);
        }
    }

    if subject == &reject.focus_resource {
        score += 100;
        match_reasons.push("focus-resource-subject");
    }
    if object == &reject.focus_resource {
        score += 90;
        match_reasons.push("focus-resource-object");
    }
    dedup_sorted(&mut matched_evidence_roles);
    dedup_sorted(&mut match_reasons);

    if score == 0 {
        None
    } else {
        Some(RejectAttributionCandidate {
            subject: subject.clone(),
            predicate: predicate.clone(),
            object: object.clone(),
            score,
            matched_evidence_roles,
            match_reasons,
        })
    }
}

fn triple_matches_evidence(triple: &(String, String, String), evidence: &RejectEvidence) -> bool {
    triple.0 == evidence.subject && triple.1 == evidence.predicate && triple.2 == evidence.object
}

fn score_for_evidence_role(role: &str) -> u16 {
    match role {
        // A fact the violated rule matched: the commit's own statement is the trigger.
        "premise" => 300,
        _ => 150,
    }
}

fn dedup_sorted(values: &mut Vec<&'static str>) {
    values.sort_unstable();
    values.dedup();
}

#[cfg(test)]
mod tests {
    use crate::delta::MutationDeltaPreview;
    use nrese_reasoner::{RejectEvidence, RejectExplanation};

    use super::attribute_reject_delta;

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

    #[test]
    fn attribution_prefers_evidence_backed_conflicting_assertion() {
        let reject = RejectExplanation {
            summary: "reject".to_owned(),
            violated_constraint: "cax-dw".to_owned(),
            focus_resource: "http://example.com/alice".to_owned(),
            evidence: vec![RejectEvidence {
                role: "premise",
                subject: "http://example.com/alice".to_owned(),
                predicate: RDF_TYPE.to_owned(),
                object: "http://example.com/Other".to_owned(),
                origin: "asserted".to_owned(),
            }],
        };
        let delta = MutationDeltaPreview {
            inserted_triples: vec![
                (
                    "http://example.com/Child".to_owned(),
                    "http://www.w3.org/2000/01/rdf-schema#subClassOf".to_owned(),
                    "http://example.com/Parent".to_owned(),
                ),
                (
                    "http://example.com/alice".to_owned(),
                    RDF_TYPE.to_owned(),
                    "http://example.com/Other".to_owned(),
                ),
            ],
            removed_triples: Vec::new(),
        };

        let attribution = attribute_reject_delta(&reject, &delta).expect("attribution");

        assert_eq!(
            attribution.likely_commit_trigger(),
            Some((
                "http://example.com/alice".to_owned(),
                RDF_TYPE.to_owned(),
                "http://example.com/Other".to_owned(),
            ))
        );
        assert_eq!(
            attribution.candidates[0].matched_evidence_roles,
            vec!["premise"]
        );
        assert!(
            attribution.candidates[0]
                .match_reasons
                .contains(&"focus-resource-subject")
        );
    }
}
