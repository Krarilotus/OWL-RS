use anyhow::{Result, anyhow};
use reqwest::Client;

use crate::compat_common::{
    HttpOutcome, RequestExecutionOptions, build_response_semantics_report, execute_query_raw,
    require_success_http,
};
use crate::layout::ServiceTarget;
use crate::model::{
    CompatCase, CompatCaseReport, CompatKind, compat_kind_label, compat_operation_label,
};
use crate::normalize::{
    canonicalize_bindings_set, canonicalize_rdf_graph_set, compare_canonical_sets,
    extract_ask_boolean, extract_binding_count, parse_json,
};
use crate::payloads::query_text;

pub async fn execute_case(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
) -> Result<CompatCaseReport> {
    let query = query_text(case)?;
    let options = RequestExecutionOptions::from_case(case);
    let left_outcome = execute_query_raw(
        client,
        left,
        &query,
        &case.accept,
        &case.request_headers,
        options,
    )
    .await?;
    let right_outcome = execute_query_raw(
        client,
        right,
        &query,
        &case.accept,
        &case.request_headers,
        options,
    )
    .await?;

    if matches!(
        case.kind,
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass
    ) {
        return build_response_semantics_report(case, &left_outcome, &right_outcome);
    }

    let left_http = require_success_http(left, "query", &left_outcome)?;
    let right_http = require_success_http(right, "query", &right_outcome)?;

    build_report(case, left_http, right_http)
}

fn build_report(
    case: &CompatCase,
    left: &HttpOutcome,
    right: &HttpOutcome,
) -> Result<CompatCaseReport> {
    let (matched, left_summary, right_summary) = compare_payloads(
        case.kind,
        left.content_type.as_deref(),
        &left.body,
        right.content_type.as_deref(),
        &right.body,
    )?;
    let result_diff = build_result_diff(
        case.kind,
        left.content_type.as_deref(),
        &left.body,
        right.content_type.as_deref(),
        &right.body,
    )?;

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
        left_result_count: result_diff.left_result_count,
        right_result_count: result_diff.right_result_count,
        left_only_sample: result_diff.left_only_sample,
        right_only_sample: result_diff.right_only_sample,
    })
}

fn compare_payloads(
    kind: CompatKind,
    left_content_type: Option<&str>,
    left: &[u8],
    right_content_type: Option<&str>,
    right: &[u8],
) -> Result<(bool, String, String)> {
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
        CompatKind::SolutionsBindingsSet => {
            let left_json = parse_json(left)?;
            let right_json = parse_json(right)?;
            let left_set = canonicalize_bindings_set(&left_json)?;
            let right_set = canonicalize_bindings_set(&right_json)?;
            let comparison = compare_canonical_sets(&left_set, &right_set);
            Ok((
                comparison.matched,
                format!("bindings={}", comparison.left_count),
                format!("bindings={}", comparison.right_count),
            ))
        }
        CompatKind::ConstructTriplesSet | CompatKind::GraphTriplesSet => {
            let left_set = canonicalize_rdf_graph_set(left_content_type, left)?;
            let right_set = canonicalize_rdf_graph_set(right_content_type, right)?;
            let comparison = compare_canonical_sets(&left_set, &right_set);
            Ok((
                comparison.matched,
                format!("triples={}", comparison.left_count),
                format!("triples={}", comparison.right_count),
            ))
        }
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass => Err(anyhow!(
            "status-and-content-type is not a query payload comparator"
        )),
    }
}

struct ResultDiff {
    left_result_count: Option<usize>,
    right_result_count: Option<usize>,
    left_only_sample: Vec<String>,
    right_only_sample: Vec<String>,
}

fn build_result_diff(
    kind: CompatKind,
    left_content_type: Option<&str>,
    left: &[u8],
    right_content_type: Option<&str>,
    right: &[u8],
) -> Result<ResultDiff> {
    match kind {
        CompatKind::SolutionsBindingsSet => {
            let left_set = canonicalize_bindings_set(&parse_json(left)?)?;
            let right_set = canonicalize_bindings_set(&parse_json(right)?)?;
            let comparison = compare_canonical_sets(&left_set, &right_set);
            Ok(ResultDiff {
                left_result_count: Some(comparison.left_count),
                right_result_count: Some(comparison.right_count),
                left_only_sample: comparison.left_only_sample,
                right_only_sample: comparison.right_only_sample,
            })
        }
        CompatKind::ConstructTriplesSet | CompatKind::GraphTriplesSet => {
            let left_set = canonicalize_rdf_graph_set(left_content_type, left)?;
            let right_set = canonicalize_rdf_graph_set(right_content_type, right)?;
            let comparison = compare_canonical_sets(&left_set, &right_set);
            Ok(ResultDiff {
                left_result_count: Some(comparison.left_count),
                right_result_count: Some(comparison.right_count),
                left_only_sample: comparison.left_only_sample,
                right_only_sample: comparison.right_only_sample,
            })
        }
        _ => Ok(ResultDiff {
            left_result_count: None,
            right_result_count: None,
            left_only_sample: Vec::new(),
            right_only_sample: Vec::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{CompatCase, CompatKind, CompatOperation};

    use super::{compat_kind_label, compat_operation_label};

    #[test]
    fn kind_labels_cover_graph_mode() {
        assert_eq!(
            compat_kind_label(CompatKind::GraphTriplesSet),
            "graph-triples-set"
        );
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

    #[test]
    fn bindings_set_comparator_detects_different_rows_with_equal_count() {
        let left = br#"{
            "results": {
                "bindings": [
                    { "s": { "type": "uri", "value": "http://example.com/a" } }
                ]
            }
        }"#;
        let right = br#"{
            "results": {
                "bindings": [
                    { "s": { "type": "uri", "value": "http://example.com/b" } }
                ]
            }
        }"#;

        let (matched, left_summary, right_summary) =
            super::compare_payloads(
                CompatKind::SolutionsBindingsSet,
                Some("application/sparql-results+json"),
                left,
                Some("application/sparql-results+json"),
                right,
            )
                .expect("comparison");

        assert!(!matched);
        assert_eq!(left_summary, "bindings=1");
        assert_eq!(right_summary, "bindings=1");
    }
}
