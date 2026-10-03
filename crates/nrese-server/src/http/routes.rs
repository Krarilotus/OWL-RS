use axum::Router;
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::routing::{MethodRouter, delete, get, post, put};

use crate::http::access_api;
use crate::http::api_v1;
use crate::http::draft_check;
use crate::http::handlers;
use crate::http::rdf4j;
use crate::state::AppState;

/// A repository's capabilities under `/api/v1/repositories/{id}` ([`super::repository`]).
fn repository_routes() -> Router<AppState> {
    Router::new()
        .route("/info", get(handlers::dataset_info))
        .route("/summary", get(handlers::operator_dataset_summary))
        .route("/reasoning", get(handlers::operator_reasoning_diagnostics))
        .route("/service-description", get(handlers::service_description))
        .route(
            "/query",
            get(handlers::query_get).post(handlers::query_post),
        )
        .route("/update", post(handlers::update_post))
        .route(
            "/sparql",
            get(handlers::sparql_get).post(handlers::sparql_post),
        )
        .route("/tell", post(handlers::tell_post))
        .route(
            "/data",
            get(handlers::graph_get)
                .head(handlers::graph_head)
                .put(handlers::graph_put)
                .post(handlers::graph_post)
                .delete(handlers::graph_delete),
        )
        .route(
            "/shacl",
            get(handlers::shacl_get).post(handlers::shacl_post),
        )
        .route(
            "/shapes",
            get(api_v1::shapes_get)
                .put(api_v1::shapes_put)
                .delete(api_v1::shapes_delete),
        )
        .route("/autocomplete", get(handlers::autocomplete))
        .route("/classification", get(handlers::classification_get))
        .route("/backup", get(handlers::admin_backup_dataset))
        .route("/restore", post(handlers::admin_restore_dataset))
        .route("/image", post(handlers::repository_image_backup))
        .route("/namespaces", get(api_v1::namespaces_get))
        .route(
            "/namespaces/{prefix}",
            put(api_v1::namespace_put).delete(api_v1::namespace_delete),
        )
        .route("/explain", get(api_v1::explain))
        .route("/queries", get(api_v1::running_queries))
        .route("/queries/{query}", delete(api_v1::cancel_query))
        .route("/graphs", get(api_v1::graphs))
        .route("/import", post(api_v1::import))
        .route(
            "/import/files",
            get(api_v1::server_files).post(api_v1::import_server_files),
        )
        .route("/reasoning/rematerialise", post(api_v1::rematerialise))
        .route(
            "/rules",
            get(api_v1::rules_get)
                .put(api_v1::rules_put)
                .delete(api_v1::rules_delete),
        )
        .route("/sessions", post(api_v1::session_begin))
        .route("/sessions/{session}", delete(api_v1::session_rollback))
        .route("/sessions/{session}/update", post(api_v1::session_update))
        .route(
            "/sessions/{session}/data",
            post(api_v1::session_data).delete(api_v1::session_data),
        )
        .route(
            "/sessions/{session}/query",
            get(api_v1::session_query).post(api_v1::session_query),
        )
        .route("/sessions/{session}/commit", post(api_v1::session_commit))
}

/// When the routes kept for clients of an earlier release were deprecated (RFC 9745's
/// form: `@` and the Unix time; 3 October 2026, ADR-0007).
const DEPRECATED_SINCE: &str = "@1790985600";

/// `route`, kept for clients of an earlier release: its answers say so (`Deprecation`,
/// RFC 9745) and link to the route that replaces it (`Link` with `rel="successor-version"`:
/// the request's path with `old` replaced by `new`).
fn deprecated(
    route: MethodRouter<AppState>,
    old: &'static str,
    new: &'static str,
) -> MethodRouter<AppState> {
    route.layer(axum::middleware::from_fn(
        move |request: Request, next: Next| async move {
            let successor = request.uri().path().replacen(old, new, 1);
            let mut response = next.run(request).await;
            let headers = response.headers_mut();
            headers.insert("deprecation", HeaderValue::from_static(DEPRECATED_SINCE));
            if let Ok(link) =
                HeaderValue::from_str(&format!("<{successor}>; rel=\"successor-version\""))
            {
                headers.insert(axum::http::header::LINK, link);
            }
            response
        },
    ))
}

