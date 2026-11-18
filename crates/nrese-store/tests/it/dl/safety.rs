//! The merge gate's "safety of the new reasoning paths" (checklist §4) for the paths the
//! other modules leave untested: a spent budget ends in a status, never a wrong answer;
//! nothing derived from a graph the requester can't read reaches it.

use std::sync::Arc;

use nrese_sparql::GraphAccess;
use nrese_store::{
    DlConfig, MutationCommand, MutationError, MutationTicket, ReadScope, Refusal, Requester,
    SparqlUpdateRequest, StoreError, WriteScope,
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
        max_branch_points: Some(0),
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
        timeout: Some(std::time::Duration::from_nanos(1)),
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
        max_branch_points: Some(0),
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
/// explanation, an undecided one (never a wrong one, nor "doesn't hold"); with the budget,
/// a minimal one.
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
        max_branch_points: Some(0),
        consistency: nrese_store::DlConsistency::Off,
        ..DlConfig::default()
    });
    insert(&spent, data).expect("data");
    let answer = statement(&spent);
    assert!(
        matches!(answer, Err(StoreError::Incomplete(_))),
        "{answer:?}"
    );
    let decided = pipeline();
    insert(&decided, data).expect("data");
    let explained = statement(&decided)
        .expect("decided")
        .expect("an explanation");
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

/// Explanations: OWL 2 DL justifications are only for readers of every graph. Another
/// reader is refused, the same for a term of a graph it can't read as for one in no graph.
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
    assert!(matches!(
        dl.store().explain_dl(&ReadScope::All, statement),
        Ok(Some(_))
    ));
    // D is only in the secret graph; Missing is in none.
    let missing = nrese_rdf::NamedNode::new_unchecked(format!("{EX}Missing"));
    let hidden = dl.store().explain_dl(&open_reader(), statement);
    let absent = dl.store().explain_dl(
        &open_reader(),
        [
            w.as_ref().into(),
            ty.as_ref().into(),
            missing.as_ref().into(),
        ],
    );
    let (Err(StoreError::Forbidden(hidden)), Err(StoreError::Forbidden(absent))) = (hidden, absent)
    else {
        panic!("refused to the open reader");
    };
    assert!(matches!(hidden, Refusal::ReadAll(_)), "{hidden:?}");
    assert_eq!(hidden, absent);
}

