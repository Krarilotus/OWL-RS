//! Dictionary lifecycle V1 (`notes/2026-11-09-dict-v1-speculative-private-design.md` §4,
//! item 1): a request's speculative work appends none of its own terms to the shared
//! dictionary. The session read and the SHACL preview with the request's own shapes (the DL
//! routes are in `dl::speculative`; the OWL 2 RL refutation's row comes with V1b, design note
//! §8.3). Each is red on the parent, whose speculative transaction interns into the dictionary.

use crate::support::Writes;
use nrese_store::{
    CancellationToken, GraphResultFormat, PreparedQuery, ReadScope, ShaclValidationRequest,
    ShapesSource, SparqlQueryRequest, SparqlUpdateRequest, StatementOp, StatementsRequest,
    StoreConfig, StoreService,
};

const EX: &str = "http://example.com/";

fn store() -> StoreService {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .execute_update_str(&format!(
            "PREFIX : <{EX}> INSERT DATA {{ :a a :B . :a :p :b . }}"
        ))
        .unwrap();
    store
}

fn terms(store: &StoreService) -> u64 {
    store.engine_stats().dictionary.terms
}

/// - Input: a session read: `INSERT DATA` of a statement with three new terms pending, and a
///   `SELECT` over it.
/// - Expected: the pending statement's object as the answer; the dictionary as before.
#[test]
fn a_session_read_appends_none_of_its_terms() {
    let store = store();
    let before = terms(&store);
    let pending = StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
        format!("PREFIX : <{EX}> INSERT DATA {{ :s1 :q1 \"session only\" . }}"),
    ))]);
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(format!(
        "PREFIX : <{EX}> SELECT ?o {{ :s1 :q1 ?o }}"
    )))
    .unwrap();
    let mut out = Vec::new();
    store
        .run_query_pending(&pending, &prepared, &CancellationToken::new(), &mut out)
        .unwrap();
    assert!(
        String::from_utf8(out).unwrap().contains("session only"),
        "the pending statement is read"
    );
    assert_eq!(terms(&store), before, "the session read appended terms");
}

/// - Input: a validation with the request's own shapes (a shape and a message no data has).
/// - Expected: the violation of `:a` (no `:r`), and the dictionary as before.
#[test]
fn a_shacl_preview_appends_none_of_its_shapes_terms() {
    let store = store();
    let before = terms(&store);
    let shapes = format!(
        "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix : <{EX}> . \
         :OnlyShape a sh:NodeShape ; sh:targetClass :B ; \
         sh:property [ sh:path :r ; sh:minCount 1 ; sh:message \"shape only\" ] ."
    );
    let validation = store
        .validate_shacl(
            &ReadScope::All,
            &ShaclValidationRequest {
                shapes: ShapesSource::Payload {
                    format: GraphResultFormat::Turtle,
                    base_iri: None,
                    payload: shapes.into_bytes(),
                },
                ..ShaclValidationRequest::default()
            },
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(validation.results_text().len(), 1, "a lacks :r");
    assert_eq!(terms(&store), before, "the preview appended terms");
}