/// Every route. The public ones (health, version, the console's files, the API
/// description, logging in and out, RDF4J's protocol version) answer anyone; every other
/// request is authenticated first ([`super::authentication`]), before its handler and its
/// body are read, and its handler checks its action ([`super::guard`]).
pub fn router(state: AppState) -> Router {
    let public = Router::new()
        .route("/", get(handlers::root_redirect))
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route("/version", get(handlers::version))
        .route("/console/{*path}", get(handlers::console_file))
        .route("/api/v1/openapi.json", get(crate::http::openapi::openapi))
        .route("/api/v1/access/login", post(access_api::login))
        .route("/api/v1/access/logout", post(access_api::logout))
        .route("/protocol", get(rdf4j::protocol));
    let authenticated = Router::new()
        .route("/console", get(handlers::console_ui))
        .route(
            "/api/v1/draft-check/capabilities",
            get(draft_check::capabilities),
        )
        .route("/api/v1/draft-check", post(draft_check::check))
        // The server: what it offers, how it runs, AI query suggestions.
        .route("/api/v1/capabilities", get(handlers::operator_capabilities))
        .route("/api/v1/health", get(handlers::operator_extended_health))
        .route(
            "/api/v1/diagnostics",
            get(handlers::operator_runtime_diagnostics),
        )
        .route("/api/v1/ai/status", get(handlers::ai_status))
        .route(
            "/api/v1/ai/query-suggestions",
            post(handlers::ai_query_suggestions),
        )
        .route("/ops", get(handlers::operator_ui))
        .route("/ui", get(handlers::operator_ui))
        // The routes of earlier releases, until the next one (ADR-0007).
        .route(
            "/api/ai/status",
            deprecated(get(handlers::ai_status), "/api/ai/", "/api/v1/ai/"),
        )
        .route(
            "/api/ai/query-suggestions",
            deprecated(
                post(handlers::ai_query_suggestions),
                "/api/ai/",
                "/api/v1/ai/",
            ),
        )
        .route(
            "/ops/api/capabilities",
            deprecated(
                get(handlers::operator_capabilities),
                "/ops/api/capabilities",
                "/api/v1/capabilities",
            ),
        )
        .route(
            "/ops/api/dataset/summary",
            deprecated(
                get(handlers::operator_dataset_summary),
                "/ops/api/dataset/summary",
                "/api/v1/repositories/nrese/summary",
            ),
        )
        .route(
            "/ops/api/health/extended",
            deprecated(
                get(handlers::operator_extended_health),
                "/ops/api/health/extended",
                "/api/v1/health",
            ),
        )
        .route(
            "/ops/api/diagnostics/runtime",
            deprecated(
                get(handlers::operator_runtime_diagnostics),
                "/ops/api/diagnostics/runtime",
                "/api/v1/diagnostics",
            ),
        )
        .route(
            "/ops/api/diagnostics/reasoning",
            deprecated(
                get(handlers::operator_reasoning_diagnostics),
                "/ops/api/diagnostics/reasoning",
                "/api/v1/repositories/nrese/reasoning",
            ),
        )
        .route(
            "/ops/api/admin/dataset/backup",
            deprecated(
                get(handlers::admin_backup_dataset),
                "/ops/api/admin/dataset/backup",
                "/api/v1/repositories/nrese/backup",
            ),
        )
        .route(
            "/ops/api/admin/dataset/restore",
            deprecated(
                post(handlers::admin_restore_dataset),
                "/ops/api/admin/dataset/restore",
                "/api/v1/repositories/nrese/restore",
            ),
        )
        .route(
            "/ops/api/admin/dataset/image",
            deprecated(
                post(handlers::admin_image_backup),
                "/ops/api/admin/dataset/image",
                "/api/v1/repositories/nrese/image",
            ),
        )
        .route("/metrics", get(handlers::metrics))
        .route(
            "/dataset/service-description",
            get(handlers::service_description),
        )
        .route("/dataset/info", get(handlers::dataset_info))
        .route("/dataset/autocomplete", get(handlers::autocomplete))
        .route(
            "/dataset/query",
            get(handlers::query_get).post(handlers::query_post),
        )
        .route("/dataset/update", post(handlers::update_post))
        // One URL for queries and updates, for clients configured with a single endpoint.
        .route(
            crate::runtime_posture::SPARQL_ENDPOINT,
            get(handlers::sparql_get).post(handlers::sparql_post),
        )
        .route(
            "/dataset",
            get(handlers::sparql_get).post(handlers::sparql_post),
        )
        .route("/dataset/tell", post(handlers::tell_post))
        .route(
            "/dataset/data",
            get(handlers::graph_get)
                .head(handlers::graph_head)
                .put(handlers::graph_put)
                .post(handlers::graph_post)
                .delete(handlers::graph_delete),
        )
        .route(
            crate::runtime_posture::SHACL_ENDPOINT,
            get(handlers::shacl_get).post(handlers::shacl_post),
        )
        .route("/dataset/classification", get(handlers::classification_get))
        // The engine API (ADR-0007): every capability for every repository, by the same
        // handlers as the default repository's `/dataset/…` routes.
        .route("/api/v1/repositories", get(api_v1::repositories))
        .route("/api/v1/jobs", get(api_v1::jobs))
        .route(
            "/api/v1/jobs/{job}",
            get(api_v1::job).delete(api_v1::job_cancel),
        )
        // Users, workspaces and graph policies (ADR-0008).
        .route("/api/v1/access", get(access_api::overview))
        .route("/api/v1/access/me", get(access_api::me))
        .route("/api/v1/access/settings", put(access_api::settings_put))
        .route(
            "/api/v1/access/roles/{name}",
            put(access_api::role_put).delete(access_api::role_delete),
        )
        .route(
            "/api/v1/access/users/{name}",
            put(access_api::user_put).delete(access_api::user_delete),
        )
        .route("/api/v1/access/workspaces", get(access_api::workspaces))
        .route(
            "/api/v1/access/workspaces/{name}",
            get(access_api::workspace)
                .put(access_api::workspace_put)
                .delete(access_api::workspace_delete),
        )
        .route(
            "/api/v1/access/workspaces/{name}/members/{user}",
            put(access_api::member_put).delete(access_api::member_delete),
        )
        .route("/api/v1/access/history", get(access_api::history))
        .route("/api/v1/access/import", post(access_api::import))
        .route("/api/v1/access/export", get(access_api::export))
        .route("/api/v1/saved-queries", get(access_api::saved_queries))
        .route(
            "/api/v1/saved-queries/{space}/{name}",
            get(access_api::saved_query)
                .put(access_api::saved_query_put)
                .delete(access_api::saved_query_delete),
        )
        // Saved queries' earlier path (it read like the running queries').
        .route(
            "/api/v1/queries",
            deprecated(
                get(access_api::saved_queries),
                "/api/v1/queries",
                "/api/v1/saved-queries",
            ),
        )
        .route(
            "/api/v1/queries/{space}/{name}",
            deprecated(
                get(access_api::saved_query)
                    .put(access_api::saved_query_put)
                    .delete(access_api::saved_query_delete),
                "/api/v1/queries",
                "/api/v1/saved-queries",
            ),
        )
        .route(
            "/api/v1/repositories/{id}",
            get(api_v1::repository_get)
                .put(api_v1::repository_put)
                .patch(api_v1::repository_patch)
                .delete(api_v1::repository_delete),
        )
        .nest("/api/v1/repositories/{id}", repository_routes())
        // The RDF4J REST protocol (`rdf4j.rs`).
        .route("/repositories", get(rdf4j::repositories))
        .route(
            "/repositories/{id}",
            get(rdf4j::query_get)
                .post(rdf4j::query_post)
                .put(rdf4j::repository_put)
                .delete(rdf4j::repository_delete),
        )
        .route(
            "/repositories/{id}/statements",
            get(rdf4j::statements_get)
                .post(rdf4j::statements_post)
                .put(rdf4j::statements_put)
                .delete(rdf4j::statements_delete),
        )
        .route("/repositories/{id}/size", get(rdf4j::size))
        .route("/repositories/{id}/contexts", get(rdf4j::contexts_get))
        .route(
            "/repositories/{id}/namespaces",
            get(rdf4j::namespaces_get).delete(rdf4j::namespaces_delete),
        )
        .route(
            "/repositories/{id}/namespaces/{prefix}",
            get(rdf4j::namespace_get)
                .put(rdf4j::namespace_put)
                .delete(rdf4j::namespace_delete),
        )
        .route(
            "/repositories/{id}/rdf-graphs/service",
            get(rdf4j::graph_store_get)
                .put(rdf4j::graph_store_put)
                .post(rdf4j::graph_store_post)
                .delete(rdf4j::graph_store_delete),
        )
        .route(
            "/repositories/{id}/transactions",
            post(rdf4j::transaction_begin),
        )
        .route(
            "/repositories/{id}/transactions/{txid}",
            put(rdf4j::transaction_action)
                .post(rdf4j::transaction_action)
                .delete(rdf4j::transaction_rollback),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            super::authentication::authenticate,
        ));
    public.merge(authenticated).with_state(state)
}
