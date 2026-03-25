use anyhow::{Result, anyhow};
use reqwest::Client;

use crate::compat_common::{
    HttpOutcome, build_response_semantics_report, execute_query_raw, ensure_success,
};
use crate::layout::ServiceTarget;
use crate::model::{
    CompatCase, CompatCaseReport, CompatKind, compat_kind_label, compat_operation_label,
};
use crate::normalize::{
    canonicalize_ntriples_set, extract_ask_boolean, extract_binding_count, parse_json,
};
use crate::payloads::query_text;

pub async fn execute_case(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
) -> Result<CompatCaseReport> {
    let query = query_text(case)?;
    let left_outcome =
        execute_query_raw(client, left, &query, &case.accept, &case.request_headers).await?;
    let right_outcome =
        execute_query_raw(client, right, &query, &case.accept, &case.request_headers).await?;

    if matches!(
        case.kind,
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass
    ) {
        return build_response_semantics_report(case, &left_outcome, &right_outcome);
    }

    ensure_success(left, "query", &left_outcome)?;
    ensure_success(right, "query", &right_outcome)?;

    build_report(case, &left_outcome, &right_outcome)
}

fn build_report(
    case: &CompatCase,
    left: &HttpOutcome,
    right: &HttpOutcome,
) -> Result<CompatCaseReport> {
    let (matched, left_summary, right_summary) = compare_payloads(case.kind, &left.body, &right.body)?;

    Ok(CompatCaseReport {
        name: case.name.clone(),
        operation: compat_operation_label(case.operation),
        kind: compat_kind_label(case.kind),
        left_status: left.status.as_u16(),
        right_status: right.status.as_u16(),
        left_content_type: left.content_type.clone(),
        right_content_type: right.content_type.clone(),
        left_body_class: None,
        right_body_class: None,
        matched,
        left_summary,
        right_summary,
    })
}

fn compare_payloads(kind: CompatKind, left: &[u8], right: &[u8]) -> Result<(bool, String, String)> {
    match kind {
        CompatKind::AskBoolean => {
            let left_json = parse_json(left)?;
            let right_json = parse_json(right)?;
            let left_value = extract_ask_boolean(&left_json)?;
            let right_value = extract_ask_boolean(&right_json)?;
            Ok((
                left_value == right_value,
                format!("boolean={left_value}"),
                format!("boolean={right_value}"),
            ))
        }
        CompatKind::SolutionsCount => {
            let left_json = parse_json(left)?;
            let right_json = parse_json(right)?;
            let left_count = extract_binding_count(&left_json)?;
            let right_count = extract_binding_count(&right_json)?;
            Ok((
                left_count == right_count,
                format!("bindings={left_count}"),
                format!("bindings={right_count}"),
            ))
        }
        CompatKind::ConstructTriplesSet | CompatKind::GraphTriplesSet => {
            let left_set = canonicalize_ntriples_set(left)?;
            let right_set = canonicalize_ntriples_set(right)?;
            Ok((
                left_set == right_set,
                format!("triples={}", left_set.len()),
                format!("triples={}", right_set.len()),
            ))
        }
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass => {
            Err(anyhow!("status-and-content-type is not a query payload comparator"))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{CompatCase, CompatKind, CompatOperation};

    use super::{compat_kind_label, compat_operation_label};

    #[test]
    fn kind_labels_cover_graph_mode() {
        assert_eq!(compat_kind_label(CompatKind::GraphTriplesSet), "graph-triples-set");
    }

    #[test]
    fn default_query_operation_is_query() {
        let case: CompatCase = serde_json::from_str(
            r#"{
                "name":"ask",
                "query":"ASK WHERE { ?s ?p ?o }",
                "kind":"ask-boolean"
            }"#,
        )
        .expect("case");

        assert!(matches!(case.operation, CompatOperation::Query));
        assert!(case.request_headers.is_empty());
    }

    #[test]
    fn operation_labels_cover_graph_write_effects() {
        assert_eq!(
            compat_operation_label(CompatOperation::GraphPutEffect),
            "graph-put-effect"
        );
        assert_eq!(
            compat_operation_label(CompatOperation::GraphPostEffect),
            "graph-post-effect"
        );
    }
}
