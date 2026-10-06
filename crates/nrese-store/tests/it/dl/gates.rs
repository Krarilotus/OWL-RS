//! The merge gate's requirements for G3 (checklist §4): every DL path has a budget, and
//! one that runs out never gives a wrong label; DL answers derive nothing from graphs
//! the reader can't read.

use std::sync::Arc;

use nrese_sparql::GraphAccess;
use nrese_store::{DlConfig, MutationPipeline, ReadScope, SparqlQueryRequest};

use super::queries::query;
use super::{PREFIXES, insert, pipeline, pipeline_with};

const UNION: &str = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
     :C rdfs:subClassOf :D . :x a :A . :y a :B . :w a [ owl:unionOf ( :B :C ) ] .";

#[test]
fn a_spent_deterministic_budget_leaves_candidates_unresolved_never_wrong() {
    // No branch point allowed: every hypertableau run that must branch gives up.
    let dl = pipeline_with(DlConfig {
        max_branch_points: 0,
        ..DlConfig::default()
    });
    insert(&dl, UNION).expect("U1 proves the data consistent");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    for row in &rows {
        assert!(["w", "x", "y"].contains(&row.as_str()), "unsound {row}");
    }
    assert!(
        rows.contains(&"y".to_owned()),
        "the RL closure's answer stands"
    );
    assert!(!status.is_complete());
    let b = status.bounds.expect("bounds");
    assert!(b.unresolved >= 1, "{b:?}");
    assert_eq!(b.refuted, 0, "a spent budget refutes nothing");
    assert!(
        status
            .reasons()
            .iter()
            .any(|r| r.contains("neither proved nor refuted")),
        "{:?}",
        status.reasons()
    );
    // Exact answers asked for: refused, naming the candidates left unresolved.
    let failed = super::queries::query_with(
        &dl,
        "SELECT ?x { ?x a :D }",
        Some(nrese_store::DlAnswers::Exact),
    );
    let Err(nrese_store::StoreError::Incomplete(answer)) = failed else {
        panic!("exact answers refused: {failed:?}");
    };
    assert_eq!(answer.unresolved.len() as u64, b.unresolved, "{answer:?}");
    assert!(
        answer
            .unresolved
            .iter()
            .all(|c| c == "?x=<http://example.com/x>" || c == "?x=<http://example.com/w>"),
        "{answer:?}"
    );
    assert_eq!(answer.status.regime, Some(nrese_sparql::Regime::Owl2Dl));
    // The same store with the default budgets decides them all.
    let decided = pipeline();
    insert(&decided, UNION).expect("data");
    let (rows, status) = query(&decided, "SELECT ?x { ?x a :D }");
    assert_eq!(rows, ["w", "x", "y"]);
    assert!(status.is_complete());
}

#[test]
fn a_spent_candidate_budget_leaves_the_rest_unresolved() {
    let dl = pipeline_with(DlConfig {
        max_candidates: 1,
        ..DlConfig::default()
    });
    insert(&dl, &format!("{UNION} :v a [ owl:unionOf ( :B :C ) ] .")).expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert!(!status.is_complete());
    let b = status.bounds.expect("bounds");
    assert_eq!((b.proved, b.unresolved), (1, 1), "{b:?}");
    assert_eq!(rows.len(), 3, "{rows:?}");
}

const EX: &str = "http://example.com/";

/// A reader of the default graph and `open`, with the inferred statements their graphs
/// support (`inferred = "supported"`) or all of them (`"visible"`).
fn reader(by_support: bool) -> ReadScope {
    ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open")],
        default_graph: true,
        inferred: true,
        inferred_by_support: by_support,
        ..GraphAccess::default()
    }))
}

fn read(dl: &MutationPipeline, scope: ReadScope, q: &str) -> (String, super::Status) {
    let store = dl.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::new(format!("{PREFIXES}{q}"), scope))
        .expect("prepared");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out)
        .expect("query")
        .expect("a status");
    (String::from_utf8(out).expect("utf8"), status)
}

#[test]
fn dl_answers_derive_nothing_from_graphs_the_reader_cannot_read() {
    let dl = pipeline();
    insert(
        &dl,
        "GRAPH :secret { :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . \
           :B rdfs:subClassOf :D . :C rdfs:subClassOf :D . :C2 rdfs:subClassOf :F } \
         GRAPH :open { :x a :A . :a a :C2 }",
    )
    .expect("data");
    // A reader of every graph: x is a D (only OWL 2 DL shows it), a an F (RL).
    let (all, status) = read(&dl, ReadScope::All, "SELECT ?x { ?x a :D }");
    assert!(all.contains("example.com/x"), "{all}");
    assert!(status.is_complete());
    for by_support in [true, false] {
        let (text, status) = read(&dl, reader(by_support), "SELECT ?x { ?x a :D }");
        // The only derivation of x a D runs through the secret schema.
        assert!(!text.contains("example.com/x"), "{text}");
        assert!(!status.is_complete());
        assert!(
            status.reasons()[0].contains("part of the data"),
            "{:?}",
            status.reasons()
        );
    }
    // The RL closure follows the configured policy: supported hides a's F (its only
    // derivation uses the secret graph), visible shows it.
    let (supported, _) = read(&dl, reader(true), "SELECT ?x { ?x a :F }");
    assert!(!supported.contains("example.com/a"), "{supported}");
    let (visible, _) = read(&dl, reader(false), "SELECT ?x { ?x a :F }");
    assert!(visible.contains("example.com/a"), "{visible}");
}

/// A read replica interns no term (its dictionary continues the primary's): where the
/// bounds' terms haven't come with the records yet (the data was written past the
/// pipeline, so the primary never interned them), it answers over L and says so.
#[test]
fn a_replica_interns_nothing_and_says_when_the_bounds_are_missing() {
    let dl = pipeline();
    dl.store()
        .execute_update_str(&format!("{PREFIXES} INSERT DATA {{ {UNION} }}"))
        .expect("written past the pipeline");
    dl.store().mark_replica();
    let terms = dl.store().engine_stats().dictionary.terms;
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(
        dl.store().engine_stats().dictionary.terms,
        terms,
        "the replica interned a term"
    );
    assert!(
        rows.iter().all(|r| ["w", "x", "y"].contains(&r.as_str())),
        "{rows:?}"
    );
    assert!(!status.is_complete());
    assert!(
        status.reasons().iter().any(|r| r.contains("replica")),
        "{:?}",
        status.reasons()
    );
}
