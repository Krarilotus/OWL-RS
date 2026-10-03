//! The engine API's description (OpenAPI 3.1), generated from the handlers' annotations and
//! the types they take and return, served at `GET /api/v1/openapi.json`: clients and the
//! frontend generate their code from it, so they can't drift from the server. A test checks
//! that every `/api/v1` route is described.
//!
//! The engine API's own operations are annotated where they are handled; the standard
//! protocols' operations (SPARQL, the Graph Store Protocol, SHACL validation, ...), whose
//! handlers also serve `/dataset/…`, are described by the functions below.

use axum::Json;
use axum::response::{IntoResponse, Response};
use utoipa::OpenApi;

/// An error, as every endpoint reports one (RFC 9457 problem details,
/// `application/problem+json`).
#[derive(utoipa::ToSchema)]
#[expect(dead_code)]
pub(crate) struct Problem {
    /// The problem's kind, a URI (`https://nrese.dev/problems/…`).
    r#type: String,
    title: String,
    status: u16,
    detail: String,
    /// For a commit a consistency check rejected: the violated rule and the statements
    /// that clash.
    reasoner_reject: Option<serde_json::Value>,
}

const ID: &str = "The repository's id (`nrese` is the default one)";

macro_rules! protocol {
    ($name:ident, $($spec:tt)*) => {
        #[utoipa::path($($spec)*)]
        #[expect(dead_code)]
        fn $name() {}
    };
}

/// A route kept for clients of an earlier release: described as deprecated (its answers
/// carry `Deprecation` and a `Link` to its successor).
macro_rules! deprecated {
    ($name:ident, $($spec:tt)*) => {
        #[utoipa::path($($spec)*)]
        #[deprecated]
        #[expect(dead_code)]
        fn $name() {}
    };
}

protocol!(
    capabilities,
    get,
    path = "/api/v1/capabilities",
    tag = "server",
    responses((
        status = 200,
        description = "What the server offers: protocols, reasoning modes, features",
        body = serde_json::Value
    ))
);
protocol!(
    health,
    get,
    path = "/api/v1/health",
    tag = "server",
    responses((
        status = 200,
        description = "Readiness and the state of every part: store, reasoning, access, jobs",
        body = serde_json::Value
    ))
);
protocol!(
    diagnostics,
    get,
    path = "/api/v1/diagnostics",
    tag = "server",
    responses((
        status = 200,
        description = "How the server runs: memory, caches, query budget, requests",
        body = serde_json::Value
    ))
);
protocol!(
    ai_status,
    get,
    path = "/api/v1/ai/status",
    tag = "server",
    responses((
        status = 200,
        description = "Whether AI query suggestions are on, and their provider",
        body = serde_json::Value
    ))
);
protocol!(
    ai_query_suggestions,
    post,
    path = "/api/v1/ai/query-suggestions",
    tag = "server",
    request_body(
        content = serde_json::Value,
        description = "What the user asks for, in words"
    ),
    responses(
        (
            status = 200,
            description = "Suggested SPARQL queries",
            body = serde_json::Value
        ),
        (
            status = 503,
            description = "Suggestions are off",
            body = Problem
        )
    )
);
protocol!(
    image,
    post,
    path = "/api/v1/repositories/{id}/image",
    tag = "data",
    params(("id" = String, Path, description = ID)),
    responses(
        (
            status = 200,
            description = "An image backup written to `backups/` of the data directory",
            body = serde_json::Value
        ),
        (
            status = 403,
            description = "Administrators only",
            body = Problem
        )
    )
);
deprecated!(
    saved_queries_before,
    get,
    path = "/api/v1/queries",
    tag = "queries",
    responses((
        status = 200,
        description = "As `GET /api/v1/saved-queries`",
        body = serde_json::Value
    ))
);
deprecated!(
    saved_query_before,
    get,
    path = "/api/v1/queries/{space}/{name}",
    tag = "queries",
    params(("space" = String, Path), ("name" = String, Path)),
    responses((
        status = 200,
        description = "As `GET /api/v1/saved-queries/{space}/{name}`",
        body = serde_json::Value
    ))
);
deprecated!(
    saved_query_put_before,
    put,
    path = "/api/v1/queries/{space}/{name}",
    tag = "queries",
    params(("space" = String, Path), ("name" = String, Path)),
    responses((
        status = 200,
        description = "As `PUT /api/v1/saved-queries/{space}/{name}`",
        body = serde_json::Value
    ))
);
deprecated!(
    saved_query_delete_before,
    delete,
    path = "/api/v1/queries/{space}/{name}",
    tag = "queries",
    params(("space" = String, Path), ("name" = String, Path)),
    responses((
        status = 204,
        description = "As `DELETE /api/v1/saved-queries/{space}/{name}`"
    ))
);

