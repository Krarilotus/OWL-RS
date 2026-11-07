//! Step 9: routing by profile. The per-axiom profile checker (`nrese_owl::profile`, held
//! against the W3C test cases' annotations in nrese-owl) decides the route: an OWL 2 RL
//! ontology goes to the RL rules alone (complete for it, theorem PR1: no U1, no DL
//! engine), Horn and EL ontologies to the context core, the rest to the hypertableau.
//! Each test names the route that must be taken (the performance choice's guard).

use super::queries::query;
use super::{insert, pipeline};

#[test]
fn an_rl_ontology_is_decided_by_the_rules_alone() {
    let dl = pipeline();
    insert(
        &dl,
        ":Dog rdfs:subClassOf :Animal . :Animal owl:disjointWith :Plant . \
         :owns rdfs:range :Pet . :ann :owns :rex . :rex a :Dog .",
    )
    .expect("an RL ontology");
    let bounds = dl.store().dl_bounds();
    assert_eq!(bounds.last, "rules");
    assert_eq!(bounds.upper_facts, 0, "no U1 compiled");
    let status = dl.store().dl().status().expect("status");
    assert_eq!(status.consistency.engine, "rules");
    assert_eq!(status.consistency.verdict.as_str(), "consistent");
    // Every predicate is closed: one evaluation, complete, negation included.
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :Animal }");
    assert_eq!(rows, ["rex"]);
    assert!(status.is_complete());
    assert_eq!(status.paths, ["closed-predicates"]);
    let (rows, status) = query(
        &dl,
        "SELECT ?x { ?x a :Pet FILTER NOT EXISTS { ?x a :Plant } }",
    );
    assert_eq!(rows, ["rex"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    // An assertion keeps the route; an RL violation is still rejected by the RL gate.
    insert(&dl, ":tom a :Dog .").expect("an assertion");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    assert!(insert(&dl, ":tom a :Plant .").is_err());
}

/// OWL 2 RL's eq-ref (OWL 2 Profiles §4.3, table 4) makes every term `owl:sameAs` itself;
/// the RL rules leave it unmaterialised by design (RSH-04), so a read that can bind both
/// ends of `owl:sameAs` to one term misses its reflexive answers on the RL route: never
/// labelled complete, and refused under `exact`. Two distinct constants are decided.
#[test]
fn reflexive_same_as_reads_are_not_complete_on_the_rules_route() {
    use nrese_store::{DlAnswers, StoreError};
    let dl = pipeline();
    insert(
        &dl,
        ":Dog rdfs:subClassOf :Animal . :rex a :Dog . :ann :owns :rex .",
    )
    .expect("an RL ontology");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    // Guard (holds on the base too): two distinct constants are decided.
    let (rows, status) = query(&dl, "ASK { :rex owl:sameAs :ann }");
    assert_eq!(rows, ["false"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    // Every read is checked and the wrong ones reported together.
    let mut wrong = Vec::new();
    for read in [
        "ASK { :rex owl:sameAs :rex }",
        "SELECT ?x { ?x owl:sameAs :rex }",
        "SELECT ?x { :ann :owns/owl:sameAs ?x }",
    ] {
        let (_, status) = query(&dl, read);
        if status.is_complete() {
            wrong.push(format!("{read}: complete"));
        }
        let exact = super::queries::query_with(&dl, read, Some(DlAnswers::Exact));
        if !matches!(exact, Err(StoreError::Incomplete(_))) {
            wrong.push(format!("{read}: exact gave {exact:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Under negation a missing reflexive pair keeps a row the certain answers lack: `?x
/// owl:sameAs ?x` holds of `:rex` (eq-ref, OWL 2 Profiles table 4), so NOT EXISTS removes
/// it (SPARQL 1.1 §17.4.1.4) and the certain answers are none. Answered over the lower
/// bound, `:rex` is returned: never labelled sound, in the plan or any answer mode, and
/// refused under `exact` (P2-06 review r1, F1). The label holds whatever decides first: a
/// schema read beside it (review r2), or a negated property set, which reads `owl:sameAs`
/// unless it excludes it.
#[test]
fn a_reflexive_same_as_read_under_negation_is_not_sound() {
    use nrese_store::{CancellationToken, DlAnswers, SparqlQueryRequest, StoreError};
    let dl = pipeline();
    insert(&dl, ":Dog rdfs:subClassOf :Animal . :rex a :Dog .").expect("an RL ontology");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    let mut wrong = Vec::new();
    // Guards (hold on the base too): a monotone read misses answers only, so it stays sound.
    for monotone in [
        "SELECT ?x { ?x owl:sameAs :rex }",
        "SELECT ?x { ?x !:owns :rex }",
        "SELECT ?x { :Dog rdfs:subClassOf :Animal . ?x owl:sameAs :rex }",
    ] {
        let (rows, status) = query(&dl, monotone);
        if !rows.is_empty() || !status.shared.sound || status.is_complete() {
            wrong.push(format!("guard {monotone}: {rows:?} {:?}", status.shared));
        }
    }
    for negated in [
        "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x owl:sameAs ?x } }",
        "SELECT ?x { :Dog rdfs:subClassOf :Animal . ?x a :Dog \
         FILTER NOT EXISTS { ?x owl:sameAs ?x } }",
        "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x !:owns ?x } }",
    ] {
        for mode in [
            None,
            Some(DlAnswers::CertainWhereComplete),
            Some(DlAnswers::Sound),
        ] {
            let (rows, status) = super::queries::query_with(&dl, negated, mode).expect("answered");
            // The lower bound's row, which the certain answers lack: answered, not certified.
            if rows != ["rex"] {
                wrong.push(format!("{negated} {mode:?}: rows {rows:?}"));
            }
            if status.shared.sound || status.is_complete() {
                wrong.push(format!("{negated} {mode:?}: {:?}", status.shared));
            }
        }
        let store = dl.store();
        let prepared = store
            .prepare_query(&SparqlQueryRequest::all(format!(
                "{}{negated}",
                super::PREFIXES
            )))
            .expect("prepared");
        let planned = store
            .plan_query(&prepared, &CancellationToken::new())
            .expect("plan")
            .completeness
            .expect("a status");
        if planned.sound || planned.complete {
            wrong.push(format!("{negated} plan: {planned:?}"));
        }
        let exact = super::queries::query_with(&dl, negated, Some(DlAnswers::Exact));
        if !matches!(exact, Err(StoreError::Incomplete(_))) {
            wrong.push(format!("{negated} exact: {exact:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn an_axiom_outside_rl_takes_the_ontology_off_the_rules_route() {
    let dl = pipeline();
    insert(&dl, ":Dog rdfs:subClassOf :Animal . :rex a :Dog .").expect("RL");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    // A union on the right of a subclass axiom is outside RL: U1 and the DL engines.
    insert(
        &dl,
        ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :Cat rdfs:subClassOf :Animal . \
         :tib a :Pet .",
    )
    .expect("not RL");
    assert_eq!(dl.store().dl_bounds().last, "rebuilt");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :Animal }");
    assert_eq!(rows, ["rex", "tib"], "tib through the union");
    assert!(status.is_complete(), "{:?}", status.reasons());
    // Back inside RL: the rules again.
    super::update(
        &dl,
        "DELETE WHERE { :Pet rdfs:subClassOf ?u . ?u owl:unionOf ?l . ?l ?p ?o }",
    )
    .expect("the union removed");
    super::update(
        &dl,
        "DELETE WHERE { ?l <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ?f ; \
           <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> ?r }",
    )
    .expect("its list removed");
    assert_eq!(dl.store().dl_bounds().last, "rules");
}
