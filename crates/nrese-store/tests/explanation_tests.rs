//! Why an inferred statement holds (`StoreService::explain_statement`): a derivation from
//! asserted statements, each step naming its rule and premises.

mod support;

use nrese_rdf::{NamedNode, Term};
use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_store::{SparqlUpdateRequest, StoreService};
use support::in_memory_store_config;

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
            iri(s).as_ref(),
            iri(p).as_ref(),
            iri(o).as_ref(),
        )
    };

    // Two subclass steps down to the asserted statements.
    let steps = explain("a", TYPE, "E").expect("a is an E");
    assert_eq!(steps[0].object, format!("{EX}E"));
    assert_eq!(steps[0].origin, "inferred");
    assert_eq!(steps[0].rule.as_deref(), Some("cax-sco"));
    let premises: Vec<String> = steps[0]
        .premises
        .iter()
        .map(|&i| format!("{} {}", steps[i].object, steps[i].origin))
        .collect();
    assert!(premises.contains(&format!("{EX}D inferred")), "{premises:?}");
    assert!(premises.contains(&format!("{EX}E asserted")), "{premises:?}");
    let d = steps
        .iter()
        .position(|s| s.object == format!("{EX}D") && s.predicate == TYPE)
        .unwrap();
    assert_eq!(steps[d].rule.as_deref(), Some("cax-sco"));
    // Every inferred step has premises; every asserted one has none; no step is its own
    // premise, and the premises come later (the proof is well-founded).
    for (i, step) in steps.iter().enumerate() {
        assert_eq!(step.origin == "inferred", !step.premises.is_empty(), "{step:?}");
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
