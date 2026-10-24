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

/// Spec: selected v1.5 operation contract, 23 October: B <= D, C <= D,
/// w : (B or C) entails w : D by cases; a spent *initial* explanation test
/// is unknown, whereas an unspent countermodel establishes non-entailment.
#[test]
fn an_initial_dl_stop_is_unknown_through_the_store_operation() {
    use nrese_store::dl::explain::DlExplanationOutcome;
    for (tests, expected) in [(Some(0), "unknown"), (None, "entailed")] {
        let dl = super::pipeline_with(nrese_store::DlConfig {
            explanation_tests: tests,
            ..nrese_store::DlConfig::default()
        });
        insert(&dl, DATA).expect("consistent hand-worked union ontology");
        let (w, d) = (named("w"), named("D"));
        let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let outcome = dl
            .store()
            .explain_dl_outcome(
                &ReadScope::All,
                [
                    w.as_ref().into(),
                    rdf_type.as_ref().into(),
                    d.as_ref().into(),
                ],
            )
            .unwrap();
        match (expected, outcome) {
            ("unknown", DlExplanationOutcome::Unknown(why)) => {
                assert!(why.contains("explanation_tests"), "{why}")
            }
            ("entailed", DlExplanationOutcome::Explained(proof)) => {
                assert!(proof.minimal && proof.verified);
                assert_eq!(proof.axioms.len(), 3);
            }
            (_, other) => panic!("{expected}, got {other:?}"),
        }
        if tests.is_none() {
            // Model: choose w in C and outside E; the B <= E axiom does not apply.
            let e = named("E");
            assert!(matches!(
                dl.store()
                    .explain_dl_outcome(
                        &ReadScope::All,
                        [
                            w.as_ref().into(),
                            rdf_type.as_ref().into(),
                            e.as_ref().into(),
                        ]
                    )
                    .unwrap(),
                DlExplanationOutcome::DoesNotHold
            ));

            // Spec: the selected truthful-outcome contract requires an actual
            // countermodel before DoesNotHold. A restricted input and a missing term
            // are incapable inputs, not negative entailment results. A fresh subject
            // is nevertheless an owl:Thing in OWL semantics (independent tautology).
            // RED against the former outcome branches / GREEN: UNRUN.
            let restricted = ReadScope::Graphs(Arc::new(GraphAccess {
                default_graph: true,
                inferred: true,
                ..GraphAccess::default()
            }));
            let fresh = named("not-in-the-dictionary");
            let thing = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#Thing");
            let mut reasons = Vec::new();
            for (scope, subject, object) in
                [(&restricted, &w, &d), (&ReadScope::All, &fresh, &thing)]
            {
                let statement = [
                    subject.as_ref().into(),
                    rdf_type.as_ref().into(),
                    object.as_ref().into(),
                ];
                match dl.store().explain_dl_outcome(scope, statement).unwrap() {
                    DlExplanationOutcome::Unknown(why) => reasons.push(why),
                    other => panic!("no negative entailment test ran: {other:?}"),
                }
                assert!(
                    dl.store().explain_dl(scope, statement).is_none(),
                    "legacy optional wrapper stays compatible"
                );
            }
            assert_eq!(
                reasons[0], reasons[1],
                "incapable inputs use one generic reason"
            );
            assert!(!reasons[0].contains(EX), "no term or graph disclosure");
        }
    }
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
