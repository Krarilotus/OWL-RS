//! The engine API's own routes ([ADR-0007](../../../../docs/adr/0007-one-engine-api.md)):
//! what the standard protocols have no JSON form for. Each is a thin translation of a store
//! operation; the capabilities the protocols cover are mounted under
//! `/api/v1/repositories/{id}` with the `/dataset/…` handlers ([`super::repository`]).

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ApiError;
use crate::http::guard;
use crate::http::repository::Repository;
use crate::repositories::DEFAULT_REPOSITORY;
use crate::state::AppState;

#[derive(Serialize)]
struct RepositoryEntry {
    id: String,
    title: String,
    /// The path of its engine API.
    path: String,
    default: bool,
}

/// The repositories: the default one first, then the others.
pub async fn repositories(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let entries: Vec<RepositoryEntry> = std::iter::once((DEFAULT_REPOSITORY.to_owned(), None))
        .chain(state.repositories().list())
        .map(|(id, title)| RepositoryEntry {
            path: format!("/api/v1/repositories/{id}"),
            title: title.unwrap_or_else(|| format!("NRESE: {id}")),
            default: id == DEFAULT_REPOSITORY,
            id,
        })
        .collect();
    Ok(Json(entries).into_response())
}

/// The repository's namespace prefixes, as a JSON object of prefix to IRI.
pub async fn namespaces_get(
    Repository(state): Repository,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    Ok(Json(state.store().namespaces().all()).into_response())
}

