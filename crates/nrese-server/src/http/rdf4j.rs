//! The RDF4J REST protocol (what RDF4J's `HTTPRepository`, GraphDB's clients and tools
//! built on them speak), over the one dataset this server holds.
//!
//! | Path | What |
//! |---|---|
//! | `GET /protocol` | the protocol version, `12` |
//! | `GET /repositories` | the repository list |
//! | `PUT`, `DELETE /repositories/{id}` | a repository created (title and reasoning from the configuration in the body, [`crate::repository_config`]) or removed with its data |
//! | `GET`/`POST /repositories/{id}` | a SPARQL query (`query`, `infer`, the dataset parameters) |
//! | `GET /repositories/{id}/statements` | the statements matching `subj`, `pred`, `obj`, `context` (`infer`), as RDF |
//! | `POST /repositories/{id}/statements` | an RDF payload added (into `context`), or a SPARQL update (`update=`) |
//! | `PUT /repositories/{id}/statements` | the statements of `context` (or all) replaced by the payload |
//! | `DELETE /repositories/{id}/statements` | the statements matching `subj`, `pred`, `obj`, `context` removed |
//! | `GET /repositories/{id}/size` | the number of statements (in `context`) |
//! | `GET /repositories/{id}/contexts` | the named graphs |
//! | `/repositories/{id}/namespaces[/{prefix}]` | namespace prefixes: list, read, set, remove |
//! | `/repositories/{id}/rdf-graphs/service` | the SPARQL Graph Store protocol |
//! | `/repositories/{id}/transactions[/{txid}]` | transactions: begin, `action=ADD`, `DELETE`, `UPDATE`, `COMMIT`, `PING`; `DELETE` rolls back |
//!
//! The configured store is repository `nrese`; others are created with `PUT` and removed
//! with `DELETE /repositories/{id}` ([`crate::repositories`]). Terms in
//! `subj`, `pred`, `obj` and `context` are written as in N-Triples (`<iri>`, `_:b`,
//! `"text"@en`, `"1"^^<…#int>`); `context=null` is the default graph. A transaction's
//! operations are kept on the server and applied in one commit; reads inside one
//! (`action=QUERY`, `GET`, `SIZE`) see its changes: its operations are applied to an engine
//! transaction that is never committed, holding the writer slot while they read. Namespaces
//! are kept in `rdf4j-namespaces.json` in an on-disk store's directory (in memory
//! otherwise), written whole at every change.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_rdf::{BlankNode, GraphName, Literal, NamedNode, Term};
use nrese_store::{
    MutationCommand, RdfPayload, SparqlUpdateRequest, StatementOp, StatementPattern,
    StatementsRequest,
};
use parking_lot::Mutex;

use crate::error::ApiError;
use crate::http::graph_store;
use crate::http::guard;
use crate::http::media::{GRAPHS, header_value_str, media_type_matches, negotiated};
use crate::http::mutation;
use crate::http::rdf_payload::{parse_graph_content_format, parse_rdf_base_iri};
use crate::http::requests::{
    accept_header_value, query_from_post, query_from_url, update_from_post,
};
use crate::http::sparql;
use crate::repositories::DEFAULT_REPOSITORY;
use crate::state::AppState;

/// The RDF4J protocol version answered at `/protocol`.
const PROTOCOL: &str = "12";
/// Transactions untouched this long are dropped.
const TRANSACTION_IDLE: Duration = Duration::from_secs(600);

/// Open transactions and namespace prefixes.
pub struct Rdf4jState {
    next_transaction: AtomicU64,
    transactions: Mutex<HashMap<String, Pending>>,
    namespaces: Mutex<BTreeMap<String, String>>,
    /// Where the namespaces are kept (on-disk stores).
    namespaces_file: Option<std::path::PathBuf>,
}

impl Default for Rdf4jState {
    fn default() -> Self {
        Self::with_file(None)
    }
}