/// Consistency on commit (and the islands deciding it): a committer who may read and
/// write `open` only is rejected for what the secret schema makes inconsistent. Its own
/// graphs have a model by themselves, so the rejection shows no axiom (Q07: even its own
/// statements, picked from a justification with secret axioms, would tell of them).
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
    // Not the secret union or disjointness, nor which of the committer's statements they
    // take part in, nor how many axioms or statements it took.
    assert!(reject.explanation.is_none(), "{reject:?}");
    for leak in [
        "unionOf",
        "disjointWith",
        "equivalentClass",
        "example.com/x",
        "axiom",
    ] {
        assert!(!reject.detail.contains(leak), "{leak}: {}", reject.detail);
    }
    assert!(
        reject
            .detail
            .contains("Only statements in graphs you can read are shown"),
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

/// The reader's own graph `open` and what a graph it can't read, `secret`, may hold: an
/// annotation property's domain (a reason to mark L's answers under `owl2-dl`), another one
/// by another name, nothing, or an axiom the ontology reader leaves out (the reserved
/// `rdf:value` with a domain, another reason, naming its term).
const OPEN: &str = "GRAPH :open { :x a :Public . :Public rdfs:subClassOf :Known }";
const HIDDEN: [&str; 4] = [
    "GRAPH :secret { :privateNote a owl:AnnotationProperty ; rdfs:domain :PrivateClass }",
    "GRAPH :secret { :otherNote a owl:AnnotationProperty ; rdfs:domain :OtherClass }",
    "",
    "GRAPH :secret { <http://www.w3.org/1999/02/22-rdf-syntax-ns#value> rdfs:domain \
     :PrivateClass . :privateItem <http://www.w3.org/1999/02/22-rdf-syntax-ns#value> :v }",
];

/// What `reader` is told of `q`, one line per path: the run, the exact run, EXPLAIN, the
/// plan, the inferred statements alone, a read with an addition pending (and its exact
/// refusal), and the store's
/// public status of the inferred statements alone (`status_without_dl`). Each with the rows
/// and the status, or the refusal.
fn told(dl: &nrese_store::MutationPipeline, reader: &ReadScope, q: &str) -> Vec<String> {
    use nrese_store::{CancellationToken, DlAnswers, SparqlQueryRequest, StatementOp};
    let store = dl.store();
    let prepare = |q: &str, mode: Option<DlAnswers>| {
        let mut request = SparqlQueryRequest::new(format!("{PREFIXES}{q}"), reader.clone());
        request.dl_answers = mode;
        store.prepare_query(&request).expect("prepared")
    };
    let rows = |out: Vec<u8>| String::from_utf8(out).expect("utf8");
    let run = |q: &str, mode| {
        let (mut out, mut status) = (Vec::new(), None);
        let result = store.run_query_reporting(
            &prepare(q, mode),
            &CancellationToken::new(),
            &mut out,
            |s, _| {
                status = s;
                None
            },
        );
        format!("{} {status:?} {:?}", rows(out), result.err())
    };
    let pending = |mode| {
        let add = StatementOp::Add {
            data: super::versions::ntriples(
                "<http://example.com/y> <http://example.com/r> <http://example.com/w> .",
            ),
            contexts: Vec::new(),
        };
        let (mut out, mut status) = (Vec::new(), None);
        let result = store.run_query_pending_reporting(
            &nrese_store::StatementsRequest::new(vec![add]),
            &prepare(q, mode),
            &CancellationToken::new(),
            &mut out,
            |s, _| {
                status = s;
                None
            },
        );
        format!("{} {status:?} {:?}", rows(out), result.err())
    };
    let explained = store
        .explain_query(&prepare(q, None), &CancellationToken::new())
        .map(|e| e.completeness);
    let planned = store
        .plan_query(&prepare(q, None), &CancellationToken::new())
        .map(|p| p.completeness);
    let implicit = q.replacen('{', "FROM <http://www.ontotext.com/implicit> {", 1);
    let without_dl = store.status_without_dl(&prepare(&implicit, None));
    vec![
        format!("run: {}", run(q, None)),
        format!("exact: {}", run(q, Some(DlAnswers::Exact))),
        format!("explain: {explained:?}"),
        format!("plan: {planned:?}"),
        format!("inferred alone: {}", run(&implicit, None)),
        format!("pending: {}", pending(None)),
        format!("pending, exact: {}", pending(Some(DlAnswers::Exact))),
        format!("status without dl: {without_dl:?}"),
    ]
}

/// What a reader of some graphs is told doesn't depend on a graph it can't read.
/// Four stores alike in `open`: the hidden graph holds an annotation property's domain,
/// another under another name, nothing, or an axiom the reader leaves out. The reader of
/// `open` is told the same rows, the
/// same status and the same refusal on every path, whatever the hidden graph holds.
#[test]
fn a_hidden_graph_changes_nothing_a_reader_of_some_graphs_is_told() {
    let told: Vec<Vec<String>> = HIDDEN
        .iter()
        .map(|hidden| {
            let dl = pipeline();
            insert(&dl, &format!("{OPEN} {hidden}")).expect("data");
            told(&dl, &open_reader(), "SELECT ?x { ?x a :Known }")
        })
        .collect();
    for (hidden, lines) in HIDDEN.iter().zip(&told) {
        eprintln!("hidden {hidden:?}:\n  {}", lines.join("\n  "));
    }
    assert_eq!(
        told[0], told[1],
        "the hidden graph's names reach the reader"
    );
    assert_eq!(
        told[0], told[2],
        "the hidden graph's hazard reaches the reader"
    );
    assert_eq!(
        told[0], told[3],
        "the hidden graph's left-out axiom reaches the reader"
    );
}

/// The same query by a reader of every graph and by a reader of `open`, on a store whose
/// hidden graph holds the hazard and on one where it holds nothing: the first is told its
/// full status (on the first store `unsound`, with the reason naming the property), the
/// second `not-certified` on both, on every path that reports a status.
#[test]
fn a_reader_of_every_graph_is_told_the_mark_and_a_reader_of_some_not_certified() {
    let mut wrong = Vec::new();
    for hidden in [HIDDEN[0], HIDDEN[2]] {
        let dl = pipeline();
        insert(&dl, &format!("{OPEN} {hidden}")).expect("data");
        let (rows, status) = query(&dl, "SELECT ?x { ?x a :Known }");
        let reasons = status.reasons();
        let line = format!(
            "hidden {hidden:?}, every graph: {rows:?} {} {reasons:?}",
            status.shared.as_str()
        );
        eprintln!("{line}");
        let marked = status.shared.as_str() == "unsound"
            && reasons.iter().any(|r| r.contains("privateNote"));
        if rows != ["x"] || marked != !hidden.is_empty() {
            wrong.push(line);
        }
        for told in told(&dl, &open_reader(), "SELECT ?x { ?x a :Known }") {
            eprintln!("hidden {hidden:?}, open: {told}");
            if !told.contains("certification_withheld: true") {
                wrong.push(format!(
                    "not `not-certified`: hidden {hidden:?}, open: {told}"
                ));
            }
            if told.contains("privateNote") {
                wrong.push(format!("named: hidden {hidden:?}, open: {told}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
