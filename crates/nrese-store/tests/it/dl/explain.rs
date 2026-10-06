//! Step 8: explanations of DL answers, as minimal sets of the ontology's axioms with the
//! triples each was read from (the black-box justification of design §10).

use std::sync::Arc;

use nrese_rdf::{NamedNode, TermRef};
use nrese_sparql::GraphAccess;
use nrese_store::{MutationError, ReadScope};

use super::{insert, pipeline};

const EX: &str = "http://example.com/";

/// w is a D through its assertion of a union; the rest is noise a justification must
/// leave out.
const DATA: &str = ":B rdfs:subClassOf :D . :C rdfs:subClassOf :D . \
     :w a [ owl:unionOf ( :B :C ) ] . \
     :Q rdfs:subClassOf :R . :x a :Q . :B rdfs:subClassOf :E . :x :knows :w .";

fn named(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

#[test]
fn a_dl_answer_is_explained_by_a_minimal_set_of_axioms() {
    let dl = pipeline();
    insert(&dl, DATA).expect("data");
    let (w, d) = (named("w"), named("D"));
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        d.as_ref().into(),
    ];
    let explained = dl
        .store()
        .explain_dl(&ReadScope::All, statement)
        .expect("w a D holds under OWL 2 DL");
    assert!(explained.minimal && explained.verified, "{explained:?}");
    let axioms: Vec<&str> = explained.axioms.iter().map(|a| a.axiom.as_str()).collect();
    assert_eq!(axioms.len(), 3, "{axioms:#?}");
    assert!(
        axioms.iter().any(|a| a.contains("ObjectUnionOf")),
        "{axioms:#?}"
    );
    for noise in ["example.com/Q", "example.com/E", "example.com/knows"] {
        assert!(
            !axioms.iter().any(|a| a.contains(noise)),
            "{noise} in {axioms:#?}"
        );
    }
    // Each axiom with the triples it was read from.
    assert!(
        explained
            .axioms
            .iter()
            .all(|a| !a.sources.is_empty() && !a.sources[0].1.is_empty())
    );
    // A statement that doesn't hold has no explanation.
    let e = named("E");
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        e.as_ref().into(),
    ];
    assert!(dl.store().explain_dl(&ReadScope::All, statement).is_none());
    // DL answers, and so their explanations, are for readers of every graph only.
    let restricted = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        ..GraphAccess::default()
    }));
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        d.as_ref().into(),
    ];
    assert!(dl.store().explain_dl(&restricted, statement).is_none());
}

#[test]
fn a_dl_rejection_names_the_axioms_with_no_model() {
    let dl = pipeline();
    insert(
        &dl,
        ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . :B owl:disjointWith :D . \
         :C owl:disjointWith :D . :Q rdfs:subClassOf :R . :x a :Q .",
    )
    .expect("schema");
    let rejected = insert(&dl, ":x a :A , :D .");
    let Err(MutationError::Rejected(reject)) = rejected else {
        panic!("expected a rejection, got {rejected:?}");
    };
    assert!(
        reject.detail.contains("have no model together"),
        "{}",
        reject.detail
    );
    let evidence = reject.explanation.expect("an explanation").evidence;
    let mentions = |local: &str| {
        evidence.iter().any(|e| {
            [&e.subject, &e.predicate, &e.object]
                .iter()
                .any(|t| t.contains(local))
        })
    };
    assert!(
        mentions("example.com/A") && mentions("example.com/D"),
        "{evidence:#?}"
    );
    assert!(!mentions("example.com/R"), "noise in {evidence:#?}");
    assert!(evidence.iter().all(|e| e.role == "axiom"));
}
