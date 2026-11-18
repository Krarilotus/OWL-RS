//! `GET /dataset/classification` and `GET /dataset/realisation`: the class hierarchy and
//! the individuals' types of the asserted ontology under OWL 2 DL, computed on request by
//! the DL engines (kept per revision), or by the EL classifier where an irregular role
//! hierarchy is all the DL engines leave out (`engine` `el`). JSON by default, with the consistency verdict
//! (`consistent`: `true`, `false` or `null` where undecided) and whether the result is
//! complete; N-Triples on request (`rdfs:subClassOf` statements with `owl:Nothing` as the
//! superclass of unsatisfiable classes; `rdf:type` statements; an inconsistent ontology
//! as the one statement `owl:Thing rdfs:subClassOf owl:Nothing`). Both forms carry the
//! `NRESE-Completeness` header, so an undecided or incomplete answer is never unmarked.
//! Nor is one a procedure outside OWL 2 DL gave: the header's `outside` and `by`, the
//! JSON's `outside_profile`, and a comment line first in N-Triples say so.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_reasoner::eval::Stop;
use nrese_sparql::{Completeness, Outside, Regime};
use nrese_store::dl::Verdict;

use crate::error::ApiError;
use crate::http::sparql::COMPLETENESS;
use crate::state::AppState;

const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The answer's status under OWL 2 DL: what it lists is entailed either way, so it is
/// sound; complete only without reasons, of which the store's report lists an undecided
/// consistency's (`nrese_store::ClassificationReport::incomplete`). Marked where a
/// procedure outside OWL 2 DL answered.
fn completeness(incomplete: &[String], outside: Option<Outside>) -> Completeness {
    let mut status = Completeness::under(Regime::Owl2Dl);
    for why in incomplete {
        status.incomplete("dl", why.clone());
    }
    status.outside = outside;
    status
}

/// The N-Triples form's first line where a procedure outside OWL 2 DL answered.
fn outside_comment(outside: &Outside) -> String {
    format!(
        "# Outside OWL 2 DL ({}); answered by {}. Guaranteed: {} Not guaranteed: {}\n",
        outside.what.join(", "),
        outside.by,
        outside.guaranteed,
        outside.not_guaranteed
    )
}

/// The verdict as JSON: `consistent` (`null` where undecided) and `consistency`.
fn verdict_json(consistency: &Verdict) -> (serde_json::Value, &'static str) {
    (
        serde_json::json!(consistency.decided()),
        consistency.as_str(),
    )
}

/// What the N-Triples form starts with: for an inconsistency, its one statement (every
/// class is below `owl:Nothing`; the report lists nothing else).
fn triples_head(consistency: &Verdict) -> String {
    match consistency {
        Verdict::Inconsistent => format!("<{THING}> <{SUB_CLASS_OF}> <{NOTHING}> .\n"),
        _ => String::new(),
    }
}

/// Whether the client asked for N-Triples.
fn wants_triples(headers: &HeaderMap) -> bool {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    accept.contains("application/n-triples") || accept.contains("text/plain")
}

/// Runs `work` off the async threads within the query timeout, with a stop that fires
/// when the request ends (at the timeout or when the client is gone).
async fn blocking<T: Send + 'static>(
    state: &AppState,
    timeout_message: &'static str,
    work: impl FnOnce(Stop<'_>) -> nrese_store::StoreResult<T> + Send + 'static,
) -> Result<T, ApiError> {
    let cancellation = nrese_store::CancellationToken::new();
    let token = cancellation.clone();
    let stop = move || token.is_cancelled();
    super::blocking::within(
        state.policy().timeouts.query,
        cancellation,
        timeout_message,
        move || work(&stop),
    )
    .await?
    .map_err(|error| ApiError::internal(error.to_string()))
}

fn respond(
    triples: bool,
    text: String,
    json: serde_json::Value,
    status: &Completeness,
) -> Result<Response, ApiError> {
    let (media_type, body) = if triples {
        ("application/n-triples", text.into_bytes())
    } else {
        (
            "application/json",
            serde_json::to_vec(&json).map_err(|error| ApiError::internal(error.to_string()))?,
        )
    };
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, media_type)],
        Body::from(body),
    )
        .into_response();
    // As a query's answers carry it (`http::sparql`), readable across origins too.
    let headers = response.headers_mut();
    headers.insert(
        COMPLETENESS,
        HeaderValue::from_str(&status.header())
            .unwrap_or_else(|_| HeaderValue::from_static(status.as_str())),
    );
    headers.append(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(COMPLETENESS),
    );
    Ok(response)
}

