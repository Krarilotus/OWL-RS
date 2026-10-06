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
