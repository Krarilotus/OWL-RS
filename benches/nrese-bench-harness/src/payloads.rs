use anyhow::{Result, anyhow};

use crate::model::{CompatCase, GeneratedPayloadKind, GeneratedPayloadSpec};

pub fn query_text(case: &CompatCase) -> Result<String> {
    match (&case.query, &case.generated_payload) {
        (Some(query), _) => Ok(query.clone()),
        (None, Some(spec)) if matches!(spec.kind, GeneratedPayloadKind::SparqlQuery) => {
            Ok(generate_query(spec))
        }
        _ => Err(anyhow!("query operation requires query text")),
    }
}

pub fn update_text(case: &CompatCase) -> Result<String> {
    match (&case.update, &case.generated_payload) {
        (Some(update), _) => Ok(update.clone()),
        (None, Some(spec)) if matches!(spec.kind, GeneratedPayloadKind::SparqlUpdate) => {
            Ok(generate_update(spec))
        }
        _ => Err(anyhow!("update-effect operation requires update text")),
    }
}

pub fn graph_payload(case: &CompatCase) -> Result<Vec<u8>> {
    match (&case.graph_payload, &case.generated_payload) {
        (Some(payload), _) => Ok(payload.as_bytes().to_vec()),
        (None, Some(spec)) if matches!(spec.kind, GeneratedPayloadKind::RdfTurtle) => {
            Ok(generate_turtle(spec).into_bytes())
        }
        _ => Err(anyhow!("graph write effect requires graph_payload")),
    }
}

fn generate_query(spec: &GeneratedPayloadSpec) -> String {
    pad_to_size("SELECT * WHERE { ?s ?p ?o } #", spec.bytes)
}

fn generate_update(spec: &GeneratedPayloadSpec) -> String {
    pad_to_size(
        "PREFIX ex: <http://example.com/> INSERT DATA { ex:s ex:p ex:o . } #",
        spec.bytes,
    )
}

fn generate_turtle(spec: &GeneratedPayloadSpec) -> String {
    pad_to_size(
        "@prefix ex: <http://example.com/> .\nex:s ex:p ex:o .\n#",
        spec.bytes,
    )
}

fn pad_to_size(prefix: &str, bytes: usize) -> String {
    if prefix.len() >= bytes {
        return prefix.to_owned();
    }

    let mut value = String::with_capacity(bytes);
    value.push_str(prefix);
    value.push_str(&"x".repeat(bytes - prefix.len()));
    value
}

#[cfg(test)]
mod tests {
    use crate::model::{CompatCase, CompatHeaders, CompatKind, CompatOperation};

    use super::{graph_payload, query_text, update_text};

    #[test]
    fn generates_query_payload_from_spec() {
        let case = CompatCase {
            name: "oversize-query".to_owned(),
            operation: CompatOperation::Query,
            query: None,
            accept: "application/sparql-results+json".to_owned(),
            update: None,
            verify_query: None,
            graph_target: None,
            graph_payload: None,
            graph_content_type: None,
            graph_replace: true,
            generated_payload: Some(crate::model::GeneratedPayloadSpec {
                kind: crate::model::GeneratedPayloadKind::SparqlQuery,
                bytes: 128,
            }),
            request_headers: CompatHeaders::new(),
            kind: CompatKind::StatusContentTypeBodyClass,
        };

        let query = query_text(&case).expect("query");
        assert!(query.starts_with("SELECT * WHERE"));
        assert!(query.len() >= 128);
    }

    #[test]
    fn generates_update_payload_from_spec() {
        let case = CompatCase {
            name: "oversize-update".to_owned(),
            operation: CompatOperation::UpdateEffect,
            query: None,
            accept: "application/sparql-results+json".to_owned(),
            update: None,
            verify_query: None,
            graph_target: None,
            graph_payload: None,
            graph_content_type: None,
            graph_replace: true,
            generated_payload: Some(crate::model::GeneratedPayloadSpec {
                kind: crate::model::GeneratedPayloadKind::SparqlUpdate,
                bytes: 128,
            }),
            request_headers: CompatHeaders::new(),
            kind: CompatKind::StatusContentTypeBodyClass,
        };

        let update = update_text(&case).expect("update");
        assert!(update.starts_with("PREFIX ex:"));
        assert!(update.len() >= 128);
    }

    #[test]
    fn generates_graph_payload_from_spec() {
        let case = CompatCase {
            name: "oversize-graph".to_owned(),
            operation: CompatOperation::GraphPutEffect,
            query: None,
            accept: "application/n-triples".to_owned(),
            update: None,
            verify_query: None,
            graph_target: Some(crate::model::CompatGraphTarget::DefaultGraph),
            graph_payload: None,
            graph_content_type: Some("text/turtle".to_owned()),
            graph_replace: true,
            generated_payload: Some(crate::model::GeneratedPayloadSpec {
                kind: crate::model::GeneratedPayloadKind::RdfTurtle,
                bytes: 128,
            }),
            request_headers: CompatHeaders::new(),
            kind: CompatKind::StatusContentTypeBodyClass,
        };

        let payload = graph_payload(&case).expect("graph payload");
        assert!(payload.len() >= 128);
    }
}
