use crate::class_consistency::ConsistencyViolation;
use crate::explanation::{assertion_evidence, declaration_evidence};
use crate::output::{RejectBlame, RejectEvidence};
use crate::vocabulary::OWL_IRREFLEXIVE_PROPERTY;

pub(super) struct AssertionCollision<'a> {
    pub(super) focus_resource: &'a str,
    pub(super) left_type: &'a str,
    pub(super) right_type: &'a str,
    pub(super) principal_assertion: &'a str,
    pub(super) contextual_assertion: &'a str,
    pub(super) heuristic: &'static str,
    pub(super) contextual_origin_label: &'a str,
    pub(super) evidence: Vec<RejectEvidence>,
}

pub(super) fn ordered_pair(left: u32, right: u32) -> (u32, u32) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

pub(super) fn render_assertion(subject: &str, property: &str, object: &str) -> String {
    format!("<{subject}> <{property}> <{object}>")
}

pub(super) fn build_irreflexive_property_violation(
    subject: &str,
    property: &str,
) -> ConsistencyViolation {
    let assertion = render_assertion(subject, property, subject);

    ConsistencyViolation {
        message: format!(
            "Consistency violation: property {property} is declared owl:IrreflexiveProperty but assertion {assertion} is present"
        ),
        violated_constraint: "owl:IrreflexiveProperty".to_owned(),
        focus_resource: subject.to_owned(),
        left_type: property.to_owned(),
        right_type: subject.to_owned(),
        left_origin: "property declaration".to_owned(),
        right_origin: format!("effective assertion {assertion}"),
        blame: RejectBlame {
            heuristic: "prefer-direct-self-loop-assertion",
            principal_contributor: assertion.clone(),
            principal_origin: "effective property assertion".to_owned(),
            contextual_contributor: property.to_owned(),
            contextual_origin: "property declaration".to_owned(),
        },
        evidence: vec![
            declaration_evidence(
                "constraint-declaration",
                property,
                OWL_IRREFLEXIVE_PROPERTY,
                "property declaration",
            ),
            assertion_evidence(
                "conflicting-assertion",
                subject,
                property,
                subject,
                "effective property assertion",
            ),
        ],
    }
}

pub(super) fn build_assertion_collision_violation(
    violated_constraint: &str,
    message: String,
    collision: AssertionCollision<'_>,
) -> ConsistencyViolation {
    ConsistencyViolation {
        message,
        violated_constraint: violated_constraint.to_owned(),
        focus_resource: collision.focus_resource.to_owned(),
        left_type: collision.left_type.to_owned(),
        right_type: collision.right_type.to_owned(),
        left_origin: format!("effective assertion {}", collision.principal_assertion),
        right_origin: format!(
            "{} {}",
            collision.contextual_origin_label, collision.contextual_assertion
        ),
        blame: RejectBlame {
            heuristic: collision.heuristic,
            principal_contributor: collision.principal_assertion.to_owned(),
            principal_origin: "effective property assertion".to_owned(),
            contextual_contributor: collision.contextual_assertion.to_owned(),
            contextual_origin: collision.contextual_origin_label.to_owned(),
        },
        evidence: collision.evidence,
    }
}
