use anyhow::{Result, anyhow, bail};
use reqwest::Client;

use crate::compat_common::{
    GraphWriteRequest, RequestExecutionOptions, build_response_semantics_report,
    execute_graph_delete_raw, execute_graph_head_raw, execute_graph_read_raw,
    execute_graph_write_raw, require_success_http,
};
use crate::layout::ServiceTarget;
use crate::model::{
    CompatCase, CompatCaseReport, CompatHeaders, CompatKind, CompatOperation, compat_kind_label,
    compat_operation_label,
};
use crate::normalize::canonicalize_rdf_graph_set;
use crate::payloads::graph_payload;

pub async fn execute_case(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
) -> Result<CompatCaseReport> {
    let graph_target = case
        .graph_target
        .as_ref()
        .ok_or_else(|| anyhow!("graph operation requires graph_target"))?;
    let options = RequestExecutionOptions::from_case(case);

    match case.operation {
        CompatOperation::GraphRead => {
            maybe_prepare_graph(client, left, right, case, graph_target).await?;
            build_graph_read_report(client, left, right, case, graph_target, options).await
        }
        CompatOperation::GraphHead => {
            maybe_prepare_graph(client, left, right, case, graph_target).await?;
            build_graph_head_report(client, left, right, case, graph_target, options).await
        }
        CompatOperation::GraphDeleteEffect => {
            maybe_prepare_graph(client, left, right, case, graph_target).await?;
            build_graph_delete_effect_report(client, left, right, case, graph_target, options).await
        }
        CompatOperation::GraphPutEffect => {
            build_graph_write_effect_report(client, left, right, case, graph_target, true, options)
                .await
        }
        CompatOperation::GraphPostEffect => {
            build_graph_write_effect_report(client, left, right, case, graph_target, false, options)
                .await
        }
        _ => bail!("graph store module received a non-graph operation"),
    }
}

async fn maybe_prepare_graph(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
    graph_target: &crate::model::CompatGraphTarget,
) -> Result<()> {
    let Ok(payload) = graph_payload(case) else {
        return Ok(());
    };
    let content_type = graph_content_type(case)?;

    write_and_ensure_success(
        client,
        left,
        graph_target,
        content_type,
        &payload,
        case.graph_replace,
        &case.request_headers,
    )
    .await?;
    write_and_ensure_success(
        client,
        right,
        graph_target,
        content_type,
        &payload,
        case.graph_replace,
        &case.request_headers,
    )
    .await?;

    Ok(())
}

async fn build_graph_read_report(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
    graph_target: &crate::model::CompatGraphTarget,
    options: RequestExecutionOptions,
) -> Result<CompatCaseReport> {
    match case.kind {
        CompatKind::GraphTriplesSet => {
            let left_outcome = execute_graph_read_raw(
                client,
                left,
                graph_target,
                &case.accept,
                &case.request_headers,
                options,
            )
            .await?;
            let right_outcome = execute_graph_read_raw(
                client,
                right,
                graph_target,
                &case.accept,
                &case.request_headers,
                options,
            )
            .await?;
            let left_http = require_success_http(left, "graph read", &left_outcome)?;
            let right_http = require_success_http(right, "graph read", &right_outcome)?;

            let left_set =
                canonicalize_rdf_graph_set(left_http.content_type.as_deref(), &left_http.body)?;
            let right_set =
                canonicalize_rdf_graph_set(right_http.content_type.as_deref(), &right_http.body)?;

            Ok(CompatCaseReport {
                name: case.name.clone(),
                operation: compat_operation_label(case.operation),
                kind: compat_kind_label(case.kind),
                left_status: left_http.status.as_u16(),
                right_status: right_http.status.as_u16(),
                left_content_type: left_http.content_type.clone(),
                right_content_type: right_http.content_type.clone(),
                left_body_class: None,
                right_body_class: None,
                matched: left_set == right_set,
                left_summary: format!("triples={}", left_set.len()),
                right_summary: format!("triples={}", right_set.len()),
            })
        }
        _ => bail!("graph-read operation only supports graph-triples-set in this build"),
    }
}

async fn build_graph_head_report(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
    graph_target: &crate::model::CompatGraphTarget,
    options: RequestExecutionOptions,
) -> Result<CompatCaseReport> {
    match case.kind {
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass => {
            let left_outcome = execute_graph_head_raw(
                client,
                left,
                graph_target,
                &case.accept,
                &case.request_headers,
                options,
            )
            .await?;
            let right_outcome = execute_graph_head_raw(
                client,
                right,
                graph_target,
                &case.accept,
                &case.request_headers,
                options,
            )
            .await?;
            build_response_semantics_report(case, &left_outcome, &right_outcome)
        }
        _ => bail!("graph-head operation only supports status-and-content-type in this build"),
    }
}