pub async fn classify(
    state: AppState,
    headers: &HeaderMap,
    scope: nrese_store::ReadScope,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let store = state.store();
    let report = blocking(
        &state,
        "classification exceeded policy timeout",
        move |stop| store.classify_until(&scope, stop),
    )
    .await?;
    let status = completeness(&report.incomplete, report.outside.clone());
    // Only the form asked for is built: each holds about as much memory as the answer (D16-700).
    if wants_triples(headers) {
        #[cfg(test)]
        tests::built(true);
        let mut text = report
            .outside
            .as_ref()
            .map_or_else(String::new, outside_comment);
        text.push_str(&triples_head(&report.consistency));
        for (sub, sup) in &report.subsumptions {
            text.push_str(&format!("<{sub}> <{SUB_CLASS_OF}> <{sup}> .\n"));
        }
        for class in &report.unsatisfiable {
            text.push_str(&format!("<{class}> <{SUB_CLASS_OF}> <{NOTHING}> .\n"));
        }
        return respond(true, text, serde_json::Value::Null, &status);
    }
    #[cfg(test)]
    tests::built(false);
    let (consistent, consistency) = verdict_json(&report.consistency);
    let mut json = serde_json::json!({
        "profile": "OWL 2 DL",
        "engine": report.engine,
        "complete": report.complete(),
        "incomplete": report.incomplete,
        "consistent": consistent,
        "consistency": consistency,
        "micros": report.micros,
        "subsumptions": report.subsumptions,
        "unsatisfiable": report.unsatisfiable,
        "equivalent_to_thing": report.equivalent_to_thing,
    });
    if let Some(outside) = &report.outside {
        json["outside_profile"] = serde_json::json!({
            "what": outside.what,
            "by": outside.by,
            "guaranteed": outside.guaranteed,
            "not_guaranteed": outside.not_guaranteed,
        });
    }
    respond(false, String::new(), json, &status)
}

pub async fn realise(
    state: AppState,
    headers: &HeaderMap,
    scope: nrese_store::ReadScope,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let store = state.store();
    let report = blocking(&state, "realisation exceeded policy timeout", move |stop| {
        store.realise_until(&scope, stop)
    })
    .await?;
    let status = completeness(&report.incomplete, None);
    if wants_triples(headers) {
        #[cfg(test)]
        tests::built(true);
        let mut text = triples_head(&report.consistency);
        for (individual, types) in &report.types {
            for class in types {
                text.push_str(&format!("<{individual}> <{RDF_TYPE}> <{class}> .\n"));
            }
        }
        return respond(true, text, serde_json::Value::Null, &status);
    }
    #[cfg(test)]
    tests::built(false);
    let types: serde_json::Map<String, serde_json::Value> = report
        .types
        .iter()
        .map(|(individual, types)| (individual.clone(), serde_json::json!(types)))
        .collect();
    let (consistent, consistency) = verdict_json(&report.consistency);
    let json = serde_json::json!({
        "profile": "OWL 2 DL",
        "engine": report.engine,
        "complete": report.complete(),
        "incomplete": report.incomplete,
        "consistent": consistent,
        "consistency": consistency,
        "micros": report.micros,
        "types": types,
    });
    respond(false, String::new(), json, &status)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
    use nrese_reasoner::{ReasonerConfig, ReasonerService};
    use nrese_store::{ReadScope, StoreConfig, StoreService};

    use crate::ai::AiSuggestionService;
    use crate::policy::PolicyConfig;
    use crate::runtime_posture::DeploymentPosture;
    use crate::state::AppState;

    thread_local! {
        /// The forms built on this thread so far: (N-Triples, JSON).
        static BUILT: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    }

    /// Where a handler builds a form (N-Triples if `triples`, else JSON), it counts it here.
    pub(super) fn built(triples: bool) {
        BUILT.with(|b| {
            let (t, j) = b.get();
            b.set(if triples { (t + 1, j) } else { (t, j + 1) });
        });
    }

    const ONTOLOGY: &str = r#"
@prefix ex: <http://example.com/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Dog rdfs:subClassOf ex:Animal .
ex:rex a ex:Dog .
"#;

    /// A request for one form builds that form only, never the other as well: each form of
    /// a large answer holds about as much memory as the answer (D16-700). Counted by the
    /// builds, on the test's own thread, where the handler builds them; not timed.
    async fn one_form_built(realisation: bool) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("pets.ttl");
        std::fs::write(&path, ONTOLOGY).expect("the ontology written");
        let store = StoreService::new(StoreConfig::in_memory().with_ontology(path))
            .expect("the store with the ontology");
        let state = AppState::new(
            store,
            ReasonerService::new(ReasonerConfig::default()),
            PolicyConfig::default(),
            AiSuggestionService::disabled(),
            DeploymentPosture::OpenWorkbench,
        );
        state.mark_ready(); // startup over, as `main` marks it
        for (accept, want) in [
            ("application/n-triples", (1, 0)),
            ("application/json", (0, 1)),
        ] {
            BUILT.with(|b| b.set((0, 0)));
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT, HeaderValue::from_static(accept));
            let answer = match realisation {
                true => super::realise(state.clone(), &headers, ReadScope::All).await,
                false => super::classify(state.clone(), &headers, ReadScope::All).await,
            };
            let response = answer.unwrap_or_else(|e| panic!("no answer for {accept}: {e:?}"));
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                BUILT.with(Cell::get),
                want,
                "forms built (N-Triples, JSON) for {accept}"
            );
        }
    }

    #[tokio::test]
    async fn a_classification_request_builds_only_the_form_it_asked_for() {
        one_form_built(false).await;
    }

    #[tokio::test]
    async fn a_realisation_request_builds_only_the_form_it_asked_for() {
        one_form_built(true).await;
    }
}