protocol!(
    info,
    get,
    path = "/api/v1/repositories/{id}/info",
    tag = "repositories",
    params(("id" = String, Path, description = ID)),
    responses((
        status = 200,
        description = "Readiness, sizes and settings",
        body = serde_json::Value
    ))
);
protocol!(
    summary,
    get,
    path = "/api/v1/repositories/{id}/summary",
    tag = "repositories",
    params(("id" = String, Path, description = ID)),
    responses((
        status = 200,
        description = "Statistics: statements, graphs, classes, properties",
        body = serde_json::Value
    ))
);
protocol!(
    reasoning,
    get,
    path = "/api/v1/repositories/{id}/reasoning",
    tag = "reasoning",
    params(("id" = String, Path, description = ID)),
    responses((
        status = 200,
        description = "The ruleset, the last run, consistency, diagnostics",
        body = serde_json::Value
    ))
);
protocol!(
    service_description,
    get,
    path = "/api/v1/repositories/{id}/service-description",
    tag = "sparql",
    params(("id" = String, Path, description = ID)),
    responses((
        status = 200,
        description = "The SPARQL service description",
        content_type = "text/turtle",
        body = String
    ))
);
protocol!(query, method(get, post), path = "/api/v1/repositories/{id}/query", tag = "sparql",
    params(("id" = String, Path, description = ID),
        ("query" = Option<String>, Query, description = "The query (`GET`, or a form `POST`)"),
        ("default-graph-uri" = Option<Vec<String>>, Query), ("named-graph-uri" = Option<Vec<String>>, Query),
        ("infer" = Option<bool>, Query, description = "Whether inferred statements are read (default true)")),
    request_body(content = String, content_type = "application/sparql-query",
        description = "The query; or `application/x-www-form-urlencoded` with `query=`"),
    responses((status = 200, description = "SPARQL results (JSON, XML, CSV, TSV, binary) or RDF, as `Accept` asks"),
        (status = 400, description = "A syntax error", body = Problem),
        (status = 408, description = "The query timed out", body = Problem),
        (status = 413, description = "Larger than the policy allows", body = Problem)));
protocol!(update, post, path = "/api/v1/repositories/{id}/update", tag = "sparql",
    params(("id" = String, Path, description = ID),
        ("using-graph-uri" = Option<Vec<String>>, Query), ("using-named-graph-uri" = Option<Vec<String>>, Query)),
    request_body(content = String, content_type = "application/sparql-update",
        description = "The update; or `application/x-www-form-urlencoded` with `update=`"),
    responses((status = 204, description = "Committed"),
        (status = 400, description = "A syntax error, or rejected by a consistency or SHACL gate", body = Problem),
        (status = 403, description = "Writes a graph the requester may not write", body = Problem)));
protocol!(
    sparql,
    method(get, post),
    path = "/api/v1/repositories/{id}/sparql",
    tag = "sparql",
    params(("id" = String, Path, description = ID)),
    responses(
        (status = 200, description = "A query's results"),
        (status = 204, description = "An update committed")
    )
);
protocol!(
    tell,
    post,
    path = "/api/v1/repositories/{id}/tell",
    tag = "data",
    params(("id" = String, Path, description = ID)),
    request_body(content = String, description = "Statements to add, as RDF"),
    responses((
        status = 200,
        description = "Added; what the reasoner derived from them",
        body = serde_json::Value
    ))
);
protocol!(data, method(get, head, put, post, delete), path = "/api/v1/repositories/{id}/data", tag = "data",
    params(("id" = String, Path, description = ID),
        ("graph" = Option<String>, Query, description = "The graph's IRI"),
        ("default" = Option<String>, Query, description = "The default graph")),
    request_body(content = String, description = "A graph (`PUT` replaces, `POST` adds), in any RDF format NRESE reads"),
    responses((status = 200, description = "The graph (`GET`), or replaced"), (status = 201, description = "Created"),
        (status = 204, description = "Added or removed"), (status = 404, description = "No such graph", body = Problem)));
protocol!(shacl, method(get, post), path = "/api/v1/repositories/{id}/shacl", tag = "shacl",
    params(("id" = String, Path, description = ID),
        ("graph" = Option<String>, Query, description = "Validate this graph only")),
    request_body(content = String, description = "`POST`: shapes to validate with instead of the stored ones"),
    responses((status = 200, description = "A validation report (JSON with `conforms`, `results`; or RDF)", body = serde_json::Value)));
