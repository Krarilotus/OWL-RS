use anyhow::{Result, anyhow};
use reqwest::Client;

use crate::compat_common::{build_response_semantics_report, execute_update_raw, ensure_success};
use crate::compat_query::execute_case as execute_query_case_from_payloads;
use crate::layout::ServiceTarget;
use crate::model::{CompatCase, CompatCaseReport, CompatKind};
use crate::payloads::update_text;

pub async fn execute_case(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
) -> Result<CompatCaseReport> {
    let status_only_comparison = matches!(
        case.kind,
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass
    );
    let update = update_text(case)?;

    let left_update = execute_update_raw(client, left, &update, &case.request_headers).await?;
    let right_update = execute_update_raw(client, right, &update, &case.request_headers).await?;

    if status_only_comparison {
        return build_response_semantics_report(case, &left_update, &right_update);
    }

    let verify_query = case
        .verify_query
        .as_deref()
        .ok_or_else(|| anyhow!("update-effect operation requires verify_query"))?;

    ensure_success(left, "update", &left_update)?;
    ensure_success(right, "update", &right_update)?;

    let query_case = CompatCase {
        name: case.name.clone(),
        operation: case.operation,
        query: Some(verify_query.to_owned()),
        accept: case.accept.clone(),
        update: None,
        verify_query: None,
        graph_target: None,
        graph_payload: None,
        graph_content_type: None,
        graph_replace: true,
        generated_payload: None,
        request_headers: case.request_headers.clone(),
        kind: case.kind,
    };

    execute_query_case_from_payloads(client, left, right, &query_case).await
}

#[cfg(test)]
mod tests {
    use crate::model::{CompatCase, CompatKind, CompatOperation};

    #[test]
    fn status_only_update_case_does_not_require_verify_query_in_fixture_shape() {
        let case: CompatCase = serde_json::from_str(
            r#"{
                "name":"invalid-update",
                "operation":"update-effect",
                "update":"THIS IS NOT SPARQL",
                "kind":"status-content-type-body-class"
            }"#,
        )
        .expect("case");

        assert!(matches!(case.operation, CompatOperation::UpdateEffect));
        assert!(case.verify_query.is_none());
        assert!(case.request_headers.is_empty());
        assert!(matches!(
            case.kind,
            CompatKind::StatusContentTypeBodyClass
        ));
    }
}
