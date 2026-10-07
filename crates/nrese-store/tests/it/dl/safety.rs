//! The merge gate's "safety of the new reasoning paths" (checklist §4) for the paths the
//! other modules leave untested: a spent budget ends in a status, never a wrong answer;
//! nothing derived from a graph the requester can't read reaches it.

use std::sync::Arc;

use nrese_sparql::GraphAccess;
use nrese_store::{
    DlConfig, MutationCommand, MutationError, MutationTicket, ReadScope, Requester,
    SparqlUpdateRequest, WriteScope,
};

use super::queries::query;
use super::{PREFIXES, insert, pipeline, pipeline_with};

const EX: &str = "http://example.com/";

/// A union U1 splits into both disjoint classes: its clash leaves consistency to the
/// hypertableau, which must branch.
const SPLIT_CLASH: &str = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :C . :x a :A .";

/// Consistency on commit: the hypertableau with no branch point to spend accepts the
/// commit and says the data's consistency is unknown, never consistent or inconsistent.
#[test]
fn a_spent_consistency_budget_leaves_the_status_unknown() {
    let dl = pipeline_with(DlConfig {
        max_branch_points: 0,
        ..DlConfig::default()
    });
    insert(&dl, SPLIT_CLASH).expect("an undecided check accepts the commit");
    let status = dl.store().dl().status().expect("status");
    assert_eq!(status.consistency.verdict.as_str(), "unknown", "{status:?}");
    // And no answer claims completeness on data whose consistency isn't known.
    let (_, answered) = query(&dl, "SELECT ?x { ?x a :B }");
    assert!(!answered.is_complete(), "{:?}", answered.reasons());
}

/// L/U1 and U1 with representatives: U1's evaluation stopped by its budget leaves it
/// unavailable, and the answers say they are sound only. The ontology equates terms in U1
/// (a functional property over an existential), so the stopped evaluation is the one by
/// representatives.
#[test]
fn a_spent_upper_bound_budget_leaves_the_answers_sound_only() {
    let dl = pipeline_with(DlConfig {
        timeout: std::time::Duration::from_nanos(1),
        ..DlConfig::default()
    });
    insert(
        &dl,
        ":Student rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :enrollIn ; \
           owl:someValuesFrom :Dept ] . :enrollIn a owl:FunctionalProperty . \
         :s1 a :Student ; :enrollIn :d1 .",
    )
    .expect("data");
    let bounds = dl.store().dl_bounds();
    assert!(bounds.unavailable.is_some(), "{bounds:?}");
    let (_, status) = query(&dl, "SELECT ?x { ?x a :Dept }");
    assert!(!status.is_complete(), "{:?}", status.reasons());
}

/// Classification and realisation: tests the hypertableau can't branch for leave the
/// reports incomplete, with the reason.
#[test]
fn a_spent_classification_budget_leaves_the_reports_incomplete() {
    let dl = pipeline_with(DlConfig {
        max_branch_points: 0,
        consistency: nrese_store::DlConsistency::Off,
        ..DlConfig::default()
    });
    insert(
        &dl,
        ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
         :C rdfs:subClassOf :D . :x a :A .",
    )
    .expect("data");
    let classified = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    assert!(!classified.complete(), "{classified:?}");
    let realised = dl.store().realise(&ReadScope::All).expect("realisation");
    assert!(!realised.complete(), "{realised:?}");
}

/// Explanations: a justification the engines can't decide within the budget is no
/// explanation (never a wrong one); with the budget, a minimal one.
#[test]
fn a_spent_explanation_budget_gives_no_explanation() {
    let data = ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
         :C rdfs:subClassOf :D . :w a :A .";
    let statement = |dl: &nrese_store::MutationPipeline| {
        let (w, ty, d) = (
            nrese_rdf::NamedNode::new_unchecked(format!("{EX}w")),
            nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            nrese_rdf::NamedNode::new_unchecked(format!("{EX}D")),
        );
        dl.store().explain_dl(
            &ReadScope::All,
            [w.as_ref().into(), ty.as_ref().into(), d.as_ref().into()],
        )
    };
    let spent = pipeline_with(DlConfig {
        max_branch_points: 0,
        consistency: nrese_store::DlConsistency::Off,
        ..DlConfig::default()
    });
    insert(&spent, data).expect("data");
    assert!(statement(&spent).is_none());
    let decided = pipeline();
    insert(&decided, data).expect("data");
    let explained = statement(&decided).expect("an explanation");
    assert!(explained.minimal, "{explained:?}");
}

