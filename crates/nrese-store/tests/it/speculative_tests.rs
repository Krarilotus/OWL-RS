//! Dictionary lifecycle V1 (`notes/2026-11-09-dict-v1-speculative-private-design.md` §4,
//! item 1): a request's speculative work appends none of its own terms to the shared
//! dictionary. The session read and the SHACL preview with the request's own shapes (the DL
//! routes are in `dl::speculative`; the OWL 2 RL refutation's row comes with V1b, design note
//! §8.3). Each is red on the parent, whose speculative transaction interns into the dictionary.

use crate::support::Writes;
use nrese_store::{
    CancellationToken, GraphResultFormat, PreparedQuery, ReadScope, SearchScope,
    ShaclValidationRequest, ShapesSource, SparqlQueryRequest, SparqlUpdateRequest, StatementOp,
    StatementsRequest, StoreConfig, StoreService,
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

/// The answer to `query` read in a session whose pending updates are `pending` (none: as a
/// query outside a session).
fn read_pending(store: &StoreService, pending: &[String], query: &str) -> String {
    let pending = StatementsRequest::new(
        pending
            .iter()
            .map(|update| StatementOp::Update(SparqlUpdateRequest::new(update.clone())))
            .collect::<Vec<_>>(),
    );
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(query.to_owned())).unwrap();
    let mut out = Vec::new();
    store
        .run_query_pending(&pending, &prepared, &CancellationToken::new(), &mut out)
        .unwrap();
    String::from_utf8(out).unwrap()
}

// Search inside the private routes (V3's "For V1" rows, notes/2026-11-09-dict-v3-handback.md;
// design note §4 item 3; review dict-v1c r1, R1). V3's oracle: a store holding the same
// statements, committed. The text rows' histories keep no literal the data has stopped using,
// as on a private snapshot the global entry points answer through the reader path.

const BDS: &str = "http://www.bigdata.com/rdf/search#";

