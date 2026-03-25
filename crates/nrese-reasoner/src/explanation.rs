use crate::output::RejectEvidence;
use crate::vocabulary::RDF_TYPE;

pub fn assertion_evidence(
    role: &'static str,
    subject: &str,
    predicate: &str,
    object: &str,
    origin: impl Into<String>,
) -> RejectEvidence {
    RejectEvidence {
        role,
        subject: subject.to_owned(),
        predicate: predicate.to_owned(),
        object: object.to_owned(),
        origin: origin.into(),
    }
}

pub fn type_assertion_evidence(
    role: &'static str,
    instance: &str,
    class_iri: &str,
    origin: impl Into<String>,
) -> RejectEvidence {
    assertion_evidence(role, instance, RDF_TYPE, class_iri, origin)
}

pub fn declaration_evidence(
    role: &'static str,
    resource: &str,
    declaration_class: &str,
    origin: impl Into<String>,
) -> RejectEvidence {
    assertion_evidence(role, resource, RDF_TYPE, declaration_class, origin)
}