/// Binds the prefix to the IRI in the body (plain text).
pub async fn namespace_put(
    Repository(state): Repository,
    Path((_, prefix)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let iri = std::str::from_utf8(&body)
        .map_err(|_| ApiError::bad_request("the namespace must be UTF-8"))?
        .trim();
    if iri.is_empty() {
        return Err(ApiError::bad_request("the namespace is empty"));
    }
    state
        .store()
        .namespaces()
        .set(&prefix, iri)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes the prefix; 404 if it isn't bound.
pub async fn namespace_delete(
    Repository(state): Repository,
    Path((_, prefix)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    match state
        .store()
        .namespaces()
        .remove(&prefix)
        .map_err(|error| ApiError::internal(error.to_string()))?
    {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found(format!("no namespace '{prefix}'"))),
    }
}

#[derive(Serialize)]
struct SessionOpened {
    id: String,
    /// Where its operations go.
    path: String,
    /// How long it lives untouched.
    idle_seconds: u64,
}

/// Opens a client transaction ([`nrese_store::Sessions`]): writes collected over requests
/// and committed as one.
pub async fn session_begin(
    Repository(state): Repository,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let sessions = state.store();
    let sessions = sessions.sessions();
    let session = sessions.begin();
    let opened = SessionOpened {
        path: format!("/api/v1/repositories/{id}/sessions/{session}"),
        idle_seconds: sessions.idle().as_secs(),
        id: session,
    };
    Ok((StatusCode::CREATED, Json(opened)).into_response())
}

fn no_session(session: &str) -> ApiError {
    ApiError::not_found(format!("no session '{session}'"))
}

/// Adds operations to session `session`; 404 if it isn't open.
fn add(
    state: &AppState,
    session: &str,
    ops: Vec<nrese_store::StatementOp>,
) -> Result<StatusCode, ApiError> {
    match state.store().sessions().add(session, ops) {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(no_session(session)),
    }
}

/// A SPARQL update (as the update endpoint takes it), applied at the commit.
pub async fn session_update(
    Repository(state): Repository,
    Path((_, session)): Path<(String, String)>,
    raw: axum::extract::RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let request = super::rdf4j::update(&raw, &headers, &body)?;
    add(
        &state,
        &session,
        vec![nrese_store::StatementOp::Update(request)],
    )
}

/// RDF data to add (`POST`) or remove (`DELETE`) at the commit: into the graph `graph`
/// if given (an IRI), else into the graphs the data names.
pub async fn session_data(
    Repository(state): Repository,
    Path((_, session)): Path<(String, String)>,
    method: axum::http::Method,
    raw: axum::extract::RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    state.policy().enforce_rdf_upload_bytes(body.len())?;
    let pairs = super::rdf4j::pairs(&raw)?;
    let contexts = pairs
        .iter()
        .filter(|(key, _)| key == "graph")
        .map(|(_, iri)| {
            nrese_rdf::NamedNode::new(iri.as_str())
                .map(nrese_rdf::GraphName::from)
                .map_err(|error| ApiError::bad_request(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = super::rdf4j::payload(&headers, &body)?;
    let op = match method {
        axum::http::Method::DELETE => nrese_store::StatementOp::RemoveData { data, contexts },
        _ => nrese_store::StatementOp::Add { data, contexts },
    };
    add(&state, &session, vec![op])
}

/// A query on the data as the session's operations would leave it.
pub async fn session_query(
    Repository(state): Repository,
    Path((_, session)): Path<(String, String)>,
    raw: axum::extract::RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let access = guard::update_access(&state, &headers).await?;
    let ops = state
        .store()
        .sessions()
        .pending(&session)
        .ok_or_else(|| no_session(&session))?;
    let pending = super::rdf4j::scoped(ops, &access);
    let mut operation = if body.is_empty() {
        super::requests::query_from_url(raw.0.as_deref())?
    } else {
        super::requests::query_from_post(
            raw.0.as_deref(),
            headers.get(axum::http::header::CONTENT_TYPE),
            &body,
        )?
    };
    operation.access = access.read.clone();
    let accept = super::requests::accept_header_value(&headers);
    super::sparql::execute_query_in(state, operation, accept, Some(pending)).await
}

/// Commits the session's operations as one transaction, through the same gates as any
/// write.
pub async fn session_commit(
    Repository(state): Repository,
    Path((_, session)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let access = guard::update_access(&state, &headers).await?;
    let request = state
        .store()
        .sessions()
        .take(&session)
        .ok_or_else(|| no_session(&session))?;
    super::rdf4j::apply(&state, request.ops, &access).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Closes the session without committing.
pub async fn session_rollback(
    Repository(state): Repository,
    Path((_, session)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    match state.store().sessions().rollback(&session) {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(no_session(&session)),
    }
}

#[derive(Serialize)]
struct ExplanationStep {
    subject: String,
    predicate: String,
    object: String,
    /// `asserted` or `inferred`.
    origin: &'static str,
    /// The rule that derives it, for an inferred step.
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<String>,
    /// The steps (indexes) it is derived from.
    premises: Vec<usize>,
}

/// Why the statement `subj pred obj` (N-Triples terms, as RDF4J's parameters) holds: a
/// derivation from asserted statements, the statement first. 404 if it doesn't hold or
/// reasoning is off; 403 for users who don't see inferred statements.
pub async fn explain(
    Repository(state): Repository,
    raw: axum::extract::RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let access = guard::query_access(&state, &headers).await?;
    if !access.sees_inferred() {
        return Err(ApiError::forbidden(
            "explanations show inferred statements, which the requester doesn't see",
        ));
    }
    let pairs = super::rdf4j::pairs(&raw)?;
    let get = |name: &str| -> Result<nrese_rdf::Term, ApiError> {
        let text = pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| ApiError::bad_request(format!("explain needs {name}")))?;
        super::rdf4j::term(text)
    };
    let (subject, predicate, object) = (get("subj")?, get("pred")?, get("obj")?);
    let Some(program) = state.pipeline().reasoner().config().materialised_program() else {
        return Err(ApiError::not_found("reasoning is off: nothing is inferred"));
    };
    let store = state.store();
    let steps = tokio::task::spawn_blocking(move || {
        store.explain_statement(
            program,
            subject.as_ref(),
            predicate.as_ref(),
            object.as_ref(),
        )
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .ok_or_else(|| ApiError::not_found("the statement doesn't hold"))?;
    let steps: Vec<ExplanationStep> = steps
        .into_iter()
        .map(|step| ExplanationStep {
            subject: step.subject,
            predicate: step.predicate,
            object: step.object,
            origin: step.origin,
            rule: step.rule,
            premises: step.premises,
        })
        .collect();
    Ok(Json(serde_json::json!({ "steps": steps })).into_response())
}

#[derive(Serialize)]
struct ReasoningRun {
    ruleset: String,
    revision: u64,
    asserted: u64,
    inferred: u64,
    violations: usize,
    elapsed_ms: u128,
}

impl From<&nrese_store::MaterialisationReport> for ReasoningRun {
    fn from(report: &nrese_store::MaterialisationReport) -> Self {
        Self {
            ruleset: report.ruleset.clone(),
            revision: report.revision,
            asserted: report.asserted,
            inferred: report.inferred,
            violations: report.violations,
            elapsed_ms: report.elapsed.as_millis(),
        }
    }
}

/// Recomputes the repository's inferences from its asserted statements under its
/// ruleset (after a change of ontology files or rules outside the store, or to leave
/// quarantine after a repair). 404 without reasoning.
pub async fn rematerialise(
    Repository(state): Repository,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &headers).await?;
    let report = rematerialised(&state)
        .await?
        .ok_or_else(|| ApiError::not_found("reasoning is off: nothing to materialise"))?;
    Ok(Json(ReasoningRun::from(&report)).into_response())
}

/// The repository's inferences recomputed, if it reasons.
async fn rematerialised(
    state: &AppState,
) -> Result<Option<nrese_store::MaterialisationReport>, ApiError> {
    let Some(program) = state.pipeline().reasoner().config().materialised_program() else {
        return Ok(None);
    };
    let store = state.store();
    tokio::task::spawn_blocking(move || store.rematerialise(program))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map(Some)
        .map_err(|error| ApiError::internal(error.to_string()))
}

#[derive(Serialize)]
struct ImportReport {
    revision: u64,
    /// Statements parsed, duplicates included.
    parsed: u64,
    inserted: u64,
    deleted: u64,
    /// Statements skipped for syntax errors (`skip_errors=true`).
    skipped: u64,
    elapsed_ms: u128,
    /// The inferences recomputed after the load, where the repository reasons.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningRun>,
}

/// Bulk-loads the RDF document in the body (its format from `Content-Type`): into the
/// graph `graph` for triple formats (the default graph without), replacing the
/// repository's statements with `replace=true`, skipping statements with syntax errors
/// with `skip_errors=true`. Then the inferences are recomputed. For large loads: the
/// loader's parallel path, not one commit's; administrators only, since a bulk load
/// replaces whole indexes and passes no commit gate.
pub async fn import(
    Repository(state): Repository,
    raw: axum::extract::RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &headers).await?;
    state.ensure_serving()?;
    state.policy().enforce_rdf_upload_bytes(body.len())?;
    let pairs = super::rdf4j::pairs(&raw)?;
    let flag = |name: &str| {
        pairs
            .iter()
            .any(|(key, value)| key == name && value.eq_ignore_ascii_case("true"))
    };
    let graph = match pairs.iter().find(|(key, _)| key == "graph") {
        Some((_, iri)) => {
            nrese_rdf::NamedNode::new(iri.as_str())
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
            nrese_store::GraphTarget::NamedGraph(iri.clone())
        }
        None => nrese_store::GraphTarget::DefaultGraph,
    };
    let format = super::rdf_payload::parse_graph_content_format(super::media::header_value_str(
        headers.get(axum::http::header::CONTENT_TYPE),
    ))?;
    let extension = match format {
        nrese_store::GraphResultFormat::NTriples => "nt",
        nrese_store::GraphResultFormat::Turtle => "ttl",
        nrese_store::GraphResultFormat::RdfXml => "rdf",
        nrese_store::GraphResultFormat::NQuads => "nq",
        nrese_store::GraphResultFormat::TriG => "trig",
        nrese_store::GraphResultFormat::JsonLd => "jsonld",
        nrese_store::GraphResultFormat::BinaryRdf => "brf",
    };
    // The loader reads files: the body is one, for the load's duration.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file = std::env::temp_dir().join(format!(
        "nrese-import-{}-{}.{extension}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let request = nrese_store::BulkLoadRequest {
        files: vec![file.clone()],
        replace: flag("replace"),
        graph,
        skip_errors: flag("skip_errors"),
    };
    let store = state.store();
    let loaded = tokio::task::spawn_blocking(move || {
        std::fs::write(&file, &body)?;
        let report = store.bulk_load(&request);
        let _ = std::fs::remove_file(&file);
        report.map_err(|error| std::io::Error::other(error.to_string()))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let reasoning = rematerialised(&state).await?;
    Ok(Json(ImportReport {
        revision: reasoning.as_ref().map_or(loaded.revision, |r| r.revision),
        parsed: loaded.parsed,
        inserted: loaded.inserted,
        deleted: loaded.deleted,
        skipped: loaded.skipped,
        elapsed_ms: loaded.elapsed.as_millis(),
        reasoning: reasoning.as_ref().map(ReasoningRun::from),
    })
    .into_response())
}