/// An in-memory store under `scope` with `updates` committed, one by one.
fn store_with(scope: SearchScope, updates: &[String]) -> StoreService {
    let store = StoreService::new(StoreConfig {
        search_scope: scope,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    for update in updates {
        store.execute_update_str(update).unwrap();
    }
    store
}

/// V3 row 1: a session's SPARQL text search over its pending state.
///
/// - Input: under `search_scope` global, then reader: a store holding `"alpha one two"`,
///   `"beta"` and eight fillers; a session whose pending update adds `"alpha beta session"` and
///   `"beta words"`, reading every subject whose label matches `alpha beta`, with relevance.
/// - Expected: the answer of a store holding all of it committed, the session's own literals
///   among the matches; afterwards, the same query outside the session answers as a store that
///   never saw the session, and the dictionary is as before.
#[test]
fn a_session_text_search_answers_as_its_statements_committed() {
    let base = {
        let fillers: String = (0..8)
            .map(|i| format!(":f{i} :label \"filler {i}\" . "))
            .collect();
        format!(
            "PREFIX : <{EX}> INSERT DATA {{ :a :label \"alpha one two\" . :b :label \"beta\" . {fillers}}}"
        )
    };
    let session = format!(
        "PREFIX : <{EX}> INSERT DATA {{ :s :label \"alpha beta session\" . :t :label \"beta words\" . }}"
    );
    let query = format!(
        "PREFIX : <{EX}> PREFIX bds: <{BDS}> SELECT ?s ?r WHERE {{ ?s :label ?o . \
         ?o bds:search \"alpha beta\" ; bds:relevance ?r }} ORDER BY ?s"
    );
    for scope in [SearchScope::Global, SearchScope::Reader] {
        let store = store_with(scope, std::slice::from_ref(&base));
        let before = terms(&store);
        let got = read_pending(&store, std::slice::from_ref(&session), &query);
        let truth = store_with(scope, &[base.clone(), session.clone()]);
        assert_eq!(got, read_pending(&truth, &[], &query), "{scope:?}");
        assert!(
            got.contains(&format!("{EX}s")) && got.contains(&format!("{EX}t")),
            "{scope:?}: {got}"
        );
        let never = store_with(scope, std::slice::from_ref(&base));
        assert_eq!(
            read_pending(&store, &[], &query),
            read_pending(&never, &[], &query),
            "{scope:?}: the shared index after the session"
        );
        assert_eq!(
            terms(&store),
            before,
            "{scope:?}: the session appended terms"
        );
    }
}

/// V3 row 2: a SHACL preview's search over its pending state.
///
/// - Input: under `search_scope` global, then reader: a store holding `:a :label "alpha one
///   two"` and `:b :label "beta"`; a preview with the request's own shapes: a SPARQL constraint
///   on every subject of `:label` that reports a label matching `alpha` (a text search), and
///   in the shapes' own graph a literal `"alpha ghost words"` no data has, an own term the
///   search's statistics count.
/// - Expected: the report of a store holding those shapes committed in a graph of their own
///   and validated against that graph: `:a`'s one result; the dictionary as before.
#[test]
fn a_shacl_preview_text_search_answers_as_its_shapes_committed() {
    let base = format!(
        "PREFIX : <{EX}> INSERT DATA {{ :a :label \"alpha one two\" . :b :label \"beta\" . }}"
    );
    let select = format!(
        "SELECT $this ?value WHERE {{ $this <{EX}label> ?value . ?value <{BDS}search> \"alpha\" }}"
    );
    let triples = format!(
        ":Labelled a sh:NodeShape ; sh:targetSubjectsOf :label ; sh:sparql :AlphaCheck . \
         :AlphaCheck sh:message \"an alpha label\" ; sh:select '''{select}''' . \
         :ghost :label \"alpha ghost words\" ."
    );
    let shapes =
        format!("@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix : <{EX}> . {triples}");
    let graph = format!("{EX}committed-shapes");
    let committed = format!(
        "PREFIX sh: <http://www.w3.org/ns/shacl#> PREFIX : <{EX}> \
         INSERT DATA {{ GRAPH <{graph}> {{ {triples} }} }}"
    );
    let report = |store: &StoreService, shapes: ShapesSource| {
        let validation = store
            .validate_shacl(
                &ReadScope::All,
                &ShaclValidationRequest {
                    shapes,
                    ..ShaclValidationRequest::default()
                },
                &CancellationToken::new(),
            )
            .unwrap();
        let mut results: Vec<String> = validation
            .results_text()
            .iter()
            .map(|result| format!("{result:?}"))
            .collect();
        results.sort();
        results
    };
    for scope in [SearchScope::Global, SearchScope::Reader] {
        let store = store_with(scope, std::slice::from_ref(&base));
        let before = terms(&store);
        let got = report(
            &store,
            ShapesSource::Payload {
                format: GraphResultFormat::Turtle,
                base_iri: None,
                payload: shapes.clone().into_bytes(),
            },
        );
        let truth = store_with(scope, &[base.clone(), committed.clone()]);
        assert_eq!(
            got,
            report(&truth, ShapesSource::Graph(graph.clone())),
            "{scope:?}"
        );
        assert_eq!(got.len(), 1, "{scope:?}: a's alpha label: {got:?}");
        assert_eq!(
            terms(&store),
            before,
            "{scope:?}: the preview appended terms"
        );
    }
}

/// `"[1, y]"^^nrv:vector`, at a cosine distance from `[1, 0]` that grows with `y`.
fn vector(y: &str) -> String {
    format!("\"[1, {y}]\"^^<urn:nrese:vector:vector>")
}

/// V3 row 3 (SRCH's addition 1): a session's `nrv:search` whose `k` nearest, of the vectors
/// the query's graph uses, are reached only by widening, and are its own. Under `global`, a
/// private snapshot searches the vectors of every graph and the query passes over those its
/// graph doesn't use, widening `k` until it covers the space it is told: a space that left the
/// own vectors out stops short (sol-loop r3 S1).
///
/// - Input: a store whose five vectors nearest to `[1, 0]` are used in another graph only, and
///   one far vector in `:g`; a session whose pending update adds two vectors of its own to
///   `:g`, nearer than the far one; the two nearest `FROM :g`, exactly.
/// - Expected: the session's two vectors, as a store holding those statements committed
///   answers; afterwards, the same query outside the session answers as a store that never
///   saw the session, and the dictionary is as before.
#[test]
fn a_session_vector_search_widens_past_another_graphs_vectors_to_its_own() {
    let near: String = ["0.01", "0.02", "0.03", "0.04", "0.05"]
        .iter()
        .enumerate()
        .map(|(i, y)| format!(":n{i} :embedding {} . ", vector(y)))
        .collect();
    let base = format!(
        "PREFIX : <{EX}> INSERT DATA {{ GRAPH :other {{ {near} }} GRAPH :g {{ :far :embedding {} . }} }}",
        vector("0.9")
    );
    let session = format!(
        "PREFIX : <{EX}> INSERT DATA {{ GRAPH :g {{ :o1 :embedding {} . :o2 :embedding {} . }} }}",
        vector("0.2"),
        vector("0.3")
    );
    let query = format!(
        "PREFIX nrv: <urn:nrese:vector:> SELECT ?v FROM <{EX}g> WHERE {{ SERVICE nrv:search {{ \
         ?v nrv:near {} ; nrv:k 2 ; nrv:exact true ; nrv:rank ?rank }} }} ORDER BY ?rank",
        vector("0")
    );
    let store = store_with(SearchScope::default(), std::slice::from_ref(&base));
    let before = terms(&store);
    let got = read_pending(&store, std::slice::from_ref(&session), &query);
    let truth = store_with(SearchScope::default(), &[base.clone(), session.clone()]);
    assert_eq!(got, read_pending(&truth, &[], &query));
    assert!(
        got.contains("[1, 0.2]") && got.contains("[1, 0.3]"),
        "{got}"
    );
    let never = store_with(SearchScope::default(), std::slice::from_ref(&base));
    assert_eq!(
        read_pending(&store, &[], &query),
        read_pending(&never, &[], &query),
        "the shared index after the session"
    );
    assert_eq!(terms(&store), before, "the session appended terms");
}