impl Rdf4jState {
    /// The state of a store whose namespaces are kept in `file`, if given: read from it
    /// when it exists, else the four standard prefixes.
    pub fn with_file(file: Option<std::path::PathBuf>) -> Self {
        let standard = || {
            [
                ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
                ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
                ("owl", "http://www.w3.org/2002/07/owl#"),
                ("xsd", "http://www.w3.org/2001/XMLSchema#"),
            ]
            .into_iter()
            .map(|(prefix, iri)| (prefix.to_owned(), iri.to_owned()))
            .collect()
        };
        let namespaces = file
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(standard);
        Self {
            next_transaction: AtomicU64::new(1),
            transactions: Mutex::default(),
            namespaces: Mutex::new(namespaces),
            namespaces_file: file,
        }
    }

    /// Changes the namespaces and keeps them (a temporary file renamed over the old one).
    fn change_namespaces(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, String>),
    ) -> Result<(), ApiError> {
        let mut namespaces = self.namespaces.lock();
        change(&mut namespaces);
        let Some(path) = &self.namespaces_file else {
            return Ok(());
        };
        let json = serde_json::to_vec_pretty(&*namespaces)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json)
            .and_then(|()| std::fs::rename(&temporary, path))
            .map_err(|error| ApiError::internal(format!("keeping the namespaces: {error}")))
    }
}

/// A transaction's operations, applied at its commit.
struct Pending {
    ops: Vec<StatementOp>,
    touched: Instant,
}

/// URL parameters, repeated ones in order.
fn pairs(raw: &RawQuery) -> Result<Vec<(String, String)>, ApiError> {
    serde_urlencoded::from_str(raw.0.as_deref().unwrap_or_default())
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

fn param<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// A term written as in N-Triples.
fn term(text: &str) -> Result<Term, ApiError> {
    let bad = || ApiError::bad_request(format!("'{text}' is not an N-Triples term"));
    let text = text.trim();
    if let Some(iri) = text.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        return NamedNode::new(unescape(iri).ok_or_else(bad)?)
            .map(Term::from)
            .map_err(|_| bad());
    }
    if let Some(label) = text.strip_prefix("_:") {
        return BlankNode::new(label).map(Term::from).map_err(|_| bad());
    }
    let rest = text.strip_prefix('"').ok_or_else(bad)?;
    // The closing quote: the last one not escaped.
    let mut end = None;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => end = Some(i),
            _ => escaped = false,
        }
        if c != '\\' {
            escaped = false;
        }
    }
    let end = end.ok_or_else(bad)?;
    let value = unescape(&rest[..end]).ok_or_else(bad)?;
    let suffix = &rest[end + 1..];
    if suffix.is_empty() {
        return Ok(Literal::new_simple_literal(value).into());
    }
    if let Some(language) = suffix.strip_prefix('@') {
        return Literal::new_language_tagged_literal(value, language)
            .map(Term::from)
            .map_err(|_| bad());
    }
    let datatype = suffix
        .strip_prefix("^^<")
        .and_then(|d| d.strip_suffix('>'))
        .ok_or_else(bad)?;
    let datatype = NamedNode::new(unescape(datatype).ok_or_else(bad)?).map_err(|_| bad())?;
    Ok(Literal::new_typed_literal(value, datatype).into())
}

/// N-Triples string escapes undone.
fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            't' => out.push('\t'),
            'b' => out.push('\u{8}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            'f' => out.push('\u{c}'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            '\\' => out.push('\\'),
            'u' => out.push(hex(&mut chars, 4)?),
            'U' => out.push(hex(&mut chars, 8)?),
            _ => return None,
        }
    }
    Some(out)
}

fn hex(chars: &mut std::str::Chars<'_>, digits: usize) -> Option<char> {
    let code: String = chars.take(digits).collect();
    char::from_u32(u32::from_str_radix(&code, 16).ok()?)
}

/// The graphs of the `context` parameters (`null`: the default graph).
fn contexts(pairs: &[(String, String)]) -> Result<Vec<GraphName>, ApiError> {
    pairs
        .iter()
        .filter(|(key, _)| key == "context")
        .map(|(_, value)| match value.trim() {
            "null" => Ok(GraphName::DefaultGraph),
            other => match term(other)? {
                Term::NamedNode(n) => Ok(GraphName::NamedNode(n)),
                Term::BlankNode(b) => Ok(GraphName::BlankNode(b)),
                _ => Err(ApiError::bad_request(format!(
                    "context '{other}' is neither an IRI, a blank node nor null"
                ))),
            },
        })
        .collect()
}