protocol!(
    autocomplete,
    get,
    path = "/api/v1/repositories/{id}/autocomplete",
    tag = "data",
    params(
        ("id" = String, Path, description = ID),
        ("q" = String, Query, description = "What was typed")
    ),
    responses((
        status = 200,
        description = "Resources whose IRI or label starts so",
        body = serde_json::Value
    ))
);
protocol!(
    classification,
    get,
    path = "/api/v1/repositories/{id}/classification",
    tag = "reasoning",
    params(("id" = String, Path, description = ID)),
    responses((
        status = 200,
        description = "The class hierarchy as the reasoner computed it",
        body = serde_json::Value
    ))
);
protocol!(
    backup,
    get,
    path = "/api/v1/repositories/{id}/backup",
    tag = "data",
    params(("id" = String, Path, description = ID)),
    responses((
        status = 200,
        description = "Every statement",
        content_type = "application/n-quads",
        body = String
    ))
);
protocol!(
    restore,
    post,
    path = "/api/v1/repositories/{id}/restore",
    tag = "data",
    params(("id" = String, Path, description = ID)),
    request_body(content = String, content_type = "application/n-quads"),
    responses((
        status = 200,
        description = "Restored",
        body = serde_json::Value
    ))
);

#[derive(OpenApi)]
#[openapi(
    info(
        title = "NRESE engine API",
        description = "Every capability of the engine, the same for every repository \
            (ADR-0007), and users, workspaces and graph policies (ADR-0008). Errors are \
            problem details (`application/problem+json`). The standard protocols (SPARQL \
            1.1/1.2 Protocol, Graph Store Protocol, RDF4J REST) are served as well; this \
            describes the engine API's routes."
    ),
    paths(
        super::api_v1::repositories,
        super::api_v1::repository_get,
        super::api_v1::repository_put,
        super::api_v1::repository_patch,
        super::api_v1::repository_delete,
        info,
        summary,
        reasoning,
        service_description,
        query,
        update,
        sparql,
        tell,
        data,
        shacl,
        autocomplete,
        classification,
        backup,
        restore,
        image,
        capabilities,
        health,
        diagnostics,
        ai_status,
        ai_query_suggestions,
        saved_queries_before,
        saved_query_before,
        saved_query_put_before,
        saved_query_delete_before,
        super::api_v1::namespaces_get,
        super::api_v1::namespace_put,
        super::api_v1::namespace_delete,
        super::api_v1::explain,
        super::api_v1::running_queries,
        super::api_v1::cancel_query,
        super::api_v1::graphs,
        super::api_v1::shapes_get,
        super::api_v1::shapes_put,
        super::api_v1::shapes_delete,
        super::api_v1::rules_get,
        super::api_v1::rules_put,
        super::api_v1::rules_delete,
        super::api_v1::import,
        super::api_v1::server_files,
        super::api_v1::import_server_files,
        super::api_v1::jobs,
        super::api_v1::job,
        super::api_v1::job_cancel,
        super::api_v1::rematerialise,
        super::api_v1::session_begin,
        super::api_v1::session_rollback,
        super::api_v1::session_update,
        super::api_v1::session_data,
        super::api_v1::session_query,
        super::api_v1::session_commit,
        super::access_api::overview,
        super::access_api::me,
        super::access_api::login,
        super::access_api::logout,
        super::access_api::settings_put,
        super::access_api::role_put,
        super::access_api::role_delete,
        super::access_api::user_put,
        super::access_api::user_delete,
        super::access_api::workspaces,
        super::access_api::workspace,
        super::access_api::workspace_put,
        super::access_api::workspace_delete,
        super::access_api::member_put,
        super::access_api::member_delete,
        super::access_api::history,
        super::access_api::import,
        super::access_api::export,
        super::access_api::saved_queries,
        super::access_api::saved_query,
        super::access_api::saved_query_put,
        super::access_api::saved_query_delete,
        super::draft_check::capabilities,
        super::draft_check::check,
        crate::replication::log,
        crate::replication::image,
        crate::replication::status,
        openapi,
    ),
    tags(
        (name = "server", description = "The server: capabilities, health, diagnostics, AI query suggestions"),
        (name = "repositories", description = "Repositories: each a store with its own data and reasoning"),
        (name = "sparql", description = "SPARQL queries and updates"),
        (name = "data", description = "Graphs, imports, backups"),
        (name = "namespaces", description = "Prefixes"),
        (name = "sessions", description = "Client transactions: writes collected over requests, committed as one"),
        (name = "reasoning", description = "Inferences, their explanations, the class hierarchy"),
        (name = "shacl", description = "Validation"),
        (name = "access", description = "Users, workspaces, personal spaces, role rules, logins (ADR-0008)"),
        (name = "queries", description = "Saved queries, in personal spaces and workspaces"),
        (name = "draft-check", description = "Isolated checks of pinned inputs for the Datamodel Workflow (dmw-store-check/1); no repository is read or written"),
        (name = "replication", description = "Read replicas: a primary's log and image, a replica's status (docs/ops/replication.md)"),
    )
)]
pub struct ApiDoc;

/// This description.
#[utoipa::path(get, path = "/api/v1/openapi.json", tag = "repositories",
    responses((status = 200, description = "The OpenAPI description", body = serde_json::Value)))]
pub async fn openapi() -> Response {
    let mut doc = ApiDoc::openapi();
    doc.info.version = env!("CARGO_PKG_VERSION").to_owned();
    Json(doc).into_response()
}