/// A reader of the default graph and `open` only.
fn open_reader() -> ReadScope {
    ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }))
}

/// Explanations: OWL 2 DL justifications are only for readers of every graph.
#[test]
fn explanations_are_only_for_readers_of_every_graph() {
    let dl = pipeline();
    insert(
        &dl,
        "GRAPH :secret { :A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
           :B rdfs:subClassOf :D . :C rdfs:subClassOf :D } GRAPH :open { :w a :A }",
    )
    .expect("data");
    let (w, ty, d) = (
        nrese_rdf::NamedNode::new_unchecked(format!("{EX}w")),
        nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        nrese_rdf::NamedNode::new_unchecked(format!("{EX}D")),
    );
    let statement = [w.as_ref().into(), ty.as_ref().into(), d.as_ref().into()];
    assert!(dl.store().explain_dl(&ReadScope::All, statement).is_some());
    assert!(dl.store().explain_dl(&open_reader(), statement).is_none());
}

/// Consistency on commit (and the islands deciding it): a committer who may read and
/// write `open` only is rejected for what the secret schema makes inconsistent, and the
/// rejection shows the axioms of the graphs it may read, never the secret ones.
#[test]
fn a_rejection_shows_no_axiom_of_a_graph_the_committer_cannot_read() {
    let dl = pipeline();
    insert(
        &dl,
        "GRAPH :secret { :A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
           :B owl:disjointWith :D . :C owl:disjointWith :D }",
    )
    .expect("schema");
    let access = Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open")],
        ..GraphAccess::default()
    });
    let committer = Requester::new(
        ReadScope::Graphs(Arc::clone(&access)),
        WriteScope::Graphs(access),
    );
    let rejected = dl.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}INSERT DATA {{ GRAPH :open {{ :x a :A , :D }} }}"
        ))),
        &committer,
        &MutationTicket::new(),
    );
    let Err(MutationError::Rejected(reject)) = rejected else {
        panic!("expected a rejection, got {rejected:?}");
    };
    let evidence = reject.explanation.expect("an explanation").evidence;
    let shown = |local: &str| {
        evidence.iter().any(|e| {
            [&e.subject, &e.predicate, &e.object]
                .iter()
                .any(|t| t.contains(local))
        })
    };
    // The committer's own statements, not the secret union or disjointness.
    assert!(shown("example.com/x"), "{evidence:#?}");
    assert!(
        !evidence.iter().any(|e| e.predicate.contains("unionOf")
            || e.predicate.contains("disjointWith")
            || e.predicate.contains("equivalentClass")),
        "{evidence:#?}"
    );
    assert!(
        reject.detail.contains("graphs you can't read"),
        "{}",
        reject.detail
    );
}

/// U1 with representatives under graph access: what U1's classes give (both departments
/// through the secret schema) never reaches a reader of `open` only.
#[test]
fn an_upper_bound_with_classes_derives_nothing_from_graphs_the_reader_cannot_read() {
    let dl = pipeline();
    insert(
        &dl,
        "GRAPH :secret { :Student rdfs:subClassOf [ a owl:Restriction ; \
           owl:onProperty :enrollIn ; owl:someValuesFrom :Dept ] . \
           :enrollIn a owl:FunctionalProperty } \
         GRAPH :open { :s1 a :Student ; :enrollIn :d1 . :s2 a :Student ; :enrollIn :d2 }",
    )
    .expect("data");
    let bounds = dl.store().dl_bounds();
    assert!(bounds.equality_classes > 0, "U1 has classes: {bounds:?}");
    let all = query(&dl, "SELECT ?x { ?x a :Dept }");
    assert_eq!(all.0, ["d1", "d2"]);
    let store = dl.store();
    let prepared = store
        .prepare_query(&nrese_store::SparqlQueryRequest::new(
            format!("{PREFIXES}SELECT ?x {{ ?x a :Dept }}"),
            open_reader(),
        ))
        .expect("prepared");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out)
        .expect("query")
        .expect("a status");
    let text = String::from_utf8(out).expect("utf8");
    assert!(!text.contains("example.com/d"), "{text}");
    assert!(!status.is_complete(), "{:?}", status.reasons());
}