async fn build_graph_delete_effect_report(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
    graph_target: &crate::model::CompatGraphTarget,
    options: RequestExecutionOptions,
) -> Result<CompatCaseReport> {
    match case.kind {
        CompatKind::GraphTriplesSet => {
            let left_delete = execute_graph_delete_raw(
                client,
                left,
                graph_target,
                &case.request_headers,
                options,
            )
            .await?;
            let right_delete = execute_graph_delete_raw(
                client,
                right,
                graph_target,
                &case.request_headers,
                options,
            )
            .await?;
            require_success_http(left, "graph delete", &left_delete)?;
            require_success_http(right, "graph delete", &right_delete)?;

            let left_outcome = execute_graph_read_raw(
                client,
                left,
                graph_target,
                "application/n-triples",
                &case.request_headers,
                RequestExecutionOptions::default(),
            )
            .await?;
            let right_outcome = execute_graph_read_raw(
                client,
                right,
                graph_target,
                "application/n-triples",
                &case.request_headers,
                RequestExecutionOptions::default(),
            )
            .await?;
            let left_http = require_success_http(left, "graph read", &left_outcome)?;
            let right_http = require_success_http(right, "graph read", &right_outcome)?;

            let left_set =
                canonicalize_rdf_graph_set(left_http.content_type.as_deref(), &left_http.body)?;
            let right_set =
                canonicalize_rdf_graph_set(right_http.content_type.as_deref(), &right_http.body)?;

            Ok(CompatCaseReport {
                name: case.name.clone(),
                operation: compat_operation_label(case.operation),
                kind: compat_kind_label(case.kind),
                left_status: left_http.status.as_u16(),
                right_status: right_http.status.as_u16(),
                left_content_type: left_http.content_type.clone(),
                right_content_type: right_http.content_type.clone(),
                left_body_class: None,
                right_body_class: None,
                matched: left_set == right_set,
                left_summary: format!("triples={}", left_set.len()),
                right_summary: format!("triples={}", right_set.len()),
            })
        }
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass => {
            let left_delete = execute_graph_delete_raw(
                client,
                left,
                graph_target,
                &case.request_headers,
                options,
            )
            .await?;
            let right_delete = execute_graph_delete_raw(
                client,
                right,
                graph_target,
                &case.request_headers,
                options,
            )
            .await?;
            build_response_semantics_report(case, &left_delete, &right_delete)
        }
        _ => bail!(
            "graph-delete-effect operation only supports graph-triples-set or response-semantics comparators in this build"
        ),
    }
}

async fn build_graph_write_effect_report(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
    graph_target: &crate::model::CompatGraphTarget,
    replace: bool,
    options: RequestExecutionOptions,
) -> Result<CompatCaseReport> {
    match case.kind {
        CompatKind::GraphTriplesSet => {
            let payload = graph_payload(case)?;
            let content_type = graph_content_type(case)?;

            let left_write = execute_graph_write_raw(
                client,
                left,
                GraphWriteRequest {
                    graph_target,
                    content_type,
                    payload: &payload,
                    replace,
                    extra_headers: &case.request_headers,
                    options,
                },
            )
            .await?;
            let right_write = execute_graph_write_raw(
                client,
                right,
                GraphWriteRequest {
                    graph_target,
                    content_type,
                    payload: &payload,
                    replace,
                    extra_headers: &case.request_headers,
                    options,
                },
            )
            .await?;
            let left_http = require_success_http(left, "graph write", &left_write)?;
            let right_http = require_success_http(right, "graph write", &right_write)?;

            let left_set =
                read_graph_set(client, left, graph_target, &case.request_headers).await?;
            let right_set =
                read_graph_set(client, right, graph_target, &case.request_headers).await?;

            Ok(CompatCaseReport {
                name: case.name.clone(),
                operation: compat_operation_label(case.operation),
                kind: compat_kind_label(case.kind),
                left_status: left_http.status.as_u16(),
                right_status: right_http.status.as_u16(),
                left_content_type: left_http.content_type.clone(),
                right_content_type: right_http.content_type.clone(),
                left_body_class: None,
                right_body_class: None,
                matched: left_http.status == right_http.status && left_set == right_set,
                left_summary: format!(
                    "write-status={} triples={}",
                    left_http.status.as_u16(),
                    left_set.len()
                ),
                right_summary: format!(
                    "write-status={} triples={}",
                    right_http.status.as_u16(),
                    right_set.len()
                ),
            })
        }
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass => {
            let payload = graph_payload(case)?;
            let content_type = graph_content_type(case)?;
            let left_write = execute_graph_write_raw(
                client,
                left,
                GraphWriteRequest {
                    graph_target,
                    content_type,
                    payload: &payload,
                    replace,
                    extra_headers: &case.request_headers,
                    options,
                },
            )
            .await?;
            let right_write = execute_graph_write_raw(
                client,
                right,
                GraphWriteRequest {
                    graph_target,
                    content_type,
                    payload: &payload,
                    replace,
                    extra_headers: &case.request_headers,
                    options,
                },
            )
            .await?;

            build_response_semantics_report(case, &left_write, &right_write)
        }
        _ => bail!(
            "graph-write-effect operation only supports graph-triples-set or response-semantics comparators in this build"
        ),
    }
}

fn graph_content_type(case: &CompatCase) -> Result<&str> {
    case.graph_content_type.as_deref().ok_or_else(|| {
        anyhow!("graph operation requires graph_content_type when graph_payload is set")
    })
}

async fn write_and_ensure_success(
    client: &Client,
    target: &ServiceTarget,
    graph_target: &crate::model::CompatGraphTarget,
    content_type: &str,
    payload: &[u8],
    replace: bool,
    extra_headers: &CompatHeaders,
) -> Result<()> {
    let outcome = execute_graph_write_raw(
        client,
        target,
        GraphWriteRequest {
            graph_target,
            content_type,
            payload,
            replace,
            extra_headers,
            options: RequestExecutionOptions::default(),
        },
    )
    .await?;
    require_success_http(target, "graph write", &outcome).map(|_| ())
}

async fn read_graph_set(
    client: &Client,
    target: &ServiceTarget,
    graph_target: &crate::model::CompatGraphTarget,
    extra_headers: &CompatHeaders,
) -> Result<std::collections::BTreeSet<String>> {
    let outcome = execute_graph_read_raw(
        client,
        target,
        graph_target,
        "application/n-triples",
        extra_headers,
        RequestExecutionOptions::default(),
    )
    .await?;
    let http = require_success_http(target, "graph read", &outcome)?;
    canonicalize_rdf_graph_set(http.content_type.as_deref(), &http.body)
}