/// The statement pattern of `subj`, `pred`, `obj` and `context`.
fn pattern(pairs: &[(String, String)]) -> Result<StatementPattern, ApiError> {
    let optional = |name: &str| param(pairs, name).map(term).transpose();
    let predicate = match optional("pred")? {
        None => None,
        Some(Term::NamedNode(n)) => Some(n),
        Some(_) => return Err(ApiError::bad_request("pred must be an IRI")),
    };
    Ok(StatementPattern {
        subject: optional("subj")?,
        predicate,
        object: optional("obj")?,
        contexts: contexts(pairs)?,
    })
}

fn infer(pairs: &[(String, String)]) -> bool {
    param(pairs, "infer") != Some("false")
}

/// An RDF request body.
fn payload(headers: &HeaderMap, body: &Bytes) -> Result<RdfPayload, ApiError> {
    Ok(RdfPayload {
        payload: body.to_vec(),
        format: parse_graph_content_format(header_value_str(headers.get(header::CONTENT_TYPE)))?,
        base_iri: parse_rdf_base_iri(headers),
    })
}

/// `sesame:wildcard`: in a transaction's `DELETE` payload, any value at that position.
const WILDCARD: &str = "http://www.openrdf.org/schema/sesame#wildcard";
/// `rdf4j:nil`: in a `DELETE` payload's graph position, the default graph.
const NIL: &str = "http://rdf4j.org/schema/rdf4j#nil";

