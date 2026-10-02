//! Why an inferred statement holds (`StoreService::explain_statement`): a derivation from
//! asserted statements, each step naming its rule and premises.

use crate::support::in_memory_store_config;
use nrese_rdf::{NamedNode, Term};
use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_store::{SparqlUpdateRequest, StoreService};

const EX: &str = "http://example.com/";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

fn iri(local: &str) -> Term {
    if local.starts_with("http") {
        NamedNode::new_unchecked(local).into()
    } else {
        NamedNode::new_unchecked(format!("{EX}{local}")).into()
    }
}

#[test]
fn inferences_are_explained_by_their_derivations() {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             PREFIX owl: <http://www.w3.org/2002/07/owl#>
             INSERT DATA {{ ex:a a ex:C . ex:C rdfs:subClassOf ex:D . ex:D rdfs:subClassOf ex:E .
                            ex:ancestor a owl:TransitiveProperty .
                            ex:x ex:ancestor ex:y . ex:y ex:ancestor ex:z . ex:z ex:ancestor ex:w }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    let explain = |s: &str, p: &str, o: &str| {
        store.explain_statement(
            Ruleset::Owl2Rl,
            &nrese_store::ReadScope::All,
            iri(s).as_ref(),
            iri(p).as_ref(),
            iri(o).as_ref(),
        )
    };

    // Two subclass steps down to the asserted statements: `a` is a D, and D is a subclass
    // of E; or `a` is a C, and C a subclass of E (both proofs are as shallow and as
    // small).
    let steps = explain("a", TYPE, "E").expect("a is an E");
    assert_eq!(steps[0].object, format!("{EX}E"));
    assert_eq!(steps[0].origin, "inferred");
    assert_eq!(steps[0].rule.as_deref(), Some("cax-sco"));
    let mut premises: Vec<String> = steps[0]
        .premises
        .iter()
        .map(|&i| format!("{} {}", steps[i].object, steps[i].origin))
        .collect();
    premises.sort();
    let through_d = [format!("{EX}D inferred"), format!("{EX}E asserted")];
    let through_c = [format!("{EX}C asserted"), format!("{EX}E inferred")];
    assert!(
        premises == through_d || premises == through_c,
        "{premises:?}"
    );
    let rules: Vec<&str> = steps.iter().filter_map(|s| s.rule.as_deref()).collect();
    assert_eq!(rules.len(), 2, "{steps:?}");
    assert!(
        rules
            .iter()
            .all(|r| matches!(*r, "cax-sco" | "scm-sco" | "prp-trp")),
        "{rules:?}"
    );
    // The same explanation every time.
    assert_eq!(explain("a", TYPE, "E").unwrap(), steps);
    // Every inferred step has premises; every asserted one has none; no step is its own
    // premise, and the premises come later (the proof is well-founded).
    for (i, step) in steps.iter().enumerate() {
        assert_eq!(
            step.origin == "inferred",
            !step.premises.is_empty(),
            "{step:?}"
        );
        assert!(step.premises.iter().all(|&p| p > i), "{step:?}");
    }

    // A transitive chain of three.
    let steps = explain("x", "ancestor", "w").expect("x is an ancestor of w");
    assert_eq!(steps[0].rule.as_deref(), Some("prp-trp"));
    assert_eq!(
        steps.iter().filter(|s| s.origin == "asserted").count(),
        4,
        "three links and the axiom: {steps:#?}"
    );

    // An asserted statement explains itself; one that doesn't hold, not at all.
    let steps = explain("a", TYPE, "C").unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].origin, "asserted");
    assert!(explain("a", TYPE, "x").is_none());
    assert!(explain("w", "ancestor", "x").is_none());
}

/// An explanation shows only what the requester may read (graph access): asserted
/// premises in no readable graph are `hidden` steps; a statement the requester doesn't
/// see is answered as one that doesn't hold, so `explain` tells nothing about hidden
/// statements' existence.
#[test]
fn explanations_stay_within_the_readers_graphs() {
    use std::sync::Arc;
    let store = StoreService::new(in_memory_store_config()).unwrap();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ GRAPH ex:open {{ ex:a a ex:C }}
                            GRAPH ex:secret {{ ex:C rdfs:subClassOf ex:D . ex:s ex:p ex:o }} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    let scope = |inferred: bool| {
        nrese_store::ReadScope::Graphs(Arc::new(nrese_sparql::GraphAccess {
            graphs: vec![format!("{EX}open")],
            inferred,
            ..nrese_sparql::GraphAccess::default()
        }))
    };
    let explain = |scope: &nrese_store::ReadScope, s: &str, p: &str, o: &str| {
        store.explain_statement(
            Ruleset::Owl2Rl,
            scope,
            iri(s).as_ref(),
            iri(p).as_ref(),
            iri(o).as_ref(),
        )
    };
    // `a` is a D: the open premise shown, the secret one hidden.
    let steps =
        explain(&scope(true), "a", TYPE, "D").expect("a is a D, and inferences are visible");
    let origins: Vec<&str> = steps.iter().map(|s| s.origin).collect();
    assert_eq!(origins[0], "inferred");
    assert!(
        origins.contains(&"asserted") && origins.contains(&"hidden"),
        "{origins:?}"
    );
    let hidden = steps.iter().find(|s| s.origin == "hidden").unwrap();
    assert!(hidden.subject.is_empty() && hidden.object.is_empty());
    // Inferences not visible: as if it didn't hold.
    assert!(explain(&scope(false), "a", TYPE, "D").is_none());
    // A statement asserted only in the secret graph: as if it didn't hold.
    assert!(explain(&scope(true), "s", "p", "o").is_none());
    // Unrestricted, everything is shown.
    let all = explain(&nrese_store::ReadScope::All, "a", TYPE, "D").unwrap();
    assert!(all.iter().all(|s| s.origin != "hidden"));
}