/// A transaction's `DELETE`: each statement of the payload a pattern, as RDF4J's server
/// reads it (its client removes this way, `clear()` too): `sesame:wildcard` matches any
/// value, a statement without a graph removes from every graph (or from the `context`
/// parameters), one in `rdf4j:nil` from the default graph. With `preserveNodeId=true`,
/// blank nodes refer to the store's.
fn removals(
    pairs: &[(String, String)],
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<Vec<StatementOp>, ApiError> {
    let data = payload(headers, body)?;
    let preserve = param(pairs, "preserveNodeId").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let parse = match preserve {
        true => nrese_store::parse_payload_preserving_blank_nodes,
        false => nrese_store::parse_payload,
    };
    let quads = parse(data.format, data.base_iri.as_deref(), &data.payload)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let given = contexts(pairs)?;
    let wildcard = |term: &Term| matches!(term, Term::NamedNode(node) if node.as_str() == WILDCARD);
    Ok(quads
        .into_iter()
        .map(|quad| {
            let subject: Term = quad.subject.into();
            let predicate = (quad.predicate.as_str() != WILDCARD).then_some(quad.predicate);
            let contexts = match quad.graph_name {
                GraphName::DefaultGraph => given.clone(),
                GraphName::NamedNode(node) if node.as_str() == NIL => vec![GraphName::DefaultGraph],
                graph => vec![graph],
            };
            StatementOp::RemoveMatching(StatementPattern {
                subject: (!wildcard(&subject)).then_some(subject),
                predicate,
                object: (!wildcard(&quad.object)).then_some(quad.object),
                contexts,
            })
        })
        .collect())
}

/// Whether the body is a SPARQL update (a form with `update`, or `sparql-update`).
fn is_update(headers: &HeaderMap) -> bool {
    let content_type = header_value_str(headers.get(header::CONTENT_TYPE));
    media_type_matches(content_type, "application/x-www-form-urlencoded")
        || media_type_matches(content_type, "application/sparql-update")
}

fn update(
    raw: &RawQuery,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<SparqlUpdateRequest, ApiError> {
    let operation = update_from_post(raw.0.as_deref(), headers.get(header::CONTENT_TYPE), body)?;
    Ok(SparqlUpdateRequest {
        update: operation.update,
        using_graphs: operation.using_graphs,
        using_named_graphs: operation.using_named_graphs,
        access: None,
    })
}

async fn apply(state: &AppState, ops: Vec<StatementOp>) -> Result<(), ApiError> {
    mutation::run(
        state,
        MutationCommand::Statements(StatementsRequest { ops }),
        state.policy().timeouts.update,
        "statement operation exceeded policy timeout",
    )
    .await
    .map(|_| ())
}

/// A table of terms as SPARQL results, JSON unless the client prefers XML.
fn table(headers: &HeaderMap, vars: &[&str], rows: Vec<Vec<Option<Term>>>) -> Response {
    let accept = accept_header_value(headers).unwrap_or_default();
    let xml = accept.contains("sparql-results+xml") && !accept.contains("sparql-results+json");
    if xml {
        let mut out = String::from(
            "<?xml version=\"1.0\"?>\n<sparql xmlns=\"http://www.w3.org/2005/sparql-results#\">\n<head>",
        );
        for var in vars {
            out.push_str(&format!("<variable name=\"{var}\"/>"));
        }
        out.push_str("</head>\n<results>\n");
        for row in rows {
            out.push_str("<result>");
            for (var, value) in vars.iter().zip(row) {
                let Some(value) = value else { continue };
                out.push_str(&format!("<binding name=\"{var}\">"));
                out.push_str(&match value {
                    Term::NamedNode(n) => format!("<uri>{}</uri>", xml_escape(n.as_str())),
                    Term::BlankNode(b) => format!("<bnode>{}</bnode>", xml_escape(b.as_str())),
                    Term::Literal(l) => match (l.language(), l.datatype().as_str()) {
                        (Some(language), _) => format!(
                            "<literal xml:lang=\"{}\">{}</literal>",
                            xml_escape(language),
                            xml_escape(l.value())
                        ),
                        (None, "http://www.w3.org/2001/XMLSchema#string") => {
                            format!("<literal>{}</literal>", xml_escape(l.value()))
                        }
                        (None, datatype) => format!(
                            "<literal datatype=\"{}\">{}</literal>",
                            xml_escape(datatype),
                            xml_escape(l.value())
                        ),
                    },
                    other => format!("<literal>{}</literal>", xml_escape(&other.to_string())),
                });
                out.push_str("</binding>");
            }
            out.push_str("</result>\n");
        }
        out.push_str("</results>\n</sparql>\n");
        return (
            [(header::CONTENT_TYPE, "application/sparql-results+xml")],
            out,
        )
            .into_response();
    }
    let bindings: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            let mut binding = serde_json::Map::new();
            for (var, value) in vars.iter().zip(row) {
                let Some(value) = value else { continue };
                let json = match value {
                    Term::NamedNode(n) => serde_json::json!({"type": "uri", "value": n.as_str()}),
                    Term::BlankNode(b) => serde_json::json!({"type": "bnode", "value": b.as_str()}),
                    Term::Literal(l) => match (l.language(), l.datatype().as_str()) {
                        (Some(language), _) => serde_json::json!(
                            {"type": "literal", "value": l.value(), "xml:lang": language}
                        ),
                        (None, "http://www.w3.org/2001/XMLSchema#string") => {
                            serde_json::json!({"type": "literal", "value": l.value()})
                        }
                        (None, datatype) => serde_json::json!(
                            {"type": "literal", "value": l.value(), "datatype": datatype}
                        ),
                    },
                    other => serde_json::json!({"type": "literal", "value": other.to_string()}),
                };
                binding.insert((*var).to_owned(), json);
            }
            serde_json::Value::Object(binding)
        })
        .collect();
    let document = serde_json::json!({"head": {"vars": vars}, "results": {"bindings": bindings}});
    (
        [(header::CONTENT_TYPE, "application/sparql-results+json")],
        document.to_string(),
    )
        .into_response()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn literal(text: impl Into<String>) -> Option<Term> {
    Some(Literal::new_simple_literal(text.into()).into())
}

fn text(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

pub async fn protocol() -> Response {
    text(PROTOCOL.to_owned())
}

pub async fn repositories(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let writable = state.runtime_posture().sparql_update_enabled;
    let ids = std::iter::once((DEFAULT_REPOSITORY.to_owned(), Some("NRESE".to_owned())))
        .chain(state.repositories().list());
    let rows = ids
        .map(|(id, title)| {
            vec![
                literal(format!("/repositories/{id}")),
                literal(id.as_str()),
                literal(title.unwrap_or_else(|| format!("NRESE: {id}"))),
                Some(Literal::from(true).into()),
                Some(Literal::from(writable).into()),
            ]
        })
        .collect();
    Ok(table(
        &headers,
        &["uri", "id", "title", "readable", "writable"],
        rows,
    ))
}

/// Creates repository `id` (`PUT /repositories/{id}`) with the settings of the
/// configuration in the body (RDF, Turtle when no type is given).
pub async fn repository_put(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_admin_write(&state, &headers).await?;
    let settings = match body.iter().all(u8::is_ascii_whitespace) {
        true => crate::repository_config::RepositorySettings::default(),
        false => {
            let content_type = header_value_str(headers.get(header::CONTENT_TYPE));
            let format = match content_type {
                None => nrese_store::GraphResultFormat::Turtle,
                Some(_) => parse_graph_content_format(content_type)?,
            };
            let quads = nrese_store::parse_payload(format, None, &body).map_err(|error| {
                ApiError::bad_request(format!("repository configuration: {error}"))
            })?;
            crate::repository_config::from_config(&id, &quads).map_err(ApiError::bad_request)?
        }
    };
    let repositories = state.clone();
    tokio::task::spawn_blocking(move || repositories.repositories().create(&id, settings))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes repository `id` and its data (`DELETE /repositories/{id}`).
pub async fn repository_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    guard::enforce_admin_write(&state, &headers).await?;
    state.repositories().delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn query_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    let operation = query_from_url(raw.0.as_deref())?;
    sparql::execute_query(state, operation, accept_header_value(&headers)).await
}

pub async fn query_post(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let state = state.for_repository(&id)?;
    // RDF4J sends updates to the statements path, but some clients post them here.
    let content_type = header_value_str(headers.get(header::CONTENT_TYPE));
    let form_update = media_type_matches(content_type, "application/x-www-form-urlencoded")
        && serde_urlencoded::from_bytes::<Vec<(String, String)>>(&body)
            .is_ok_and(|pairs| pairs.iter().any(|(key, _)| key == "update"));
    if form_update || media_type_matches(content_type, "application/sparql-update") {
        guard::enforce_update_write(&state, &headers).await?;
        let request = update(&raw, &headers, &body)?;
        apply(&state, vec![StatementOp::Update(request)]).await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    guard::enforce_query_read(&state, &headers).await?;
    let operation = query_from_post(raw.0.as_deref(), headers.get(header::CONTENT_TYPE), &body)?;
    sparql::execute_query(state, operation, accept_header_value(&headers)).await
}

pub async fn statements_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_graph_read(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    statements(state, raw, headers, None).await
}

/// The statements matching the request's pattern, on the committed data or as `pending`
/// would leave it.
async fn statements(
    state: AppState,
    raw: RawQuery,
    headers: HeaderMap,
    pending: Option<StatementsRequest>,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let pairs = pairs(&raw)?;
    let pattern = pattern(&pairs)?;
    let format = negotiated(header_value_str(headers.get(header::ACCEPT)), GRAPHS)?;
    let store = state.store();
    let infer = infer(&pairs);
    let body = tokio::task::spawn_blocking(move || {
        let quads = match &pending {
            None => store.read_statements(&pattern, infer)?,
            Some(pending) => store.read_statements_pending(pending, &pattern, infer)?,
        };
        nrese_store::statements::serialize_statements(format, quads)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.media_type()),
    );
    Ok(response)
}

pub async fn statements_post(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    let op = if is_update(&headers) {
        StatementOp::Update(update(&raw, &headers, &body)?)
    } else {
        state.policy().enforce_rdf_upload_bytes(body.len())?;
        StatementOp::Add {
            data: payload(&headers, &body)?,
            contexts: contexts(&pairs(&raw)?)?,
        }
    };
    apply(&state, vec![op]).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn statements_put(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    state.policy().enforce_rdf_upload_bytes(body.len())?;
    let contexts = contexts(&pairs(&raw)?)?;
    let ops = vec![
        StatementOp::RemoveMatching(StatementPattern {
            contexts: contexts.clone(),
            ..StatementPattern::default()
        }),
        StatementOp::Add {
            data: payload(&headers, &body)?,
            contexts,
        },
    ];
    apply(&state, ops).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn statements_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    let pattern = pattern(&pairs(&raw)?)?;
    apply(&state, vec![StatementOp::RemoveMatching(pattern)]).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn size(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    count(state, raw, None).await
}

/// The number of statements (in the request's contexts), on the committed data or as
/// `pending` would leave it.
async fn count(
    state: AppState,
    raw: RawQuery,
    pending: Option<StatementsRequest>,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let pairs = pairs(&raw)?;
    let pattern = StatementPattern {
        contexts: contexts(&pairs)?,
        ..StatementPattern::default()
    };
    let store = state.store();
    let infer = infer(&pairs);
    let count = tokio::task::spawn_blocking(move || match &pending {
        None => Ok(store.count_statements(&pattern, infer)),
        Some(pending) => store.count_statements_pending(pending, &pattern, infer),
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(text(count.to_string()))
}

pub async fn contexts_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    state.ensure_serving()?;
    let rows = state
        .store()
        .contexts()
        .into_iter()
        .map(|graph| vec![Some(graph)])
        .collect();
    Ok(table(&headers, &["contextID"], rows))
}

pub async fn namespaces_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    let rows = state
        .rdf4j()
        .namespaces
        .lock()
        .iter()
        .map(|(prefix, iri)| vec![literal(prefix.as_str()), literal(iri.as_str())])
        .collect();
    Ok(table(&headers, &["prefix", "namespace"], rows))
}

pub async fn namespaces_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    state.rdf4j().change_namespaces(BTreeMap::clear)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn namespace_get(
    State(state): State<AppState>,
    Path((id, prefix)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    match state.rdf4j().namespaces.lock().get(&prefix) {
        Some(iri) => Ok(text(iri.clone())),
        None => Err(ApiError::not_found(format!("no namespace '{prefix}'"))),
    }
}

pub async fn namespace_put(
    State(state): State<AppState>,
    Path((id, prefix)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    let iri = std::str::from_utf8(&body)
        .map_err(|_| ApiError::bad_request("the namespace must be UTF-8"))?
        .trim()
        .to_owned();
    if iri.is_empty() {
        return Err(ApiError::bad_request("the namespace is empty"));
    }
    state.rdf4j().change_namespaces(|namespaces| {
        namespaces.insert(prefix, iri);
    })?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn namespace_delete(
    State(state): State<AppState>,
    Path((id, prefix)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    state.rdf4j().change_namespaces(|namespaces| {
        namespaces.remove(&prefix);
    })?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn graph_store_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let state = state.for_repository(&id)?;
    graph_store::get_graph(state, raw, headers).await
}

pub async fn graph_store_put(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let state = state.for_repository(&id)?;
    graph_store::put_graph(state, raw, headers, body).await
}

pub async fn graph_store_post(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let state = state.for_repository(&id)?;
    graph_store::post_graph(state, raw, headers, body).await
}

pub async fn graph_store_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let state = state.for_repository(&id)?;
    graph_store::delete_graph(state, raw, headers).await
}

/// `path` as an absolute URL of this server as the request reached it (the `Host` header,
/// or a proxy's `X-Forwarded-Host` and `X-Forwarded-Proto`): RDF4J's client follows a new
/// transaction's `Location` as given and fails on a relative one ("Target host is not
/// specified"). Without a host, the path alone.
fn absolute(headers: &HeaderMap, path: &str) -> String {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let Some(host) = value("x-forwarded-host").or_else(|| value("host")) else {
        return path.to_owned();
    };
    let scheme = value("x-forwarded-proto").unwrap_or("http");
    format!("{scheme}://{host}{path}")
}

pub async fn transaction_begin(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    let rdf4j = state.rdf4j();
    let number = rdf4j.next_transaction.fetch_add(1, Ordering::Relaxed);
    let txid = format!("tx-{number}");
    let mut open = rdf4j.transactions.lock();
    open.retain(|_, pending| pending.touched.elapsed() < TRANSACTION_IDLE);
    open.insert(
        txid.clone(),
        Pending {
            ops: Vec::new(),
            touched: Instant::now(),
        },
    );
    let location = absolute(&headers, &format!("/repositories/{id}/transactions/{txid}"));
    let mut response = StatusCode::CREATED.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(|error| ApiError::internal(error.to_string()))?,
    );
    Ok(response)
}

pub async fn transaction_action(
    State(state): State<AppState>,
    Path((id, txid)): Path<(String, String)>,
    raw: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    let pairs = pairs(&raw)?;
    let action = param(&pairs, "action")
        .unwrap_or_default()
        .to_ascii_uppercase();
    {
        let rdf4j = state.rdf4j();
        let mut open = rdf4j.transactions.lock();
        let pending = open
            .get_mut(&txid)
            .ok_or_else(|| ApiError::not_found(format!("no transaction '{txid}'")))?;
        pending.touched = Instant::now();
        let ops = match action.as_str() {
            "ADD" => Some(vec![StatementOp::Add {
                data: payload(&headers, &body)?,
                contexts: contexts(&pairs)?,
            }]),
            "DELETE" => Some(removals(&pairs, &headers, &body)?),
            "UPDATE" => Some(vec![StatementOp::Update(update(&raw, &headers, &body)?)]),
            _ => None,
        };
        if let Some(ops) = ops {
            state.policy().enforce_rdf_upload_bytes(body.len())?;
            pending.ops.extend(ops);
            return Ok(StatusCode::OK.into_response());
        }
    }
    match action.as_str() {
        "COMMIT" => {
            let pending = state
                .rdf4j()
                .transactions
                .lock()
                .remove(&txid)
                .ok_or_else(|| ApiError::not_found(format!("no transaction '{txid}'")))?;
            apply(&state, pending.ops).await?;
            Ok(StatusCode::OK.into_response())
        }
        "PING" => Ok(text(TRANSACTION_IDLE.as_millis().to_string())),
        // Reads see the transaction's changes (module docs).
        "QUERY" | "GET" | "SIZE" => {
            let pending = state
                .rdf4j()
                .transactions
                .lock()
                .get(&txid)
                .map(|pending| StatementsRequest {
                    ops: pending.ops.clone(),
                })
                .ok_or_else(|| ApiError::not_found(format!("no transaction '{txid}'")))?;
            match action.as_str() {
                "QUERY" => {
                    let operation = if body.is_empty() {
                        query_from_url(raw.0.as_deref())?
                    } else {
                        query_from_post(raw.0.as_deref(), headers.get(header::CONTENT_TYPE), &body)?
                    };
                    let accept = accept_header_value(&headers);
                    sparql::execute_query_in(state, operation, accept, Some(pending)).await
                }
                "GET" => statements(state, raw, headers, Some(pending)).await,
                _ => count(state, raw, Some(pending)).await,
            }
        }
        other => Err(ApiError::bad_request(format!(
            "unknown transaction action '{other}'"
        ))),
    }
}

pub async fn transaction_rollback(
    State(state): State<AppState>,
    Path((id, txid)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let state = state.for_repository(&id)?;
    match state.rdf4j().transactions.lock().remove(&txid) {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(ApiError::not_found(format!("no transaction '{txid}'"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_as_in_n_triples() {
        assert_eq!(
            term("<http://example.com/a>").unwrap(),
            Term::from(NamedNode::new("http://example.com/a").unwrap())
        );
        assert_eq!(
            term("\"say \\\"hi\\\"\"@en").unwrap(),
            Term::from(Literal::new_language_tagged_literal("say \"hi\"", "en").unwrap())
        );
        assert_eq!(
            term("\"1\"^^<http://www.w3.org/2001/XMLSchema#int>").unwrap(),
            Term::from(Literal::new_typed_literal(
                "1",
                NamedNode::new("http://www.w3.org/2001/XMLSchema#int").unwrap()
            ))
        );
        assert_eq!(
            term("\"caf\\u00E9\"").unwrap(),
            Term::from(Literal::new_simple_literal("café"))
        );
        assert!(matches!(term("_:b1").unwrap(), Term::BlankNode(_)));
        assert!(term("plain").is_err());
        assert!(term("\"open").is_err());
    }

    #[test]
    fn contexts_with_null() {
        let pairs = vec![
            ("context".to_owned(), "null".to_owned()),
            ("context".to_owned(), "<http://example.com/g>".to_owned()),
        ];
        let graphs = contexts(&pairs).unwrap();
        assert_eq!(graphs.len(), 2);
        assert_eq!(graphs[0], GraphName::DefaultGraph);
    }
}
